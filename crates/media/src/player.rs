//! GStreamer player: `playbin` with an `appsink` as video sink.
//!
//! Threading model: the UI thread never touches the pipeline. A dedicated
//! control thread builds it, applies commands, reads the bus without blocking
//! and tears it down. The UI only reads atomics and takes the latest
//! `gst::Sample` from a one-slot mailbox (ref-counted: no pixel copy).

use crate::stream::{Feeder, ReadSeek};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UNKNOWN: u64 = u64::MAX;

/// What a player reads from.
enum Input {
    File(PathBuf),
    Stream { reader: Box<dyn ReadSeek>, len: u64 },
}

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
    /// Playback rate as `f64` bits (1.0 by default).
    rate_bits: AtomicU64,
    loop_a: AtomicU64,
    loop_b: AtomicU64,
    error: Mutex<Option<String>>,
    mailbox: Mutex<Option<gst::Sample>>,
}

enum Cmd {
    Play,
    Pause,
    Toggle,
    Seek {
        ns: u64,
        accurate: bool,
    },
    SeekRel {
        delta_ns: i64,
    },
    Volume(f64),
    Mute(bool),
    Replay,
    Rate(f64),
    Step {
        forward: bool,
        fps: f32,
    },
    /// Loop points live in `Shared`; this only wakes the control loop.
    Loop,
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
        Self::spawn(Input::File(path.to_path_buf()), volume, wake)
    }

    /// Plays from an in-process reader of `len` bytes that may still be
    /// filling up (a download in progress). No URL, no socket: bytes are
    /// pulled from `reader` and pushed into an `appsrc`. `WouldBlock` from the
    /// reader means "not downloaded yet" and shows as buffering.
    pub fn open_stream(
        reader: Box<dyn ReadSeek>,
        len: u64,
        volume: f64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Player {
        Self::spawn(Input::Stream { reader, len }, volume, wake)
    }

    fn spawn(input: Input, volume: f64, wake: Arc<dyn Fn() + Send + Sync>) -> Player {
        let shared = Arc::new(Shared::default());
        shared.duration_ns.store(UNKNOWN, Relaxed);
        shared.rate_bits.store(1.0f64.to_bits(), Relaxed);
        shared.loop_a.store(UNKNOWN, Relaxed);
        shared.loop_b.store(UNKNOWN, Relaxed);
        let (tx, rx) = channel();
        let sh = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("jizzcraft-player".into())
            .spawn(move || control_thread(input, volume, sh, rx, wake))
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

    /// Playback speed, clamped to 0.25..=4.0. Audio keeps its pitch
    /// (`scaletempo`) when that element is installed.
    pub fn set_rate(&self, rate: f64) {
        let r = if rate.is_finite() {
            rate.clamp(0.25, 4.0)
        } else {
            1.0
        };
        self.shared.rate_bits.store(r.to_bits(), Relaxed);
        self.send(Cmd::Rate(r));
    }

    pub fn rate(&self) -> f64 {
        f64::from_bits(self.shared.rate_bits.load(Relaxed))
    }

    /// One frame forward (a real GStreamer step event) or back (pause plus an
    /// accurate seek of one frame period; `fps_hint` ≤ 0 falls back to 25).
    pub fn step_frame(&self, forward: bool, fps_hint: f32) {
        self.send(Cmd::Step {
            forward,
            fps: fps_hint,
        });
    }

    /// A-B loop. `a` alone loops from `a` to the end; `b` alone loops from the
    /// start to `b`; `None, None` clears the loop.
    pub fn set_loop(&self, a: Option<Duration>, b: Option<Duration>) {
        let ns = |d: Option<Duration>| d.map_or(UNKNOWN, |d| d.as_nanos() as u64);
        let (a, b) = (ns(a), ns(b));
        self.shared.loop_a.store(a, Relaxed);
        self.shared.loop_b.store(b, Relaxed);
        self.send(Cmd::Loop);
    }

    pub fn loop_points(&self) -> (Option<Duration>, Option<Duration>) {
        let get = |v: u64| (v != UNKNOWN).then(|| Duration::from_nanos(v));
        (
            get(self.shared.loop_a.load(Relaxed)),
            get(self.shared.loop_b.load(Relaxed)),
        )
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
    /// Set by `guard::watch` when an element that could reach the network
    /// was refused.
    refused: Arc<AtomicBool>,
    /// Torn down after a refusal: only `Stop` is handled from then on.
    dead: bool,
    seek_in_flight: bool,
    pending_seek: Option<(u64, bool)>,
    /// What the user asked for (as opposed to a temporary buffering pause).
    want_playing: bool,
    buffering_paused: bool,
    is_live: bool,
    last_target_ns: u64,
    rate: f64,
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
        // `seek_simple` would reset the rate to 1.0; always carry ours.
        if self
            .pipeline
            .seek(
                self.rate,
                flags,
                gst::SeekType::Set,
                pos,
                gst::SeekType::None,
                gst::ClockTime::NONE,
            )
            .is_ok()
        {
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
        if self.dead {
            return !matches!(cmd, Cmd::Stop);
        }
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
            Cmd::Rate(r) => {
                self.rate = r;
                // A flushing seek to the current position applies the new rate.
                let cur = self.current_ns();
                self.request_seek(cur, true);
            }
            Cmd::Step { forward, fps } => {
                // Stepping always happens paused.
                self.want_playing = false;
                let _ = self.pipeline.set_state(gst::State::Paused);
                let _ = self.pipeline.state(gst::ClockTime::from_mseconds(250));
                if self.shared.state.load(Relaxed) != PlayState::Ended as u8 {
                    self.set_state(PlayState::Paused);
                }
                if forward {
                    let ev =
                        gst::event::Step::new(gst::format::Buffers::from_u64(1), 1.0, true, false);
                    let _ = self.pipeline.send_event(ev);
                } else {
                    let fps = if fps > 1.0 { fps as f64 } else { 25.0 };
                    let one = (1_000_000_000.0 / fps) as u64;
                    let target = self.current_ns().saturating_sub(one);
                    self.request_seek(target, true);
                }
            }
            Cmd::Loop => {}
            Cmd::Stop => return false,
        }
        true
    }

    /// A-B loop enforcement, called from the control loop.
    fn enforce_loop(&mut self) {
        let a = self.shared.loop_a.load(Relaxed);
        let b = self.shared.loop_b.load(Relaxed);
        if b == UNKNOWN || self.seek_in_flight {
            return;
        }
        if self.shared.state.load(Relaxed) != PlayState::Playing as u8 {
            return;
        }
        if let Some(p) = self.pipeline.query_position::<gst::ClockTime>()
            && p.nseconds() >= b
        {
            let to = if a == UNKNOWN || a >= b { 0 } else { a };
            self.request_seek(to, true);
        }
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
            MessageView::StepDone(_) => {
                if let Some(p) = self.pipeline.query_position::<gst::ClockTime>() {
                    self.shared.position_ns.store(p.nseconds(), Relaxed);
                }
                (self.wake)();
            }
            MessageView::Eos(_) if self.shared.loop_a.load(Relaxed) != UNKNOWN => {
                // `a` set and no `b` before the end: loop A -> end.
                let a = self.shared.loop_a.load(Relaxed);
                self.want_playing = true;
                self.request_seek(a, true);
                let _ = self.pipeline.set_state(gst::State::Playing);
                self.set_state(PlayState::Playing);
            }
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
                let text = if self.refused.load(Relaxed) {
                    crate::guard::REFUSED.to_owned()
                } else {
                    known.unwrap_or_else(|| {
                        let mut t = format!("playback error: {}", e.error());
                        if let Some(d) = e.debug() {
                            crate::debug_detail(&mut t, &d);
                        }
                        t
                    })
                };
                if text == crate::guard::REFUSED {
                    // Stop everything now; this is the control thread.
                    let _ = self.pipeline.set_state(gst::State::Null);
                    self.dead = true;
                }
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

/// A pipeline, its refusal flag and (for streams) the feeder to stop last.
type Built = (
    gst::Element,
    Arc<AtomicBool>,
    Option<Arc<Mutex<Option<Feeder>>>>,
);

fn build_pipeline(
    input: Input,
    volume: f64,
    shared: &Arc<Shared>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Built, String> {
    crate::init()?;
    let (uri, stream) = match input {
        Input::File(path) => (crate::file_uri(&path)?, None),
        Input::Stream { reader, len } => ("appsrc://".to_owned(), Some((reader, len))),
    };
    let mk = |name: &str, hint: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("missing GStreamer element `{name}` — {hint}"))
    };

    let playbin = mk("playbin", "install gst-plugins-base")?;
    let bin_ref = playbin
        .downcast_ref::<gst::Bin>()
        .ok_or("playbin is not a bin")?;
    // Only a stream pipeline may contain an appsrc; fetchers stay refused.
    let refused = if stream.is_some() {
        crate::guard::watch_with(bin_ref, &["appsrc"])
    } else {
        crate::guard::watch(bin_ref)
    };
    playbin.set_property("uri", &uri);
    let feeder_slot = stream.map(|(reader, len)| {
        let slot: Arc<Mutex<Option<Feeder>>> = Arc::new(Mutex::new(None));
        let (s, reader) = (Arc::clone(&slot), Mutex::new(Some(reader)));
        playbin.connect("source-setup", false, move |args| {
            let el = args.get(1)?.get::<gst::Element>().ok()?;
            let src = el.downcast::<gst_app::AppSrc>().ok()?;
            if let Some(r) = reader.lock().unwrap().take() {
                *s.lock().unwrap() = Some(crate::stream::start(&src, r, len));
            }
            None
        });
        slot
    });
    playbin.set_property("volume", volume);
    // Keep pitch at non-1.0 rates. Optional: absent plugin = pitch shifts.
    if let Ok(st) = gst::ElementFactory::make("scaletempo").build() {
        playbin.set_property("audio-filter", &st);
    }

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
    Ok((playbin, refused, feeder_slot))
}

/// How long a stream may show nothing at all before it is called broken.
const STREAM_LOAD_LIMIT: Duration = Duration::from_secs(20);
/// How long the first bytes of a stream may take to arrive.
const SNIFF_LIMIT: Duration = Duration::from_secs(30);

/// Read the first bytes of a stream, waiting on `WouldBlock` (shown as
/// buffering) and honouring `Stop`. `None` = the player was dropped.
fn sniff_head(
    reader: &mut dyn ReadSeek,
    rx: &Receiver<Cmd>,
    shared: &Shared,
) -> Option<Result<Vec<u8>, String>> {
    use std::io::{ErrorKind, SeekFrom};
    let mut head = vec![0u8; 4096];
    let mut got = 0;
    let start = Instant::now();
    shared.buffering.store(0, Relaxed);
    while got < head.len() {
        match reader.read(&mut head[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                if start.elapsed() > SNIFF_LIMIT {
                    return Some(Err("could not start this video".into()));
                }
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(Cmd::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return None;
                    }
                    _ => {}
                }
            }
            Err(_) => return Some(Err("the download stopped being readable".into())),
        }
        // A tiny file ends before 4 KiB: what we have is the whole head.
        if got > 0 && got < head.len() && start.elapsed() > Duration::from_millis(200) {
            break;
        }
    }
    shared.buffering.store(100, Relaxed);
    head.truncate(got);
    if reader.seek(SeekFrom::Start(0)).is_err() {
        return Some(Err("the download stopped being readable".into()));
    }
    Some(Ok(head))
}

fn control_thread(
    mut input: Input,
    volume: f64,
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    shared.buffering.store(100, Relaxed);
    let is_stream = matches!(input, Input::Stream { .. });
    if let Input::Stream { reader, .. } = &mut input {
        // Refuse a playlist before any pipeline exists.
        let verdict = match sniff_head(&mut **reader, &rx, &shared) {
            None => return,
            Some(Ok(head)) if crate::guard::looks_like_manifest(&head) => {
                Some(crate::guard::REFUSED.to_owned())
            }
            Some(Ok(_)) => None,
            Some(Err(e)) => Some(e),
        };
        if let Some(e) = verdict {
            *shared.error.lock().unwrap() = Some(e);
            shared.state.store(PlayState::Error as u8, Relaxed);
            wake();
            while rx.recv().is_ok_and(|c| !matches!(c, Cmd::Stop)) {}
            return;
        }
    }
    let loading_since = Instant::now();
    let (pipeline, refused, feeder) = match build_pipeline(input, volume, &shared, &wake) {
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
        refused,
        dead: false,
        seek_in_flight: false,
        pending_seek: None,
        want_playing: true,
        buffering_paused: false,
        is_live: false,
        last_target_ns: 0,
        rate: 1.0,
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
        ctl.enforce_loop();
        // A stream that never starts must not sit on "Loading" forever.
        if is_stream
            && !ctl.dead
            && shared.state.load(Relaxed) == PlayState::Loading as u8
            && shared.buffering.load(Relaxed) >= 100
            && loading_since.elapsed() > STREAM_LOAD_LIMIT
        {
            ctl.fail("could not start this video".into());
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
    // Teardown off the UI thread. The feeder is stopped and joined after the
    // pipeline is Null, so nothing pushes into a dead appsrc.
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(slot) = feeder {
        drop(slot.lock().unwrap().take());
    }
}
