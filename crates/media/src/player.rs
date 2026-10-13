//! GStreamer player: `playbin` with an `appsink` as video sink.
//!
//! Threading model: the UI thread never touches the pipeline. A dedicated
//! control thread builds it, applies commands, reads the bus without blocking
//! and tears it down. The UI only reads atomics and takes the latest
//! `gst::Sample` from a one-slot mailbox (ref-counted: no pixel copy).

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UNKNOWN: u64 = u64::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlayState {
    Loading,
    Playing,
    Paused,
    /// End of stream: last frame is held, playhead is at the end.
    Ended,
    Error,
}

impl PlayState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => PlayState::Playing,
            2 => PlayState::Paused,
            3 => PlayState::Ended,
            4 => PlayState::Error,
            _ => PlayState::Loading,
        }
    }
}

/// Counters for `--bench` and debugging. All cumulative since open.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    /// Frames delivered by the sink to the mailbox.
    pub delivered: u64,
    /// Dropped frames reported by GStreamer QoS (late frames).
    pub qos_dropped: u64,
    pub qos_processed: u64,
}

#[derive(Default)]
struct Shared {
    position_ns: AtomicU64,
    duration_ns: AtomicU64,
    state: AtomicU8,
    buffering: AtomicU8,
    delivered: AtomicU64,
    qos_dropped: AtomicU64,
    qos_processed: AtomicU64,
    seeking: AtomicBool,
    has_audio: AtomicBool,
    error: Mutex<Option<String>>,
    mailbox: Mutex<Option<gst::Sample>>,
}

enum Cmd {
    Play,
    Pause,
    Toggle,
    Seek { ns: u64, accurate: bool },
    SeekRel { delta_ns: i64 },
    Volume(f64),
    Mute(bool),
    Replay,
    Stop,
}

/// Latest decoded frame, as a ref-counted sample.
pub struct Frame {
    sample: gst::Sample,
    info: gst_video::VideoInfo,
}

impl Frame {
    pub fn width(&self) -> u32 {
        self.info.width()
    }

    pub fn height(&self) -> u32 {
        self.info.height()
    }

    /// Nominal frame rate (0 when unknown/variable).
    pub fn fps(&self) -> f32 {
        let f = self.info.fps();
        if f.denom() == 0 {
            0.0
        } else {
            f.numer() as f32 / f.denom() as f32
        }
    }

    pub fn stride(&self) -> usize {
        self.info.stride()[0] as usize
    }

    /// Pixel aspect ratio as (numerator, denominator).
    pub fn pixel_aspect(&self) -> (u32, u32) {
        let par = self.info.par();
        (par.numer().max(1) as u32, par.denom().max(1) as u32)
    }

    /// Aspect ratio to display at, honouring non-square pixels.
    pub fn display_aspect(&self) -> f32 {
        let (n, d) = self.pixel_aspect();
        self.width() as f32 * n as f32 / (self.height() as f32 * d as f32)
    }

    /// Borrow the RGBA bytes (row stride = `stride()`); nothing is copied.
    pub fn with_pixels<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let buf = self.sample.buffer()?;
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buf, &self.info).ok()?;
        let data = frame.plane_data(0).ok()?;
        Some(f(data))
    }
}

pub struct Player {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    volume: f64,
    muted: bool,
}

