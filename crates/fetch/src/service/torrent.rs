//! Torrent session, add/list/pause/resume/forget and file readers.

use super::reuse::{Stamp, may_reuse, stamp};
use super::{FetchService, StreamReader, StreamTicket};
use crate::config::{FetchConfig, ResumeOnStart};
use crate::diskspace::{self, Fault, Verdict};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, DhtSessionConfig,
    ListenerOptions, Session, SessionOptions, SessionPersistenceConfig, api::TorrentIdOrHash,
};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TorrentState {
    Initializing,
    Downloading,
    Seeding,
    Paused,
    Error,
}

#[derive(Clone, Debug)]
pub struct TorrentStatus {
    pub id: usize,
    pub name: String,
    pub state: TorrentState,
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub eta_secs: Option<u64>,
    /// `Some("disk full")` when the volume ran out of space or is about to
    /// (the torrent is paused), `Some("error")` for any other failure. Never
    /// a raw message.
    pub error: Option<String>,
}

/// How long a magnet may take to turn into a file list.
pub const METADATA_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a torrent may receive nothing after an engine panic before the
/// watchdog gives up on it.
pub const STALL_WINDOW: Duration = Duration::from_secs(20);

/// Last engine panic seen by the hook below, and when.
static FAULT: Mutex<Option<(Instant, Fault)>> = Mutex::new(None);
static HOOK: Once = Once::new();

/// The last engine panic, if it happened at or after `since`: a panic that
/// predates the torrent's last byte (or its restart) says nothing about why
/// it is quiet now.
fn fault_since(since: Instant) -> Fault {
    match FAULT.lock().ok().and_then(|g| *g) {
        Some((at, f)) if at >= since => f,
        _ => Fault::None,
    }
}

/// Remember panics that come from the torrent engine and keep them off the
/// terminal (its message carries a source path). tokio already contains such
/// a panic to its task; what it cannot do is tell us that the torrent behind
/// that task will never move again, which is what the watchdog needs.
/// Every other panic goes to the previous hook untouched.
fn install_fault_hook() {
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let file = info.location().map(|l| l.file()).unwrap_or("");
            if file.contains("librqbit") {
                let kind = if file.ends_with("file_ops.rs") {
                    Fault::Write
                } else {
                    Fault::Other
                };
                if let Ok(mut g) = FAULT.lock() {
                    *g = Some((Instant::now(), kind));
                }
                if std::env::var_os("JIZZCRAFT_DEBUG").is_some() {
                    prev(info);
                }
            } else {
                prev(info);
            }
        }));
    });
}

/// Run `fut` as its own task and send its result; a panic inside it comes
/// back as an error value instead of a hung-up channel.
fn spawn_reply<T: Send + 'static>(
    rt: &tokio::runtime::Runtime,
    tx: Sender<Result<T, String>>,
    fut: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) {
    rt.spawn(async move {
        let r = match tokio::spawn(fut).await {
            Ok(r) => r,
            Err(e) if e.is_panic() => Err("torrent engine fault".to_string()),
            Err(_) => Err("cancelled".to_string()),
        };
        let _ = tx.send(r);
    });
}

type FreeFn = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

#[derive(Default)]
struct SharedInner {
    finished_at: Mutex<HashMap<usize, Instant>>,
    meta_timeout: Mutex<Option<Duration>>,
    free_fn: Mutex<Option<FreeFn>>,
    /// Restored torrents still to be put to sleep (see `session`).
    settle: Mutex<HashSet<usize>>,
    /// Torrents paused by the watchdog, with the status label to show.
    halted: Mutex<HashMap<usize, &'static str>>,
    /// Last progress value seen per torrent and when it last changed.
    progress: Mutex<HashMap<usize, (u64, Instant)>>,
    stall_window: Mutex<Option<Duration>>,
    run_margin: Mutex<Option<u64>>,
    /// Peers given at add time, to feed a torrent that is started again.
    peers: Mutex<HashMap<usize, Vec<std::net::SocketAddr>>>,
    /// Files this session saw being created, per torrent: what a restart
    /// over the partial files is checked against (see `reuse`).
    owned: Mutex<HashMap<usize, HashMap<PathBuf, Stamp>>>,
}

/// State shared between the service and its spawned tasks.
#[derive(Clone, Default)]
pub(crate) struct TorrentShared(Arc<SharedInner>);

