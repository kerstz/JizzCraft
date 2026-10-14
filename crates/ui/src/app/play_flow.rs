//! The Play flow as a pure state machine (feature `network`).
//!
//! typed input -> list files (the window starts it with `begin_listing`) -> (pick) -> add torrent -> open stream -> player.
//! Nothing here touches the network or the UI: a [`FlowBackend`] starts each
//! step and hands back a channel, and [`PlayFlow::step`] polls it with
//! `try_recv`. Errors are mapped to fixed sentences by `classify`; no name,
//! address or path ever ends up in an event.

use jizzcraft_fetch::errors::{NO_VIDEO, classify};
use jizzcraft_fetch::pick::{Pick, pick_video};
use std::sync::mpsc::{Receiver, TryRecvError};

/// What `choices` hands the window: file names and lengths, then the picks.
pub(super) type Choices<'a> = (&'a [(String, u64)], &'a [usize]);

pub(super) type Rx<T> = Receiver<Result<T, String>>;

/// A listed torrent: only its file names and lengths are visible.
pub(super) trait FileList {
    fn files(&self) -> &[(String, u64)];
}

/// Where each step really happens. `Listed` and `Ticket` are associated so
/// tests can use plain values.
pub(super) trait FlowBackend {
    type Listed: FileList;
    type Ticket;
    /// Files of a torrent that is already added.
    fn files(&mut self, id: usize) -> Rx<Vec<(String, u64)>>;
    fn add(&mut self, l: Self::Listed, file: usize) -> Rx<usize>;
    fn open(&mut self, id: usize, file: usize) -> Rx<Self::Ticket>;
    fn cancel(&mut self);
}

pub(super) enum FlowState<L, T> {
    Idle,
    Listing(Rx<L>),
    ListingFiles {
        id: usize,
        rx: Rx<Vec<(String, u64)>>,
    },
    Choosing {
        listed: Option<L>,
        id: Option<usize>,
        files: Vec<(String, u64)>,
        picks: Vec<usize>,
    },
    Adding {
        rx: Rx<usize>,
        file: usize,
    },
    Opening(Rx<T>),
    Ready(T),
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FlowEvent<T> {
    /// Nothing is running.
    Idle,
    /// Still working; the key goes through `i18n::tr`.
    Progress(&'static str),
    /// The user has to pick a file.
    Choose,
    /// The stream is ready to open.
    Open(T),
    /// The flow stopped; a fixed sentence.
    Error(String),
}

pub(super) const P_LISTING: &str = "Looking for the files…";
pub(super) const P_ADDING: &str = "Adding it to your downloads…";
pub(super) const P_OPENING: &str = "Starting playback…";

/// "Looking for peers (n)" while a magnet resolves; `None` when there is no
/// live probe (a .torrent file or an address) or it already finished.
pub(super) fn probe_line(resolving_peers: Option<u32>) -> Option<String> {
    resolving_peers.map(|n| format!("Looking for peers ({n})"))
}

/// Failures the user can try again (the swarm may simply be quiet).
pub(super) fn is_retryable(sentence: &str) -> bool {
    sentence == jizzcraft_fetch::errors::NO_PEERS
        || sentence == jizzcraft_fetch::errors::METADATA_TIMEOUT
}

/// A banner at the top of the Downloads window.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Banner {
    /// `fetch.json` was unreadable and was set aside.
    ConfigUnreadable,
    /// "Resume N paused downloads?"
    ResumeAsk(usize),
}

/// Which banners to show. Pure: the window only draws the result.
pub(super) fn banners(
    load_error: bool,
    load_error_dismissed: bool,
    resume: jizzcraft_fetch::config::ResumeOnStart,
    paused_unfinished: usize,
    ask_dismissed: bool,
) -> Vec<Banner> {
    use jizzcraft_fetch::config::ResumeOnStart;
    let mut v = Vec::new();
    if load_error && !load_error_dismissed {
        v.push(Banner::ConfigUnreadable);
    }
    if resume == ResumeOnStart::Ask && paused_unfinished > 0 && !ask_dismissed {
        v.push(Banner::ResumeAsk(paused_unfinished));
    }
    v
}

pub(super) struct PlayFlow<L, T> {
    pub state: FlowState<L, T>,
}

impl<L, T> Default for PlayFlow<L, T> {
    fn default() -> Self {
        Self {
            state: FlowState::Idle,
        }
    }
}

impl<L: FileList, T> PlayFlow<L, T> {
    /// Something is in flight or waiting for the user.
    pub fn busy(&self) -> bool {
        !matches!(self.state, FlowState::Idle | FlowState::Failed(_))
    }