impl Player {
    /// Opens a local file. Returns immediately; everything else (pipeline
    /// construction, state changes) happens on the control thread.
    /// `wake` is called whenever the UI should repaint (new frame, state change).
    pub fn open(path: &Path, volume: f64, wake: Arc<dyn Fn() + Send + Sync>) -> Player {
        let shared = Arc::new(Shared::default());
        shared.duration_ns.store(UNKNOWN, Relaxed);
        let (tx, rx) = channel();
        let path = path.to_path_buf();
        let sh = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("jizzcraft-player".into())
            .spawn(move || control_thread(path, volume, sh, rx, wake))
            .expect("spawn player thread");
        Player {
            tx,
            shared,
            volume,
            muted: false,
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    pub fn play(&self) {
        self.send(Cmd::Play);
    }

    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }

    pub fn toggle(&self) {
        self.send(Cmd::Toggle);
    }

    /// Absolute seek. While scrubbing pass `accurate = false` (key-unit, fast);
    /// on release pass `true`. Seeks are coalesced: one in flight, latest wins.
    pub fn seek(&self, pos: Duration, accurate: bool) {
        self.shared
            .position_ns
            .store(pos.as_nanos() as u64, Relaxed);
        self.send(Cmd::Seek {
            ns: pos.as_nanos() as u64,
            accurate,
        });
    }

    pub fn seek_relative(&self, delta_secs: i64) {
        self.send(Cmd::SeekRel {
            delta_ns: delta_secs * 1_000_000_000,
        });
    }

    pub fn replay(&self) {
        self.send(Cmd::Replay);
    }

    pub fn volume(&self) -> f64 {
        self.volume
    }

    pub fn set_volume(&mut self, v: f64) {
        self.volume = v.clamp(0.0, 1.0);
        self.send(Cmd::Volume(self.volume));
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        self.send(Cmd::Mute(m));
    }

    pub fn state(&self) -> PlayState {
        PlayState::from_u8(self.shared.state.load(Relaxed))
    }

    pub fn position(&self) -> Duration {
        Duration::from_nanos(self.shared.position_ns.load(Relaxed))
    }

    pub fn duration(&self) -> Option<Duration> {
        match self.shared.duration_ns.load(Relaxed) {
            UNKNOWN => None,
            n => Some(Duration::from_nanos(n)),
        }
    }

    /// 100 when not buffering.
    pub fn buffering_percent(&self) -> u8 {
        self.shared.buffering.load(Relaxed)
    }

    pub fn is_seeking(&self) -> bool {
        self.shared.seeking.load(Relaxed)
    }

    pub fn has_audio(&self) -> bool {
        self.shared.has_audio.load(Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    pub fn counters(&self) -> Counters {
        Counters {
            delivered: self.shared.delivered.load(Relaxed),
            qos_dropped: self.shared.qos_dropped.load(Relaxed),
            qos_processed: self.shared.qos_processed.load(Relaxed),
        }
    }

    /// Take the newest frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        let sample = self.shared.mailbox.lock().unwrap().take()?;
        let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
        Some(Frame { sample, info })
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Teardown (set_state(Null)) happens on the control thread.
        let _ = self.tx.send(Cmd::Stop);
    }
}

struct Ctl {
    pipeline: gst::Element,
    shared: Arc<Shared>,
    wake: Arc<dyn Fn() + Send + Sync>,
    seek_in_flight: bool,
    pending_seek: Option<(u64, bool)>,
    /// What the user asked for (as opposed to a temporary buffering pause).
    want_playing: bool,
    buffering_paused: bool,
    is_live: bool,
    last_target_ns: u64,
}

impl Ctl {
    fn set_state(&self, s: PlayState) {
        self.shared.state.store(s as u8, Relaxed);
        (self.wake)();
    }

    fn fail(&self, msg: String) {
        *self.shared.error.lock().unwrap() = Some(msg);
        self.set_state(PlayState::Error);
    }

    fn do_seek(&mut self, ns: u64, accurate: bool) {
        let flags = gst::SeekFlags::FLUSH
            | if accurate {
                gst::SeekFlags::ACCURATE
            } else {
                gst::SeekFlags::KEY_UNIT
            };
        let pos = gst::ClockTime::from_nseconds(ns);
        if self.pipeline.seek_simple(flags, pos).is_ok() {
            self.seek_in_flight = true;
            self.last_target_ns = ns;
            self.shared.seeking.store(true, Relaxed);
            self.shared.position_ns.store(ns, Relaxed);
            // Flush has completed downstream by now: drop anything stale.
            *self.shared.mailbox.lock().unwrap() = None;
            if self.shared.state.load(Relaxed) == PlayState::Ended as u8 {
                self.set_state(if self.want_playing {
                    PlayState::Playing
                } else {
                    PlayState::Paused
                });
            }
        }
    }

    fn request_seek(&mut self, ns: u64, accurate: bool) {
        // Before the first preroll a seek would be rejected: park it, it is
        // applied on the first ASYNC_DONE.
        let loading = self.shared.state.load(Relaxed) == PlayState::Loading as u8;
        if self.seek_in_flight || loading {
            self.pending_seek = Some((ns, accurate));
            self.shared.position_ns.store(ns, Relaxed);
        } else {
            self.do_seek(ns, accurate);
        }
    }

    fn current_ns(&self) -> u64 {
        if self.seek_in_flight {
            self.pending_seek.map_or(self.last_target_ns, |p| p.0)
        } else {
            self.pipeline
                .query_position::<gst::ClockTime>()
                .map_or(self.shared.position_ns.load(Relaxed), |p| p.nseconds())
        }
    }

    fn apply(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Play => {
                self.want_playing = true;
                if !self.buffering_paused {
                    let _ = self.pipeline.set_state(gst::State::Playing);
                    self.set_state(PlayState::Playing);
                }
            }
            Cmd::Pause => {
                self.want_playing = false;
                let _ = self.pipeline.set_state(gst::State::Paused);
                if self.shared.state.load(Relaxed) != PlayState::Ended as u8 {
                    self.set_state(PlayState::Paused);
                }
            }
            Cmd::Toggle => {
                let playing = self.shared.state.load(Relaxed) == PlayState::Playing as u8;
                return self.apply(if playing { Cmd::Pause } else { Cmd::Play });
            }
            Cmd::Seek { ns, accurate } => {
                let dur = self.shared.duration_ns.load(Relaxed);
                let ns = if dur == UNKNOWN { ns } else { ns.min(dur) };
                self.request_seek(ns, accurate);
            }
            Cmd::SeekRel { delta_ns } => {
                let cur = self.current_ns() as i64;
                let mut target = (cur + delta_ns).max(0) as u64;
                let dur = self.shared.duration_ns.load(Relaxed);
                if dur != UNKNOWN {
                    target = target.min(dur);
                }
                self.request_seek(target, true);
            }
            Cmd::Volume(v) => self.pipeline.set_property("volume", v),
            Cmd::Mute(m) => self.pipeline.set_property("mute", m),
            Cmd::Replay => {
                self.want_playing = true;
                self.request_seek(0, true);
                let _ = self.pipeline.set_state(gst::State::Playing);
                self.set_state(PlayState::Playing);
            }
            Cmd::Stop => return false,
        }
        true
    }

    fn on_message(&mut self, msg: &gst::Message) {
        use gst::MessageView;
        match msg.view() {
            MessageView::AsyncDone(_) => {
                self.seek_in_flight = false;
                self.shared.seeking.store(false, Relaxed);
                if let Some((ns, acc)) = self.pending_seek.take() {
                    self.do_seek(ns, acc);
                }
                self.update_duration();
                if self.shared.state.load(Relaxed) == PlayState::Loading as u8 {
                    self.set_state(if self.want_playing {
                        PlayState::Playing
                    } else {
                        PlayState::Paused
                    });
                }
                (self.wake)();
            }
            MessageView::DurationChanged(_) => self.update_duration(),
            MessageView::Eos(_) => {
                let dur = self.shared.duration_ns.load(Relaxed);
                if dur != UNKNOWN {
                    self.shared.position_ns.store(dur, Relaxed);
                }
                self.set_state(PlayState::Ended);
            }
            MessageView::Buffering(b) => {
                let pct = b.percent().clamp(0, 100) as u8;
                self.shared.buffering.store(pct, Relaxed);
                if !self.is_live {
                    if pct < 100 && !self.buffering_paused {
                        self.buffering_paused = true;
                        let _ = self.pipeline.set_state(gst::State::Paused);
                    } else if pct >= 100 && self.buffering_paused {
                        self.buffering_paused = false;
                        if self.want_playing {
                            let _ = self.pipeline.set_state(gst::State::Playing);
                        }
                    }
                }
                (self.wake)();
            }
            MessageView::Qos(q) => {
                let (processed, dropped) = q.stats();
                use gst::format::GenericFormattedValue as G;
                let val = |v: G| u64::try_from(v.value()).ok();
                if let Some(p) = val(processed) {
                    self.shared.qos_processed.fetch_max(p, Relaxed);
                }
                if let Some(d) = val(dropped) {
                    self.shared.qos_dropped.fetch_max(d, Relaxed);
                }
            }
            MessageView::Element(_) => {
                if let Some(text) = crate::describe_missing_plugin(msg) {
                    // Remember it: the follow-up Error message is cryptic.
                    let mut e = self.shared.error.lock().unwrap();
                    if e.is_none() {
                        *e = Some(text);
                    }
                }
            }
            MessageView::Error(e) => {
                let known = self.shared.error.lock().unwrap().clone();
                let text = known.unwrap_or_else(|| {
                    let mut t = format!("playback error: {}", e.error());
                    if let Some(d) = e.debug() {
                        crate::debug_detail(&mut t, &d);
                    }
                    t
                });
                self.fail(text);
            }
            MessageView::StateChanged(s)
                if msg.src().is_some_and(|src| *src == self.pipeline)
                    && s.current() == gst::State::Playing =>
            {
                // Audio presence: `n-audio` is set by playbin after discovery.
                let n_audio: i32 = self.pipeline.property("n-audio");
                self.shared.has_audio.store(n_audio > 0, Relaxed);
            }
            _ => {}
        }
    }

    fn update_duration(&self) {
        if let Some(d) = self.pipeline.query_duration::<gst::ClockTime>() {
            self.shared.duration_ns.store(d.nseconds(), Relaxed);
        }
    }
}

fn build_pipeline(
    path: &Path,
    volume: f64,
    shared: &Arc<Shared>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<gst::Element, String> {
    crate::init()?;
    let uri = crate::file_uri(path)?;
    let mk = |name: &str, hint: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("missing GStreamer element `{name}` — {hint}"))
    };