impl TorrentShared {
    fn settled(&self, id: usize) {
        if let Ok(mut g) = self.0.settle.lock() {
            g.remove(&id);
        }
    }
    fn timeout(&self) -> Duration {
        self.0
            .meta_timeout
            .lock()
            .ok()
            .and_then(|g| *g)
            .unwrap_or(METADATA_TIMEOUT)
    }
    fn window(&self) -> Duration {
        self.0
            .stall_window
            .lock()
            .ok()
            .and_then(|g| *g)
            .unwrap_or(STALL_WINDOW)
    }
    fn margin(&self) -> u64 {
        self.0
            .run_margin
            .lock()
            .ok()
            .and_then(|g| *g)
            .unwrap_or(diskspace::RUN_MARGIN)
    }
    fn halted(&self, id: usize) -> Option<&'static str> {
        self.0.halted.lock().ok().and_then(|g| g.get(&id).copied())
    }
    fn halt(&self, id: usize, label: &'static str) {
        if let Ok(mut g) = self.0.halted.lock() {
            g.insert(id, label);
        }
    }
    /// Forget the watchdog's memory of `id` (resumed, or not running).
    fn clear(&self, id: usize) {
        if let Ok(mut g) = self.0.halted.lock() {
            g.remove(&id);
        }
        if let Ok(mut g) = self.0.progress.lock() {
            g.remove(&id);
        }
    }
    /// When the progress of `id` last changed (or was first seen).
    fn changed_at(&self, id: usize, bytes: u64) -> Instant {
        let now = Instant::now();
        let Ok(mut g) = self.0.progress.lock() else {
            return now;
        };
        let e = g.entry(id).or_insert((bytes, now));
        if e.0 != bytes {
            *e = (bytes, now);
        }
        e.1
    }
    fn free(&self, p: &Path) -> Option<u64> {
        let f = self.0.free_fn.lock().ok().and_then(|g| g.clone());
        match f {
            Some(f) => f(p),
            None => crate::diskspace::free_bytes(p),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeState {
    /// Looking for peers and for the file list.
    Resolving,
    Ready,
    Failed,
}

/// Live view of a magnet being resolved, for "Looking for peers (n)".
#[derive(Debug, Default)]
pub struct TorrentProbe {
    peers: AtomicU32,
    seen: AtomicU32,
    state: AtomicU8,
}

impl TorrentProbe {
    /// Peers connected right now (session wide: one resolve runs at a time).
    pub fn peers(&self) -> u32 {
        self.peers.load(Ordering::Relaxed)
    }
    pub fn state(&self) -> ProbeState {
        match self.state.load(Ordering::Relaxed) {
            1 => ProbeState::Ready,
            2 => ProbeState::Failed,
            _ => ProbeState::Resolving,
        }
    }
    fn set(&self, s: ProbeState) {
        self.state.store(s as u8, Ordering::Relaxed);
    }
}

/// Marks a probe `Failed` when dropped while still `Resolving`.
struct ProbeGuard(Arc<TorrentProbe>);

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if self.0.state() == ProbeState::Resolving {
            self.0.set(ProbeState::Failed);
        }
    }
}

/// Numbers the player can show while it waits on a torrent, so a pause or a
/// full disk never looks like endless buffering. Updated every half second
/// until the last holder drops it.
#[derive(Debug, Default)]
pub struct StreamStatus {
    pub peers: AtomicU32,
    pub down_bps: AtomicU64,
    /// The torrent is paused: nothing will arrive until it is resumed.
    pub paused: AtomicBool,
    /// `"disk full"` or `"error"` once the torrent failed.
    pub error: Mutex<Option<String>>,
}

impl StreamStatus {
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|g| g.clone())
    }
}

fn is_enospc(raw: &str) -> bool {
    let r = raw.to_ascii_lowercase();
    r.contains("no space left") || r.contains("os error 28") || r.contains("disk quota")
}

impl TorrentStatus {
    pub fn progress(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.progress_bytes as f32 / self.total_bytes as f32
        }
    }
}

fn mibps(v: Option<u32>) -> Option<std::num::NonZeroU32> {
    v.and_then(|m| std::num::NonZeroU32::new(m.saturating_mul(1024 * 1024)))
}

/// A resolved torrent source: a magnet or the .torrent bytes.
#[derive(Clone)]
pub enum Prepared {
    Magnet(String),
    Bytes(Vec<u8>),
}

/// A torrent whose file list is known but which has not been added yet.
pub struct Listed {
    /// Relative file names and lengths, in torrent order. Never log these.
    pub files: Vec<(String, u64)>,
    prepared: Prepared,
    peers: Vec<std::net::SocketAddr>,
}

/// `Read + Seek` over a file that is still downloading. `WouldBlock` means
/// the pieces under the read position have not arrived yet.
pub struct TorrentReader {
    handle: tokio::runtime::Handle,
    // librqbit does not export `FileStream`, so it is held as a trait object.
    stream: Box<dyn AsyncFile>,
}

trait AsyncFile: tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin> AsyncFile for T {}

impl std::io::Read for TorrentReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        let fut = self.stream.as_mut().read(buf);
        // A panic of the engine must not unwind into the player thread.
        let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle
                .block_on(async { tokio::time::timeout(Duration::from_millis(100), fut).await })
        }));
        match polled {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(std::io::ErrorKind::WouldBlock.into()),
            Err(_) => Err(std::io::Error::other("torrent engine fault")),
        }
    }
}

impl std::io::Seek for TorrentReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        use tokio::io::AsyncSeekExt;
        let fut = self.stream.as_mut().seek(pos);
        let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle
                .block_on(async { tokio::time::timeout(Duration::from_secs(5), fut).await })
        }));
        match polled {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(std::io::ErrorKind::TimedOut.into()),
            Err(_) => Err(std::io::Error::other("torrent engine fault")),
        }
    }
}

/// What to add. A `TorrentUrl` is fetched over HTTP first (indexer download
/// link); a `Magnet` goes straight to the session.
#[derive(Clone)]
pub enum Source {
    Magnet(String),
    TorrentUrl(String),
    TorrentFile(PathBuf),
}