    /// Press Play on a torrent that is already added.
    pub fn start_files<B: FlowBackend<Listed = L, Ticket = T>>(&mut self, id: usize, be: &mut B) {
        self.state = FlowState::ListingFiles {
            id,
            rx: be.files(id),
        };
    }

    pub fn begin_listing(&mut self, rx: Rx<L>) {
        self.state = FlowState::Listing(rx);
    }

    #[cfg(test)]
    pub fn begin_files(&mut self, id: usize, rx: Rx<Vec<(String, u64)>>) {
        self.state = FlowState::ListingFiles { id, rx };
    }

    pub fn begin_opening(&mut self, rx: Rx<T>) {
        self.state = FlowState::Opening(rx);
    }

    /// Cancel from any state (button or boss key). Dropping the receivers
    /// makes the workers' sends fail; the backend stops the requests.
    pub fn cancel<B: FlowBackend<Listed = L, Ticket = T>>(&mut self, be: &mut B) {
        be.cancel();
        self.state = FlowState::Idle;
    }

    /// One-line status key while a step is running (not while choosing).
    pub fn progress_key(&self) -> Option<&'static str> {
        match self.state {
            FlowState::Listing(_) | FlowState::ListingFiles { .. } => Some(P_LISTING),
            FlowState::Adding { .. } => Some(P_ADDING),
            FlowState::Opening(_) | FlowState::Ready(_) => Some(P_OPENING),
            _ => None,
        }
    }