    let playbin = mk("playbin", "install gst-plugins-base")?;
    playbin.set_property("uri", &uri);
    playbin.set_property("volume", volume);

    // video-sink bin: videoflip(auto orientation) ! videoconvert ! appsink(RGBA)
    let flip = mk("videoflip", "install gst-plugins-good")?;
    flip.set_property_from_str("method", "automatic");
    let conv = mk("videoconvert", "install gst-plugins-base")?;
    conv.set_property("n-threads", 0u32);
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .build();

    let (s1, w1) = (Arc::clone(shared), Arc::clone(wake));
    let (s2, w2) = (Arc::clone(shared), Arc::clone(wake));
    let callbacks = gst_app::AppSinkCallbacks::builder()
        .new_sample(move |sink| {
            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
            *s1.mailbox.lock().unwrap() = Some(sample);
            s1.delivered.fetch_add(1, Relaxed);
            w1();
            Ok(gst::FlowSuccess::Ok)
        })
        // Frames shown while paused (after a seek) arrive as prerolls.
        .new_preroll(move |sink| {
            let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
            *s2.mailbox.lock().unwrap() = Some(sample);
            w2();
            Ok(gst::FlowSuccess::Ok)
        })
        .build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .max_buffers(1)
        .drop(true)
        // The GStreamer clock paces frames; never sync=false.
        .sync(true)
        .callbacks(callbacks)
        .build();