/// Reject any path that could escape the download folder.
pub fn path_is_safe(p: &Path) -> bool {
    !p.as_os_str().is_empty()
        && p.components().all(|c| match c {
            Component::Normal(n) => name_is_portable(&n.to_string_lossy()),
            _ => false,
        })
}

/// A file name that is harmless on every platform: no alternate data stream
/// (`a:b`), no characters Windows forbids, no control characters, no trailing
/// dot or space, no reserved device name. Checked on all platforms so a
/// torrent behaves the same everywhere.
fn name_is_portable(n: &str) -> bool {
    if n.is_empty()
        || n.chars()
            .any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*' | '\\'))
        || n.ends_with('.')
        || n.ends_with(' ')
    {
        return false;
    }
    // Windows trims trailing spaces of the stem before comparing. It also
    // reserves `COM¹`, because superscripts were apparently a threat vector.
    let stem = n
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p).is_some_and(|d| {
            let mut c = d.chars();
            matches!(c.next(), Some('0'..='9' | '¹' | '²' | '³')) && c.next().is_none()
        })
    };
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT");
    !reserved
}

impl FetchService {
    async fn session(
        cfg: &FetchConfig,
        state_dir: &Path,
        cell: &OnceCell<Arc<Session>>,
        shared: &TorrentShared,
    ) -> Result<Arc<Session>, String> {
        let dir = cfg.effective_download_dir();
        cell.get_or_try_init(|| async {
            install_fault_hook();
            if cfg.require_proxy && cfg.proxy.as_ref().is_none_or(|p| p.is_empty()) {
                return Err("proxy required".to_string());
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let policy = cfg.net_policy();
            let opts = SessionOptions {
                disable_trackers: !policy.trackers,
                dht: policy.dht.then(|| DhtSessionConfig {
                    persistence: None,
                    ..Default::default()
                }),
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(state_dir.to_path_buf()),
                }),
                fastresume: true,
                // UPnP is explicit: off, and not configurable.
                listen: policy.listen.then(|| ListenerOptions {
                    enable_upnp_port_forwarding: policy.upnp,
                    ..Default::default()
                }),
                bind_device_name: cfg.bind_interface.clone().filter(|n| !n.is_empty()),
                connect: Some(ConnectionOptions {
                    proxy_url: cfg
                        .proxy
                        .as_ref()
                        .filter(|p| !p.is_empty())
                        .map(|p| p.expose().to_owned()),
                    ..Default::default()
                }),
                disable_local_service_discovery: true,
                ratelimits: librqbit::limits::LimitsConfig {
                    download_bps: mibps(cfg.down_limit_mibps),
                    upload_bps: mibps(cfg.up_limit_mibps),
                },
                ..Default::default()
            };
            let session = Session::new_with_opts(dir, opts)
                .await
                .map_err(|e| e.to_string())?;
            // Persisted torrents come back running. Unless the user asked for
            // that, put them to sleep before they move a byte more.
            if cfg.resume_on_start != ResumeOnStart::Always {
                let ids: HashSet<usize> =
                    session.with_torrents(|it| it.map(|(id, _)| id).collect());
                if let Ok(mut g) = shared.0.settle.lock() {
                    g.extend(ids);
                }
                // A torrent that is still checking its files may start live
                // right after the check: keep watching for a while.
                let (s, sh) = (session.clone(), shared.clone());
                tokio::spawn(async move {
                    for _ in 0..300 {
                        let pending: Vec<usize> =
                            sh.0.settle
                                .lock()
                                .map(|g| g.iter().copied().collect())
                                .unwrap_or_default();
                        if pending.is_empty() {
                            return;
                        }
                        for id in pending {
                            let Some(h) = s.get(TorrentIdOrHash::Id(id)) else {
                                sh.settled(id);
                                continue;
                            };
                            match h.stats().state {
                                librqbit::TorrentStatsState::Live => {
                                    let _ = s.pause(&h).await;
                                    sh.settled(id);
                                }
                                librqbit::TorrentStatsState::Initializing { .. } => {
                                    let _ = s.pause(&h).await;
                                }
                                _ => sh.settled(id),
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                });
            }
            tokio::spawn(Self::watchdog(
                session.clone(),
                cfg.effective_download_dir(),
                shared.clone(),
            ));
            Ok(session)
        })
        .await
        .cloned()
    }

    /// Once a second: pause running torrents the volume cannot hold, and
    /// torrents that the engine left stuck after a panic. Every tick is its
    /// own task, so a panic in one cannot end the loop.
    async fn watchdog(session: Arc<Session>, dir: PathBuf, shared: TorrentShared) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (s, d, sh) = (session.clone(), dir.clone(), shared.clone());
            let _ = tokio::spawn(async move { Self::watch_tick(&s, &d, &sh).await }).await;
        }
    }