    /// The files being asked about, while waiting for the user.
    pub fn choices(&self) -> Option<Choices<'_>> {
        match &self.state {
            FlowState::Choosing { files, picks, .. } => Some((files, picks)),
            _ => None,
        }
    }

    /// The user picked file `i` of the current choice.
    pub fn choose<B: FlowBackend<Listed = L, Ticket = T>>(&mut self, i: usize, be: &mut B) {
        match std::mem::replace(&mut self.state, FlowState::Idle) {
            FlowState::Choosing {
                listed: Some(l), ..
            } => {
                let rx = be.add(l, i);
                self.state = FlowState::Adding { rx, file: i };
            }
            FlowState::Choosing { id: Some(id), .. } => {
                self.state = FlowState::Opening(be.open(id, i));
            }
            other => self.state = other,
        }
    }

    fn after_pick<B: FlowBackend<Listed = L, Ticket = T>>(
        &mut self,
        be: &mut B,
        pick: Pick,
        listed: Option<L>,
        id: Option<usize>,
        files: Vec<(String, u64)>,
    ) {
        self.state = match (pick, listed, id) {
            (Pick::None, ..) => FlowState::Failed(NO_VIDEO.to_owned()),
            (Pick::One(i), Some(l), _) => FlowState::Adding {
                rx: be.add(l, i),
                file: i,
            },
            (Pick::One(i), None, Some(id)) => FlowState::Opening(be.open(id, i)),
            (Pick::Ask(picks), listed, id) => FlowState::Choosing {
                listed,
                id,
                files,
                picks,
            },
            _ => FlowState::Failed(classify("error").to_owned()),
        };
    }

    /// Advance at most one transition. Call every frame.
    pub fn step<B: FlowBackend<Listed = L, Ticket = T>>(&mut self, be: &mut B) -> FlowEvent<T> {
        match std::mem::replace(&mut self.state, FlowState::Idle) {
            FlowState::Idle => FlowEvent::Idle,
            FlowState::Failed(m) => FlowEvent::Error(m),
            FlowState::Ready(t) => FlowEvent::Open(t),
            FlowState::Listing(rx) => match rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.state = FlowState::Listing(rx);
                    FlowEvent::Progress(P_LISTING)
                }
                Err(TryRecvError::Disconnected) => self.fail("error"),
                Ok(Err(e)) => self.fail(&e),
                Ok(Ok(l)) => {
                    let files = l.files().to_vec();
                    let pick = pick_video(&files);
                    self.after_pick(be, pick, Some(l), None, files);
                    self.event_now()
                }
            },
            FlowState::ListingFiles { id, rx } => match rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.state = FlowState::ListingFiles { id, rx };
                    FlowEvent::Progress(P_LISTING)
                }
                Err(TryRecvError::Disconnected) => self.fail("error"),
                Ok(Err(e)) => self.fail(&e),
                Ok(Ok(files)) => {
                    let pick = pick_video(&files);
                    self.after_pick(be, pick, None, Some(id), files);
                    self.event_now()
                }
            },
            FlowState::Adding { rx, file } => match rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.state = FlowState::Adding { rx, file };
                    FlowEvent::Progress(P_ADDING)
                }
                Err(TryRecvError::Disconnected) => self.fail("error"),
                Ok(Err(e)) => self.fail(&e),
                Ok(Ok(id)) => {
                    self.state = FlowState::Opening(be.open(id, file));
                    FlowEvent::Progress(P_OPENING)
                }
            },
            FlowState::Opening(rx) => match rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.state = FlowState::Opening(rx);
                    FlowEvent::Progress(P_OPENING)
                }
                Err(TryRecvError::Disconnected) => self.fail("error"),
                Ok(Err(e)) => self.fail(&e),
                Ok(Ok(t)) => {
                    self.state = FlowState::Ready(t);
                    self.step(be)
                }
            },
            c @ FlowState::Choosing { .. } => {
                self.state = c;
                FlowEvent::Choose
            }
        }
    }

    fn fail(&mut self, raw: &str) -> FlowEvent<T> {
        self.state = FlowState::Idle;
        FlowEvent::Error(classify(raw).to_owned())
    }

    /// The event for the state just entered by `after_pick`.
    fn event_now(&mut self) -> FlowEvent<T> {
        match &self.state {
            FlowState::Failed(_) => {
                let FlowState::Failed(m) = std::mem::replace(&mut self.state, FlowState::Idle)
                else {
                    unreachable!()
                };
                FlowEvent::Error(m)
            }
            FlowState::Choosing { .. } => FlowEvent::Choose,
            _ => FlowEvent::Progress(P_ADDING),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Sender, channel};

    struct L(Vec<(String, u64)>);
    impl FileList for L {
        fn files(&self) -> &[(String, u64)] {
            &self.0
        }
    }

    #[derive(Default)]
    struct Fake {
        add_tx: Vec<Sender<Result<usize, String>>>,
        open_tx: Vec<Sender<Result<u32, String>>>,
        cancelled: u32,
        adds: Vec<usize>,
        opens: Vec<(usize, usize)>,
    }
    impl FlowBackend for Fake {
        type Listed = L;
        type Ticket = u32;
        fn files(&mut self, _: usize) -> Rx<Vec<(String, u64)>> {
            channel().1
        }
        fn add(&mut self, _: L, file: usize) -> Rx<usize> {
            let (tx, rx) = channel();
            self.adds.push(file);
            self.add_tx.push(tx);
            rx
        }
        fn open(&mut self, id: usize, file: usize) -> Rx<u32> {
            let (tx, rx) = channel();
            self.opens.push((id, file));
            self.open_tx.push(tx);
            rx
        }
        fn cancel(&mut self) {
            self.cancelled += 1;
        }
    }

    type Flow = PlayFlow<L, u32>;

    fn one() -> Vec<(String, u64)> {
        vec![("a.mp4".into(), 9_000_000)]
    }
    fn two() -> Vec<(String, u64)> {
        vec![("a.mp4".into(), 9_000_000), ("b.mp4".into(), 9_000_000)]
    }
    fn listing(files: Vec<(String, u64)>) -> (Flow, std::sync::mpsc::Sender<Result<L, String>>) {
        let (tx, rx) = channel();
        let mut f = Flow::default();
        f.begin_listing(rx);
        let _ = tx.send(Ok(L(files)));
        (f, tx)
    }

    #[test]
    fn idle_stays_idle() {
        let mut f = Flow::default();
        assert_eq!(f.step(&mut Fake::default()), FlowEvent::Idle);
        assert!(!f.busy());
    }

    #[test]
    fn listing_waits() {
        let (_tx, rx) = channel::<Result<L, String>>();
        let mut f = Flow::default();
        f.begin_listing(rx);
        assert_eq!(f.step(&mut Fake::default()), FlowEvent::Progress(P_LISTING));
        assert!(f.busy());
    }

    #[test]
    fn list_error_is_fixed_sentence() {
        let (tx, rx) = channel();
        let mut f = Flow::default();
        f.begin_listing(rx);
        tx.send(Err("secret-title cancelled".into())).unwrap();
        assert_eq!(
            f.step(&mut Fake::default()),
            FlowEvent::Error(jizzcraft_fetch::errors::CANCELLED.into())
        );
        assert!(!f.busy());
    }

    #[test]
    fn single_file_auto_picks_then_adds_then_opens() {
        let (mut f, _tx) = listing(one());
        let mut be = Fake::default();
        assert_eq!(f.step(&mut be), FlowEvent::Progress(P_ADDING));
        assert_eq!(be.adds, vec![0]);
        assert_eq!(f.step(&mut be), FlowEvent::Progress(P_ADDING));
        be.add_tx[0].send(Ok(7)).unwrap();
        assert_eq!(f.step(&mut be), FlowEvent::Progress(P_OPENING));
        assert_eq!(be.opens, vec![(7, 0)]);
        be.open_tx[0].send(Ok(42)).unwrap();
        assert_eq!(f.step(&mut be), FlowEvent::Open(42));
    }

    #[test]
    fn no_video_fails() {
        let (mut f, _tx) = listing(vec![("a.txt".into(), 10)]);
        assert_eq!(
            f.step(&mut Fake::default()),
            FlowEvent::Error(NO_VIDEO.into())
        );
        assert!(!f.busy());
    }

    #[test]
    fn multi_file_choose_then_add_then_open() {
        let (mut f, _tx) = listing(two());
        let mut be = Fake::default();
        assert_eq!(f.step(&mut be), FlowEvent::Choose);
        assert_eq!(f.step(&mut be), FlowEvent::Choose);
        assert_eq!(f.choices().unwrap().1, &[0, 1]);
        f.choose(1, &mut be);
        assert_eq!(be.adds, vec![1]);
        be.add_tx[0].send(Ok(3)).unwrap();
        assert_eq!(f.step(&mut be), FlowEvent::Progress(P_OPENING));
        assert_eq!(be.opens, vec![(3, 1)]);
        be.open_tx[0].send(Ok(5)).unwrap();
        assert_eq!(f.step(&mut be), FlowEvent::Open(5));
    }

    #[test]
    fn files_of_added_torrent_choose_opens_directly() {
        let (tx, rx) = channel();
        let mut f = Flow::default();
        f.begin_files(4, rx);
        let mut be = Fake::default();
        assert_eq!(f.step(&mut be), FlowEvent::Progress(P_LISTING));
        tx.send(Ok(two())).unwrap();
        assert_eq!(f.step(&mut be), FlowEvent::Choose);
        f.choose(0, &mut be);
        assert_eq!(be.opens, vec![(4, 0)]);
        assert!(be.adds.is_empty());
    }

    #[test]
    fn files_single_opens_without_add() {
        let (tx, rx) = channel();
        let mut f = Flow::default();
        f.begin_files(9, rx);
        let mut be = Fake::default();
        tx.send(Ok(one())).unwrap();
        assert_eq!(f.step(&mut be), FlowEvent::Progress(P_ADDING));
        assert_eq!(be.opens, vec![(9, 0)]);
    }

    #[test]
    fn add_error() {
        let (mut f, _tx) = listing(one());
        let mut be = Fake::default();
        f.step(&mut be);
        be.add_tx[0].send(Err("disk full".into())).unwrap();
        assert_eq!(
            f.step(&mut be),
            FlowEvent::Error(jizzcraft_fetch::errors::DISK_FULL.into())
        );
        assert!(!f.busy());
    }

    #[test]
    fn open_error() {
        let (tx, rx) = channel();
        let mut f = Flow::default();
        f.begin_opening(rx);
        tx.send(Err("nope".into())).unwrap();
        assert!(matches!(f.step(&mut Fake::default()), FlowEvent::Error(_)));
        assert_eq!(f.step(&mut Fake::default()), FlowEvent::Idle);
    }

    #[test]
    fn disconnected_in_every_waiting_state() {
        let g = classify("error").to_owned();
        let mut be = Fake::default();
        // listing
        let (tx, rx) = channel::<Result<L, String>>();
        let mut f = Flow::default();
        f.begin_listing(rx);
        drop(tx);
        assert_eq!(f.step(&mut be), FlowEvent::Error(g.clone()));
        // files
        let (tx, rx) = channel();
        f.begin_files(1, rx);
        drop(tx);
        assert_eq!(f.step(&mut be), FlowEvent::Error(g.clone()));
        // adding
        let (mut f, _tx) = listing(one());
        f.step(&mut be);
        be.add_tx.clear();
        assert_eq!(f.step(&mut be), FlowEvent::Error(g.clone()));
        // opening
        let (mut f, _tx) = listing(one());
        f.step(&mut be);
        be.add_tx.last().unwrap().send(Ok(1)).unwrap();
        f.step(&mut be);
        be.open_tx.clear();
        assert_eq!(f.step(&mut be), FlowEvent::Error(g));
    }

    #[test]
    fn cancel_and_boss_key_in_each_state() {
        // Cancel and the boss key both end in `cancel`.
        let mut be = Fake::default();
        let (_t1, r1) = channel::<Result<L, String>>();
        let (_t2, r2) = channel();
        let (_t3, r3) = channel::<Result<u32, String>>();
        let mut states: Vec<Flow> = Vec::new();
        for st in [
            FlowState::Listing(r1),
            FlowState::ListingFiles { id: 1, rx: r2 },
            FlowState::Choosing {
                listed: Some(L(two())),
                id: None,
                files: two(),
                picks: vec![0, 1],
            },
            FlowState::Adding {
                rx: channel().1,
                file: 0,
            },
            FlowState::Opening(r3),
            FlowState::Ready(1),
            FlowState::Failed("x".into()),
        ] {
            states.push(Flow { state: st });
        }
        let n = states.len() as u32;
        for f in &mut states {
            f.cancel(&mut be);
            assert!(!f.busy());
            assert_eq!(f.step(&mut be), FlowEvent::Idle);
        }
        assert_eq!(be.cancelled, n);
    }

    #[test]
    fn late_result_after_cancel_is_ignored() {
        let (tx, rx) = channel::<Result<u32, String>>();
        let mut f = Flow::default();
        f.begin_opening(rx);
        let mut be = Fake::default();
        f.cancel(&mut be);
        assert!(tx.send(Ok(1)).is_err());
        assert_eq!(f.step(&mut be), FlowEvent::Idle);
    }

    #[test]
    fn probe_line_shows_peer_count() {
        assert_eq!(
            probe_line(Some(3)).as_deref(),
            Some("Looking for peers (3)")
        );
        assert_eq!(probe_line(None), None);
    }

    #[test]
    fn only_peer_and_metadata_failures_retry() {
        use jizzcraft_fetch::errors::*;
        assert!(is_retryable(classify("no peers")));
        assert!(is_retryable(classify("metadata timeout")));
        assert!(!is_retryable(DISK_FULL));
        assert!(!is_retryable(CANCELLED));
    }

    #[test]
    fn banner_rules() {
        use jizzcraft_fetch::config::ResumeOnStart::{Always, Ask, Never};
        assert_eq!(banners(false, false, Ask, 0, false), vec![]);
        assert_eq!(
            banners(false, false, Ask, 2, false),
            vec![Banner::ResumeAsk(2)]
        );
        assert_eq!(banners(false, false, Ask, 2, true), vec![]);
        assert_eq!(banners(false, false, Never, 2, false), vec![]);
        assert_eq!(banners(false, false, Always, 2, false), vec![]);
        assert_eq!(
            banners(true, false, Ask, 1, false),
            vec![Banner::ConfigUnreadable, Banner::ResumeAsk(1)]
        );
        assert_eq!(banners(true, true, Never, 0, false), vec![]);
    }
}