    let bin = gst::Bin::new();
    bin.add_many([&flip, &conv, sink.upcast_ref()])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&flip, &conv, sink.upcast_ref()]).map_err(|e| e.to_string())?;
    let pad = flip.static_pad("sink").ok_or("videoflip has no sink pad")?;
    let ghost = gst::GhostPad::with_target(&pad).map_err(|e| e.to_string())?;
    bin.add_pad(&ghost).map_err(|e| e.to_string())?;
    playbin.set_property("video-sink", &bin);

    // Decoder choice is logged at debug level only.
    if jizzcraft_core::privacy::debug_enabled()
        && let Some(b) = playbin.dynamic_cast_ref::<gst::Bin>()
    {
        b.connect_deep_element_added(|_, _, el| {
            if let Some(f) = el.factory() {
                let klass = f.metadata("klass").unwrap_or_default();
                if klass.contains("Decoder") {
                    eprintln!("[jizzcraft:debug] decoder: {}", f.name());
                }
            }
        });
    }
    Ok(playbin)
}

fn control_thread(
    path: std::path::PathBuf,
    volume: f64,
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    shared.buffering.store(100, Relaxed);
    let pipeline = match build_pipeline(&path, volume, &shared, &wake) {
        Ok(p) => p,
        Err(e) => {
            *shared.error.lock().unwrap() = Some(e);
            shared.state.store(PlayState::Error as u8, Relaxed);
            wake();
            // Stay alive until dropped so late commands don't panic anywhere.
            while rx.recv().is_ok_and(|c| !matches!(c, Cmd::Stop)) {}
            return;
        }
    };
    let bus = pipeline.bus().expect("playbin has a bus");
    let mut ctl = Ctl {
        pipeline: pipeline.clone(),
        shared: Arc::clone(&shared),
        wake,
        seek_in_flight: false,
        pending_seek: None,
        want_playing: true,
        buffering_paused: false,
        is_live: false,
        last_target_ns: 0,
    };

    match pipeline.set_state(gst::State::Playing) {
        Ok(gst::StateChangeSuccess::NoPreroll) => ctl.is_live = true,
        Ok(_) => {}
        Err(_) => ctl.fail("cannot start playback (missing decoder?)".into()),
    }

    let mut last_poll = Instant::now();
    'main: loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(cmd) => {
                if !ctl.apply(cmd) {
                    break 'main;
                }
                // Drain whatever else is queued so rapid seeks coalesce.
                while let Ok(cmd) = rx.try_recv() {
                    if !ctl.apply(cmd) {
                        break 'main;
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 'main,
        }
        // Non-blocking bus read.
        while let Some(msg) = bus.timed_pop(gst::ClockTime::ZERO) {
            ctl.on_message(&msg);
        }
        if last_poll.elapsed() >= Duration::from_millis(100) {
            last_poll = Instant::now();
            let st = shared.state.load(Relaxed);
            if st == PlayState::Playing as u8 && !ctl.seek_in_flight {
                if let Some(p) = pipeline.query_position::<gst::ClockTime>() {
                    shared.position_ns.store(p.nseconds(), Relaxed);
                }
                if shared.duration_ns.load(Relaxed) == UNKNOWN {
                    ctl.update_duration();
                }
            }
        }
    }
    // Teardown off the UI thread.
    let _ = pipeline.set_state(gst::State::Null);
}