    async fn watch_tick(session: &Arc<Session>, dir: &Path, shared: &TorrentShared) {
        let items: Vec<_> = session.with_torrents(|it| it.map(|(id, h)| (id, h.clone())).collect());
        let mut running = Vec::new();
        for (id, h) in items {
            let st = h.stats();
            if matches!(st.state, librqbit::TorrentStatsState::Live) && !st.finished {
                running.push((id, h, st.progress_bytes, st.total_bytes));
            } else if st.error.as_deref().is_some_and(is_enospc) {
                shared.halt(id, "disk full");
            } else if !matches!(st.state, librqbit::TorrentStatsState::Initializing { .. })
                && let Ok(mut g) = shared.0.progress.lock()
            {
                g.remove(&id);
            }
        }
        if running.is_empty() {
            return;
        }
        let remaining = running
            .iter()
            .map(|(_, _, p, t)| t.saturating_sub(*p))
            .fold(0u64, u64::saturating_add);
        let free = shared.free(dir);
        let window = shared.window();
        for (id, h, progress, _) in running {
            let since = shared.changed_at(id, progress);
            let (idle, fault) = (since.elapsed(), fault_since(since));
            let label =
                match diskspace::judge(free, remaining, idle, window, fault, shared.margin()) {
                    Verdict::Ok => continue,
                    Verdict::DiskFull => "disk full",
                    Verdict::Error => "error",
                };
            shared.halt(id, label);
            let _ = session.pause(&h).await;
        }
    }

    /// Start a stopped torrent again; returns the id it runs under.
    ///
    /// A torrent that librqbit moved to its error state (a fatal write
    /// error, which is what a real ENOSPC usually is) cannot be started from
    /// there: it tries to create its files anew and refuses because they
    /// exist. It is then forgotten (files stay) and added again over its own
    /// partial files, with the same file selection and peers. The id changes.
    /// Only torrents this session added get here, and the torrent is added
    /// over existing files only after `reuse::may_reuse` has shown that every
    /// one of them is the same file this session wrote (same inode, regular,
    /// not longer than the torrent says). Otherwise the restart is refused.
    async fn start_again(
        session: &Arc<Session>,
        shared: &TorrentShared,
        dir: &Path,
        id: usize,
        h: &Arc<librqbit::ManagedTorrent>,
    ) -> Result<usize, String> {
        if !matches!(h.stats().state, librqbit::TorrentStatsState::Error) {
            session.unpause(h).await.map_err(|e| e.to_string())?;
            return Ok(id);
        }
        let bytes = h
            .with_metadata(|m| m.torrent_bytes.clone())
            .map_err(|e| e.to_string())?;
        let only = h.only_files();
        let peers = shared
            .0
            .peers
            .lock()
            .ok()
            .and_then(|g| g.get(&id).cloned())
            .unwrap_or_default();
        let files: Vec<(PathBuf, u64)> = h
            .with_metadata(|m| {
                m.file_infos
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| only.as_ref().is_none_or(|o| o.contains(i)))
                    .map(|(_, f)| (f.relative_filename.clone(), f.len))
                    .collect()
            })
            .map_err(|e| e.to_string())?;
        let stamps = shared
            .0
            .owned
            .lock()
            .ok()
            .and_then(|g| g.get(&id).cloned())
            .unwrap_or_default();
        may_reuse(dir, &files, &stamps)?;
        session
            .delete(TorrentIdOrHash::Id(id), false)
            .await
            .map_err(|e| e.to_string())?;
        shared.clear(id);
        let added = session
            .add_torrent(
                AddTorrent::from_bytes(bytes),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(dir.to_string_lossy().into_owned()),
                    only_files: only,
                    initial_peers: (!peers.is_empty()).then(|| peers.clone()),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        let new = added
            .into_handle()
            .map(|h| h.id())
            .ok_or_else(|| "torrent was not added".to_string())?;
        if let Ok(mut g) = shared.0.peers.lock() {
            g.remove(&id);
            if !peers.is_empty() {
                g.insert(new, peers);
            }
        }
        if let Ok(mut g) = shared.0.owned.lock()
            && let Some(m) = g.remove(&id)
        {
            g.insert(new, m);
        }
        Ok(new)
    }

    /// Room check before a paused torrent is started again. False (and the
    /// torrent keeps its typed "disk full" status) when it would not fit.
    fn may_start(
        shared: &TorrentShared,
        dir: &Path,
        id: usize,
        h: &librqbit::ManagedTorrent,
    ) -> bool {
        let st = h.stats();
        let remaining = st.total_bytes.saturating_sub(st.progress_bytes);
        if diskspace::is_low(shared.free(dir), remaining, shared.margin()) {
            shared.halt(id, "disk full");
            return false;
        }
        shared.clear(id);
        true
    }

    /// Test hook: room kept free on top of what a running torrent still
    /// has to write (default [`diskspace::RUN_MARGIN`], 64 MiB).
    pub fn set_run_margin(&self, bytes: u64) {
        if let Ok(mut g) = self.tor.0.run_margin.lock() {
            *g = Some(bytes);
        }
    }

    /// Test hook: how long a torrent may sit still after an engine panic.
    pub fn set_stall_window(&self, d: Duration) {
        if let Ok(mut g) = self.tor.0.stall_window.lock() {
            *g = Some(d);
        }
    }

    /// File names and lengths of an already added torrent.
    pub fn torrent_files(&self, id: usize) -> Receiver<Result<Vec<(String, u64)>, String>> {
        let (tx, rx) = channel();
        let session = self.session.clone();
        spawn_reply(self.rt(), tx, async move {
            let s = session.get().cloned().ok_or("no session")?;
            let h = s.get(TorrentIdOrHash::Id(id)).ok_or("torrent not found")?;
            h.with_metadata(|m| {
                m.file_infos
                    .iter()
                    .map(|f| (f.relative_filename.to_string_lossy().into_owned(), f.len))
                    .collect::<Vec<_>>()
            })
            .map_err(|e| e.to_string())
        });
        rx
    }

    /// How long a magnet may take to resolve (default 120 s).
    pub fn set_metadata_timeout(&self, d: Duration) {
        if let Ok(mut g) = self.tor.0.meta_timeout.lock() {
            *g = Some(d);
        }
    }

    /// Replace the free-space query used by the disk preflight (tests, or a
    /// platform without `statvfs`).
    pub fn set_free_space_fn(&self, f: impl Fn(&Path) -> Option<u64> + Send + Sync + 'static) {
        if let Ok(mut g) = self.tor.0.free_fn.lock() {
            *g = Some(Arc::new(f));
        }
    }

    /// Push the speed limits of the current config into a running session.
    pub(super) fn apply_limits(&self) {
        if let Some(s) = self.session.get() {
            s.ratelimits
                .set_download_bps(mibps(self.cfg.down_limit_mibps));
            s.ratelimits.set_upload_bps(mibps(self.cfg.up_limit_mibps));
        }
    }

    /// Same token the HTTP openers use: `cancel_streams` ends a magnet that
    /// is still looking for its metadata.
    fn torrent_cancel(&self) -> Arc<AtomicBool> {
        self.stream_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Open the session (restores persisted torrents). Idempotent.
    pub fn start(&self) -> Receiver<Result<(), String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        let shared = self.tor.clone();
        spawn_reply(self.rt(), tx, async move {
            Self::session(&cfg, &state, &cell, &shared)
                .await
                .map(|_| ())
        });
        rx
    }

    /// Add a whole torrent. Returns its id.
    pub fn add(&self, source: Source) -> Receiver<Result<usize, String>> {
        self.add_with_peers(source, Vec::new())
    }

    /// Like [`add`](Self::add), with known peer addresses to try first.
    pub fn add_with_peers(
        &self,
        source: Source,
        peers: Vec<std::net::SocketAddr>,
    ) -> Receiver<Result<usize, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        let (shared, tok) = (self.tor.clone(), self.torrent_cancel());
        spawn_reply(self.rt(), tx, async move {
            let prepared = Self::prepare(source).await?;
            let probe = Arc::new(TorrentProbe::default());
            Self::add_prepared(
                &cfg, &state, &cell, prepared, None, peers, &shared, &probe, tok,
            )
            .await
        });
        rx
    }

    /// Resolve a source to bytes or a magnet, then read the file list
    /// without writing anything. Unsafe paths are refused here.
    pub fn list_files(&self, source: Source) -> Receiver<Result<Listed, String>> {
        self.list_files_with_peers(source, Vec::new())
    }

    pub fn list_files_with_peers(
        &self,
        source: Source,
        peers: Vec<std::net::SocketAddr>,
    ) -> Receiver<Result<Listed, String>> {
        self.list_files_probed(source, peers).0
    }

    /// Like [`list_files_with_peers`](Self::list_files_with_peers), plus a
    /// live probe. A magnet that finds no metadata within the timeout (120 s)
    /// fails with `no peers` (nobody was ever connected) or `metadata
    /// timeout`; `cancel_streams` ends it with `cancelled`.
    pub fn list_files_probed(
        &self,
        source: Source,
        peers: Vec<std::net::SocketAddr>,
    ) -> (Receiver<Result<Listed, String>>, Arc<TorrentProbe>) {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        let (shared, tok) = (self.tor.clone(), self.torrent_cancel());
        let probe = Arc::new(TorrentProbe::default());
        let p2 = probe.clone();
        spawn_reply(self.rt(), tx, async move {
            // Failed unless it completes; also when the task panics.
            let _g = ProbeGuard(p2.clone());
            let prepared = Self::prepare(source).await?;
            let session = Self::session(&cfg, &state, &cell, &shared).await?;
            let files =
                Self::list_prepared(&session, &prepared, &peers, &p2, shared.timeout(), tok)
                    .await?;
            p2.set(ProbeState::Ready);
            Ok(Listed {
                files,
                prepared,
                peers,
            })
        });
        (rx, probe)
    }

    /// Add a listed torrent; `only` limits the download to those file
    /// indexes (None = everything). Returns its id.
    pub fn add_listed(
        &self,
        listed: Listed,
        only: Option<Vec<usize>>,
    ) -> Receiver<Result<usize, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        let (shared, tok) = (self.tor.clone(), self.torrent_cancel());
        spawn_reply(self.rt(), tx, async move {
            let probe = Arc::new(TorrentProbe::default());
            Self::add_prepared(
                &cfg,
                &state,
                &cell,
                listed.prepared,
                only,
                listed.peers,
                &shared,
                &probe,
                tok,
            )
            .await
        });
        rx
    }

    async fn prepare(source: Source) -> Result<Prepared, String> {
        Ok(match source {
            Source::Magnet(m) => Prepared::Magnet(m),
            Source::TorrentUrl(u) if u.starts_with("magnet:") => Prepared::Magnet(u),
            Source::TorrentUrl(u) => Prepared::Bytes(Self::http_get(u, Arc::default()).await?),
            Source::TorrentFile(p) => {
                Prepared::Bytes(std::fs::read(&p).map_err(|e| e.kind().to_string())?)
            }
        })
    }

    fn to_add(p: &Prepared) -> AddTorrent<'static> {
        match p {
            Prepared::Magnet(m) => AddTorrent::from_url(m.clone()),
            Prepared::Bytes(b) => AddTorrent::from_bytes(b.clone()),
        }
    }

    /// Samples the connected-peer count into `probe` until aborted.
    fn sample_peers(
        session: Arc<Session>,
        probe: Arc<TorrentProbe>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let p = session.stats_snapshot().peers;
                let n = p
                    .live_tcp
                    .saturating_add(p.live_utp)
                    .saturating_add(p.live_socks);
                probe.peers.store(n, Ordering::Relaxed);
                probe.seen.fetch_max(n, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
    }

    async fn list_prepared(
        session: &Arc<Session>,
        p: &Prepared,
        peers: &[std::net::SocketAddr],
        probe: &Arc<TorrentProbe>,
        timeout: Duration,
        tok: Arc<AtomicBool>,
    ) -> Result<Vec<(String, u64)>, String> {
        probe.set(ProbeState::Resolving);
        let sampler = Self::sample_peers(session.clone(), probe.clone());
        let fut = session.add_torrent(
            Self::to_add(p),
            Some(AddTorrentOptions {
                list_only: true,
                initial_peers: (!peers.is_empty()).then(|| peers.to_vec()),
                ..Default::default()
            }),
        );
        let res = tokio::select! {
            r = tokio::time::timeout(timeout, fut) => match r {
                Ok(r) => r.map_err(|e| {
                    let e = e.to_string();
                    if e.contains("no known way to resolve peers")
                        || e.contains("stream exhausted")
                    {
                        "no peers".to_string()
                    } else {
                        e
                    }
                }),
                Err(_) if probe.seen.load(Ordering::Relaxed) == 0 => Err("no peers".to_string()),
                Err(_) => Err("metadata timeout".to_string()),
            },
            _ = super::open::wait_cancel(tok) => Err("cancelled".to_string()),
        };
        sampler.abort();
        let AddTorrentResponse::ListOnly(l) = res? else {
            return Err("torrent is already being downloaded".into());
        };
        let mut out = Vec::new();
        for f in l.info.iter_file_details() {
            let path = f.filename.to_pathbuf();
            if !path_is_safe(&path) {
                return Err("torrent contains an unsafe file path; refused".into());
            }
            out.push((path.to_string_lossy().into_owned(), f.len));
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    async fn add_prepared(
        cfg: &FetchConfig,
        state: &Path,
        cell: &OnceCell<Arc<Session>>,
        prepared: Prepared,
        only: Option<Vec<usize>>,
        peers: Vec<std::net::SocketAddr>,
        shared: &TorrentShared,
        probe: &Arc<TorrentProbe>,
        tok: Arc<AtomicBool>,
    ) -> Result<usize, String> {
        let session = Self::session(cfg, state, cell, shared).await?;
        // The metadata pass also validates every path before anything is written.
        let files =
            Self::list_prepared(&session, &prepared, &peers, probe, shared.timeout(), tok).await?;
        let dir = cfg.effective_download_dir();
        // Disk preflight: what will be written, plus room to breathe.
        let needed: u64 = files
            .iter()
            .enumerate()
            .filter(|(i, _)| only.as_ref().is_none_or(|o| o.contains(i)))
            .map(|(_, (_, l))| *l)
            .fold(0u64, u64::saturating_add);
        if crate::diskspace::is_full(shared.free(&dir), needed) {
            return Err("disk full".into());
        }
        let given = peers.clone();
        let added = session
            .add_torrent(
                Self::to_add(&prepared),
                Some(AddTorrentOptions {
                    overwrite: false,
                    output_folder: Some(dir.to_string_lossy().into_owned()),
                    only_files: only,
                    initial_peers: (!peers.is_empty()).then_some(peers),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        let id = added
            .into_handle()
            .map(|h| h.id())
            .ok_or_else(|| "torrent was not added".to_string())?;
        if !given.is_empty()
            && let Ok(mut g) = shared.0.peers.lock()
        {
            g.insert(id, given);
        }
        Ok(id)
    }

    /// Something to play for file `file` of torrent `id`: the finished file
    /// on disk, or a reader over the part that is still downloading. A live
    /// ticket carries a [`StreamStatus`] that shows peers, speed, `paused`
    /// and `error`.
    pub fn open_reader(&self, id: usize, file: usize) -> Receiver<Result<StreamTicket, String>> {
        let (tx, rx) = channel();
        let session = self.session.clone();
        let handle = self.rt().handle().clone();
        let (shared, dir) = (self.tor.clone(), self.cfg.effective_download_dir());
        spawn_reply(self.rt(), tx, async move {
            let session = session.get().cloned().ok_or("no session")?;
            let mut id = id;
            let mut h = session
                .get(TorrentIdOrHash::Id(id))
                .ok_or("torrent not found")?;
            // Metadata and state may take a moment after an add.
            let t0 = std::time::Instant::now();
            let (len, rel) = loop {
                if let Ok(v) = h.with_metadata(|m| {
                    m.file_infos
                        .get(file)
                        .map(|f| (f.len, f.relative_filename.clone()))
                }) && let Some(v) = v
                {
                    break v;
                }
                if t0.elapsed() > Duration::from_secs(60) {
                    return Err("could not read the torrent metadata".to_string());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            let path = h.output_folder().join(&rel);
            if !path_is_safe(&rel) {
                return Err("unsafe file path".to_string());
            }
            let done = h.stats().file_progress.get(file).is_some_and(|&p| p >= len);
            if done && path.is_file() {
                return Ok(StreamTicket::Done(path));
            }
            // A paused torrent never receives pieces: the user asked to play.
            // "Play" is not a suggestion.
            if h.is_paused() || h.stats().error.is_some() {
                // ... unless the volume cannot hold it: say so, typed.
                if !FetchService::may_start(&shared, &dir, id, &h) {
                    return Err("disk full".to_string());
                }
                if let Ok(new) = FetchService::start_again(&session, &shared, &dir, id, &h).await {
                    id = new;
                    h = session
                        .get(TorrentIdOrHash::Id(id))
                        .ok_or("torrent not found")?;
                }
            }
            let stream = loop {
                match h.clone().stream(file).await {
                    Ok(s) => break s,
                    Err(_) if t0.elapsed() < Duration::from_secs(60) => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    Err(_) => return Err("could not start streaming".to_string()),
                }
            };
            let status = Arc::new(StreamStatus::default());
            refresh_status(&h, &status, shared.halted(id));
            let (st, hh) = (status.clone(), h.clone());
            tokio::spawn(async move {
                // Ends when the player drops its handle (we hold the other one).
                while Arc::strong_count(&st) > 1 {
                    refresh_status(&hh, &st, shared.halted(id));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
            Ok(StreamTicket::Live {
                reader: StreamReader(Box::new(TorrentReader {
                    handle,
                    stream: Box::new(stream),
                })),
                len,
                name: rel
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                status: Some(status),
            })
        });
        rx
    }

    /// Snapshot of every torrent. Reading is cheap; the cache is refreshed
    /// on the runtime so the UI thread never waits on the session. The same
    /// refresh enforces the seeding policy and the disk-full pause.
    pub fn list(&self) -> Vec<TorrentStatus> {
        if let Some(session) = self.session.get().cloned() {
            let cache = self.cache.clone();
            let cfg = self.cfg.clone();
            let shared = self.tor.clone();
            self.rt().spawn(async move {
                let mut stop = Vec::new();
                let items: Vec<_> =
                    session.with_torrents(|it| it.map(|(id, h)| (id, h.clone())).collect());
                let mut v = Vec::with_capacity(items.len());
                for (id, h) in items {
                    let s = h.stats();
                    if s.error.is_none() {
                        record_owned(&shared, id, &h);
                    }
                    let disk_full = s.error.as_deref().is_some_and(is_enospc);
                    // Paused by the watchdog: typed status until it runs again.
                    let halted = shared.halted(id).filter(|_| {
                        let paused = matches!(s.state, librqbit::TorrentStatsState::Paused);
                        if !paused && matches!(s.state, librqbit::TorrentStatsState::Live) {
                            shared.clear(id);
                        }
                        paused
                    });
                    let state = match (&s.state, s.error.is_some(), s.finished) {
                        // A full disk is a pause, not a failure: it starts
                        // again from this state once there is room.
                        (_, true, _) if disk_full => TorrentState::Paused,
                        (_, true, _) => TorrentState::Error,
                        (librqbit::TorrentStatsState::Paused, _, _) => TorrentState::Paused,
                        (librqbit::TorrentStatsState::Initializing { paused: true }, _, _) => {
                            TorrentState::Paused
                        }
                        (librqbit::TorrentStatsState::Initializing { .. }, _, _) => {
                            TorrentState::Initializing
                        }
                        (_, _, true) => TorrentState::Seeding,
                        _ => TorrentState::Downloading,
                    };
                    if disk_full {
                        // Stop the torrent so it does not retry in a loop.
                        stop.push(h.clone());
                    }
                    if s.finished && state != TorrentState::Paused {
                        let over = cfg.seed_after_complete
                            && seed_goal_reached(
                                &cfg,
                                &shared,
                                id,
                                s.uploaded_bytes,
                                s.total_bytes,
                            );
                        if !cfg.seed_after_complete || over {
                            stop.push(h.clone());
                        }
                    }
                    let (down, up) = s.live.as_ref().map_or((0, 0), |l| {
                        (
                            (l.download_speed.mbps * 1_048_576.0) as u64,
                            (l.upload_speed.mbps * 1_048_576.0) as u64,
                        )
                    });
                    let eta = (down > 0 && !s.finished)
                        .then(|| s.total_bytes.saturating_sub(s.progress_bytes) / down);
                    v.push(TorrentStatus {
                        id,
                        name: h.name().unwrap_or_default(),
                        state,
                        progress_bytes: s.progress_bytes,
                        total_bytes: s.total_bytes,
                        down_bps: down,
                        up_bps: up,
                        eta_secs: eta,
                        error: s
                            .error
                            .as_ref()
                            .map(|_| if disk_full { "disk full" } else { "error" })
                            .or(halted)
                            .map(str::to_string),
                    });
                }
                for h in stop {
                    let _ = session.pause(&h).await;
                }
                if let Ok(mut g) = cache.lock() {
                    *g = v;
                }
            });
        }
        self.cache.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn pause(&self, id: usize) {
        if let Some(s) = self.session.get().cloned() {
            self.rt().spawn(async move {
                if let Some(h) = s.get(TorrentIdOrHash::Id(id)) {
                    let _ = s.pause(&h).await;
                }
            });
        }
    }

    pub fn resume(&self, id: usize) {
        if let Some(s) = self.session.get().cloned() {
            let shared = self.tor.clone();
            let dir = self.cfg.effective_download_dir();
            self.rt().spawn(async move {
                if let Some(h) = s.get(TorrentIdOrHash::Id(id)) {
                    // A manual resume restarts the seeding clock.
                    if let Ok(mut g) = shared.0.finished_at.lock() {
                        g.remove(&id);
                    }
                    shared.settled(id);
                    // Still no room: stay paused, keep the typed status.
                    if !h.stats().finished && !Self::may_start(&shared, &dir, id, &h) {
                        return;
                    }
                    let _ = Self::start_again(&s, &shared, &dir, id, &h).await;
                }
            });
        }
    }

    /// Resume every paused, unfinished torrent (the answer to "Resume
    /// downloads from last time?"). Yields how many were resumed.
    pub fn resume_all(&self) -> Receiver<usize> {
        let (tx, rx) = channel();
        let session = self.session.clone();
        let shared = self.tor.clone();
        let dir = self.cfg.effective_download_dir();
        self.rt().spawn(async move {
            let mut n = 0;
            if let Some(s) = session.get().cloned() {
                let hs: Vec<_> = s.with_torrents(|it| it.map(|(id, h)| (id, h.clone())).collect());
                for (id, h) in hs {
                    shared.settled(id);
                    let st = h.stats();
                    let stopped = h.is_paused() || st.error.as_deref().is_some_and(is_enospc);
                    if stopped
                        && !st.finished
                        && Self::may_start(&shared, &dir, id, &h)
                        && Self::start_again(&s, &shared, &dir, id, &h).await.is_ok()
                    {
                        n += 1;
                    }
                }
            }
            let _ = tx.send(n);
        });
        rx
    }

    /// Stop tracking a torrent. Downloaded files are left on disk, always.
    /// We forget; we do not shred.
    pub fn forget(&self, id: usize) {
        if let Some(s) = self.session.get().cloned() {
            if let Ok(mut g) = self.tor.0.owned.lock() {
                g.remove(&id);
            }
            self.rt().spawn(async move {
                let _ = s.delete(TorrentIdOrHash::Id(id), false).await;
            });
        }
    }
}

/// Remember the identity of every selected file of torrent `id` that exists
/// and was not stamped yet. A file that is a link, or not there, is skipped.
fn record_owned(shared: &TorrentShared, id: usize, h: &Arc<librqbit::ManagedTorrent>) {
    let only = h.only_files();
    let base = h.output_folder().to_path_buf();
    let Ok(files) = h.with_metadata(|m| {
        m.file_infos
            .iter()
            .enumerate()
            .filter(|(i, _)| only.as_ref().is_none_or(|o| o.contains(i)))
            .map(|(_, f)| (f.relative_filename.clone(), f.len))
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    let Ok(mut g) = shared.0.owned.lock() else {
        return;
    };
    let m = g.entry(id).or_default();
    for (rel, len) in files {
        if !m.contains_key(&rel)
            && path_is_safe(&rel)
            && let Some(st) = stamp(&base.join(&rel), len)
        {
            m.insert(rel, st);
        }
    }
}

/// True once the ratio or the time limit of the seeding policy is met.
fn seed_goal_reached(
    cfg: &FetchConfig,
    shared: &TorrentShared,
    id: usize,
    uploaded: u64,
    total: u64,
) -> bool {
    if let Some(r) = cfg.seed_ratio
        && r > 0.0
        && total > 0
        && uploaded as f64 / total as f64 >= f64::from(r)
    {
        return true;
    }
    if let Some(m) = cfg.seed_minutes.filter(|m| *m > 0)
        && let Ok(mut g) = shared.0.finished_at.lock()
    {
        let t = *g.entry(id).or_insert_with(Instant::now);
        return t.elapsed() >= Duration::from_secs(u64::from(m) * 60);
    }
    false
}

fn refresh_status(
    h: &Arc<librqbit::ManagedTorrent>,
    st: &StreamStatus,
    halted: Option<&'static str>,
) {
    let s = h.stats();
    let paused = h.is_paused() || matches!(s.state, librqbit::TorrentStatsState::Paused);
    st.paused.store(paused, Ordering::Relaxed);
    if let Some(l) = &s.live {
        let p = &l.snapshot.peer_stats;
        let n = p
            .live_tcp
            .saturating_add(p.live_utp)
            .saturating_add(p.live_socks);
        st.peers.store(n, Ordering::Relaxed);
        st.down_bps.store(
            (l.download_speed.mbps * 1_048_576.0) as u64,
            Ordering::Relaxed,
        );
    } else {
        st.peers.store(0, Ordering::Relaxed);
        st.down_bps.store(0, Ordering::Relaxed);
    }
    if let Ok(mut g) = st.error.lock() {
        *g = s
            .error
            .as_deref()
            .map(|e| if is_enospc(e) { "disk full" } else { "error" }.to_string())
            .or_else(|| halted.map(str::to_string));
    }
}
