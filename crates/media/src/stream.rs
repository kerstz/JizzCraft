//! Feeds a player from an in-process reader (`Player::open_stream`).
//!
//! The bytes come from a local source that is still being filled, such as a
//! torrent being downloaded to disk. No URL and no socket is involved: the
//! pipeline's `appsrc` is fed by a thread owned by the player's control
//! thread, which pulls from a `Read + Seek` handle.
//!
//! The reader contract: `ErrorKind::WouldBlock` means "those bytes are not
//! here yet". It is never end of stream; EOS is sent only when `len` bytes
//! have been delivered. While the reader would block the feeder reports
//! `Buffering` so the player pauses instead of failing.
//!
//! `WouldBlock` is not a no. It is a "not yet", delivered with dignity.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use jizzcraft_core::timed::{Chunk, TimedSource};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Anything the player can stream from.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// Bytes the appsrc may queue before it asks the feeder to stop.
const QUEUE_BYTES: u64 = 4 * 1024 * 1024;
/// Largest single read.
const CHUNK: usize = 64 * 1024;
/// How long a read request may stay unanswered before it can count as
/// starvation.
const STALL_AFTER: Duration = Duration::from_millis(300);
/// How long no frame may have been delivered before starvation is believed.
const FRAME_QUIET: Duration = Duration::from_millis(500);
/// Upper bound on the new data needed after a stall before playback resumes.
const RESUME_BYTES: u64 = 2 * 1024 * 1024;
/// Playback time worth of new data that is enough to resume after a stall.
const RESUME_SECS: f64 = 2.0;
/// Poll interval while waiting for data or for the appsrc to want more.
const TICK: Duration = Duration::from_millis(20);

/// Shared between the control thread (which owns the pipeline state) and the
/// feeder threads. Everything here is advisory: the feeders only read it to
/// decide whether downstream is really starving.
pub(crate) struct Health {
    /// Frames the sink delivered to the mailbox.
    pub frames: AtomicU64,
    /// Bytes (or timed chunks' bytes) the feeders pushed into the appsrc.
    pub bytes: AtomicU64,
    /// Media duration in ns; 0 or `u64::MAX` when unknown.
    pub duration_ns: AtomicU64,
    /// The player is past its first preroll and the user wants playback.
    pub playing: AtomicBool,
}

impl Default for Health {
    fn default() -> Self {
        Health {
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            duration_ns: AtomicU64::new(u64::MAX),
            playing: AtomicBool::new(false),
        }
    }
}

impl Health {
    fn duration(&self) -> Option<Duration> {
        match self.duration_ns.load(Ordering::Relaxed) {
            0 | u64::MAX => None,
            n => Some(Duration::from_nanos(n)),
        }
    }
}

/// New bytes needed after a stall before playback resumes: about `secs` of
/// media when the length and duration are known, never more than
/// [`RESUME_BYTES`] and never less than one chunk (or the whole file, for a
/// file smaller than that). Without a duration a quarter of the file stands in.
pub(crate) fn resume_threshold(len: u64, duration: Option<Duration>, secs: f64) -> u64 {
    let by_time = match duration {
        Some(d) if d.as_secs_f64() > 0.0 => (len as f64 * secs / d.as_secs_f64()) as u64,
        _ => len / 4,
    };
    by_time.min(RESUME_BYTES).max(CHUNK as u64).min(len.max(1))
}

#[derive(Default)]
struct State {
    /// The appsrc asked for data and has not been given a chunk yet.
    want: bool,
    seek_to: Option<u64>,
    /// Bumped by every `seek-data`; a chunk read under an older value is
    /// dropped, so stale bytes never answer a newer offset.
    generation: u64,
}

struct Ctl {
    state: Mutex<State>,
    cv: Condvar,
    stop: AtomicBool,
}

type SourceSlot = Arc<Mutex<Option<Box<dyn TimedSource>>>>;

/// Handle on the running feeder; dropping it stops and joins the thread.
pub(crate) struct Feeder {
    ctl: Arc<Ctl>,
    handle: Option<JoinHandle<()>>,
    /// Timed feeders hand their source back here when they exit.
    back: Option<SourceSlot>,
}

impl Feeder {
    /// Stop the feeder, join it and give the timed source back (`None` for a
    /// reader feeder, or when the thread died).
    pub(crate) fn into_source(mut self) -> Option<Box<dyn TimedSource>> {
        self.ctl.stop.store(true, Ordering::SeqCst);
        self.ctl.cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let back = self.back.take()?;
        back.lock().unwrap().take()
    }
}

impl Drop for Feeder {
    fn drop(&mut self) {
        self.ctl.stop.store(true, Ordering::SeqCst);
        self.ctl.cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Configure `src` for random access over `len` bytes and start feeding it.
pub(crate) fn start(
    src: &gst_app::AppSrc,
    mut reader: Box<dyn ReadSeek>,
    len: u64,
    health: Arc<Health>,
) -> Feeder {
    src.set_stream_type(gst_app::AppStreamType::RandomAccess);
    src.set_format(gst::Format::Bytes);
    src.set_size(len as i64);
    src.set_max_bytes(QUEUE_BYTES);

    let ctl = Arc::new(Ctl {
        state: Mutex::new(State::default()),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
    });
    let (c1, c2, c3) = (Arc::clone(&ctl), Arc::clone(&ctl), Arc::clone(&ctl));
    src.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |_, _| {
                c1.state.lock().unwrap().want = true;
                c1.cv.notify_all();
            })
            .enough_data(move |_| {
                c2.state.lock().unwrap().want = false;
            })
            .seek_data(move |_, off| {
                let mut s = c3.state.lock().unwrap();
                s.seek_to = Some(off);
                s.generation += 1;
                // The pull-mode source asks for data right after a seek.
                s.want = true;
                c3.cv.notify_all();
                true
            })
            .build(),
    );

    let app = src.clone();
    let c = Arc::clone(&ctl);
    let handle = std::thread::Builder::new()
        .name("jizzcraft-stream".into())
        .spawn(move || feed(&app, &mut *reader, len, &c, &health))
        .expect("spawn stream feeder");
    Feeder {
        ctl,
        handle: Some(handle),
        back: None,
    }
}

/// What the timed feeder tells the control thread. `u64::MAX` is "unknown".
pub(crate) struct TimedStats {
    pub buffered_ns: AtomicU64,
    pub variant: AtomicU64,
    /// The source asked for a lower variant (one-shot, taken by the player).
    pub downgrade: AtomicBool,
}

impl Default for TimedStats {
    fn default() -> Self {
        TimedStats {
            buffered_ns: AtomicU64::new(u64::MAX),
            variant: AtomicU64::new(u64::MAX),
            downgrade: AtomicBool::new(false),
        }
    }
}

/// Configure `src` for timed chunks and start feeding it from `source`.
/// `first` is a chunk the caller already pulled (to sniff it).
/// `duration == None` is a live stream: no seeking.
pub(crate) fn start_timed(
    src: &gst_app::AppSrc,
    source: Box<dyn TimedSource>,
    first: Option<Chunk>,
    stats: Arc<TimedStats>,
    health: Arc<Health>,
) -> Feeder {
    let duration = source.duration();
    src.set_format(gst::Format::Time);
    src.set_max_bytes(QUEUE_BYTES);
    match duration {
        Some(d) => {
            src.set_stream_type(gst_app::AppStreamType::Seekable);
            src.set_duration(gst::ClockTime::from_nseconds(d.as_nanos() as u64));
        }
        None => src.set_stream_type(gst_app::AppStreamType::Stream),
    }
    let ctl = Arc::new(Ctl {
        state: Mutex::new(State::default()),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
    });
    let (c1, c2, c3) = (Arc::clone(&ctl), Arc::clone(&ctl), Arc::clone(&ctl));
    src.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |_, _| {
                c1.state.lock().unwrap().want = true;
                c1.cv.notify_all();
            })
            .enough_data(move |_| {
                c2.state.lock().unwrap().want = false;
            })
            .seek_data(move |_, ns| {
                let mut s = c3.state.lock().unwrap();
                s.seek_to = Some(ns);
                s.generation += 1;
                s.want = true;
                c3.cv.notify_all();
                true
            })
            .build(),
    );
    let back: SourceSlot = Arc::new(Mutex::new(None));
    let (app, c, b) = (src.clone(), Arc::clone(&ctl), Arc::clone(&back));
    let handle = std::thread::Builder::new()
        .name("jizzcraft-timed".into())
        .spawn(move || {
            let mut source = source;
            feed_timed(&app, &mut *source, first, &stats, &c, &health);
            *b.lock().unwrap() = Some(source);
        })
        .expect("spawn timed feeder");
    Feeder {
        ctl,
        handle: Some(handle),
        back: Some(back),
    }
}

fn feed_timed(
    app: &gst_app::AppSrc,
    source: &mut dyn TimedSource,
    mut first: Option<Chunk>,
    stats: &TimedStats,
    ctl: &Ctl,
    health: &Health,
) {
    let mut stalled = false;
    let mut idle_since: Option<Instant> = None;
    let mut after_seek = false;
    while !ctl.stop.load(Ordering::SeqCst) {
        let (want, seek, gen_before) = {
            let mut s = ctl.state.lock().unwrap();
            if !s.want && s.seek_to.is_none() {
                let (g, _) = ctl.cv.wait_timeout(s, TICK).unwrap();
                s = g;
            }
            (s.want, s.seek_to.take(), s.generation)
        };
        if let Some(ns) = seek {
            // The pre-pulled chunk belongs to the old position.
            first = None;
            source.seek(Duration::from_nanos(ns));
            after_seek = true;
            idle_since = None;
        }
        if !want {
            continue;
        }
        let pulled = match first.take() {
            Some(c) => Ok(Some(c)),
            None => source.pull(),
        };
        if let Some(b) = source.buffered_until() {
            stats
                .buffered_ns
                .store(b.as_nanos() as u64, Ordering::Relaxed);
        }
        let v = source.current_variant().map_or(u64::MAX, |v| v as u64);
        stats.variant.store(v, Ordering::Relaxed);
        if source.wants_downgrade() {
            stats.downgrade.store(true, Ordering::SeqCst);
        }
        match pulled {
            Ok(Some(chunk)) => {
                {
                    let mut s = ctl.state.lock().unwrap();
                    if s.generation != gen_before {
                        continue; // stale: a newer seek is waiting
                    }
                    s.want = false;
                }
                let head = &chunk.bytes[..chunk.bytes.len().min(4096)];
                if crate::guard::looks_like_manifest(head) {
                    let _ = app.post_message(
                        gst::message::Error::builder(
                            gst::ResourceError::NotAuthorized,
                            crate::guard::REFUSED,
                        )
                        .build(),
                    );
                    return;
                }
                let n = chunk.bytes.len() as u64;
                let mut b = gst::Buffer::from_slice(chunk.bytes);
                {
                    let r = b.get_mut().expect("fresh buffer");
                    if let Some(p) = chunk.pts {
                        r.set_pts(gst::ClockTime::from_nseconds(p.as_nanos() as u64));
                    }
                    if chunk.discont || after_seek {
                        r.set_flags(gst::BufferFlags::DISCONT);
                    }
                }
                after_seek = false;
                if app.push_buffer(b).is_err() {
                    return;
                }
                health.bytes.fetch_add(n, Ordering::Relaxed);
                idle_since = None;
                if stalled {
                    stalled = false;
                    post_buffering(app, 100);
                }
            }
            Ok(None) => {
                let _ = app.end_of_stream();
                ctl.state.lock().unwrap().want = false;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                // Push mode: starving means a request is outstanding (`want`
                // was true to get here) while the appsrc queue is empty.
                let since = *idle_since.get_or_insert_with(Instant::now);
                if !stalled && since.elapsed() >= STALL_AFTER && app.current_level_bytes() == 0 {
                    stalled = true;
                    post_buffering(app, 0);
                }
                std::thread::sleep(TICK);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => {
                fail(app);
                return;
            }
        }
    }
}

fn post_buffering(app: &gst_app::AppSrc, pct: i32) {
    let _ = app.post_message(gst::message::Buffering::new(pct));
}

fn feed(app: &gst_app::AppSrc, reader: &mut dyn ReadSeek, len: u64, ctl: &Ctl, health: &Health) {
    let mut pos: u64 = 0;
    let mut stalled = false;
    let mut idle_since: Option<Instant> = None;
    let mut since_stall: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    // When the sink last delivered a new frame, as seen from here.
    let mut seen_frames = health.frames.load(Ordering::Relaxed);
    let mut frame_at = Instant::now();

    while !ctl.stop.load(Ordering::SeqCst) {
        let (want, seek, gen_before) = {
            let mut s = ctl.state.lock().unwrap();
            if !s.want && s.seek_to.is_none() {
                let (g, _) = ctl.cv.wait_timeout(s, TICK).unwrap();
                s = g;
            }
            (s.want, s.seek_to.take(), s.generation)
        };
        if let Some(off) = seek {
            // A seek re-arms everything: the new region may be missing.
            if reader.seek(SeekFrom::Start(off)).is_err() {
                fail(app);
                return;
            }
            pos = off;
            idle_since = None;
        }
        let frames = health.frames.load(Ordering::Relaxed);
        if frames != seen_frames {
            seen_frames = frames;
            frame_at = Instant::now();
        }
        if !want {
            continue;
        }
        if pos >= len {
            // Safe to repeat: a source that was seeked clears its EOS flag.
            let _ = app.end_of_stream();
            ctl.state.lock().unwrap().want = false;
            continue;
        }
        let max = CHUNK.min((len - pos) as usize);
        match reader.read(&mut buf[..max]) {
            Ok(0) => std::thread::sleep(TICK),
            Ok(n) => {
                {
                    // A seek that arrived while reading makes this chunk stale.
                    let mut s = ctl.state.lock().unwrap();
                    if s.generation != gen_before {
                        // Re-seek the reader at the newest request next turn.
                        continue;
                    }
                    // Exactly one chunk per request.
                    s.want = false;
                }
                let mut b = gst::Buffer::from_slice(buf[..n].to_vec());
                b.get_mut().expect("fresh buffer").set_offset(pos);
                if app.push_buffer(b).is_err() {
                    return; // flushing or torn down
                }
                pos += n as u64;
                health.bytes.fetch_add(n as u64, Ordering::Relaxed);
                idle_since = None;
                if stalled {
                    since_stall += n as u64;
                    let dur = health.duration();
                    // The blocked request was served; resume on ~2 s of new
                    // data (capped at RESUME_BYTES, so small files resume at
                    // once) or at the end of the file.
                    if since_stall >= resume_threshold(len, dur, RESUME_SECS) || pos >= len {
                        stalled = false;
                        post_buffering(app, 100);
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                // Pull mode: starving means this request has been blocked for
                // STALL_AFTER while the player is playing and the sink has
                // gone quiet. Before the first preroll (`playing` is false)
                // a blocked read is only "loading": a percentage at most.
                let since = *idle_since.get_or_insert_with(Instant::now);
                if !stalled
                    && since.elapsed() >= STALL_AFTER
                    && health.playing.load(Ordering::Relaxed)
                    && frame_at.elapsed() >= FRAME_QUIET
                {
                    stalled = true;
                    since_stall = 0;
                    post_buffering(app, 0);
                }
                std::thread::sleep(TICK);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => {
                fail(app);
                return;
            }
        }
    }
}

/// No path or name in the text.
fn fail(app: &gst_app::AppSrc) {
    let _ = app.post_message(
        gst::message::Error::builder(
            gst::ResourceError::Read,
            "the download stopped being readable",
        )
        .build(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn resume_threshold_scales_with_the_bitrate() {
        // 100 MiB over 1000 s: 2 s = 205 KiB, well under the cap.
        let t = resume_threshold(100 * MIB, Some(Duration::from_secs(1000)), 2.0);
        assert_eq!(t, (100 * MIB) / 500);
        // A heavy 4 GiB film over 1 h would want 2.3 MiB: capped.
        let t = resume_threshold(4096 * MIB, Some(Duration::from_secs(3600)), 2.0);
        assert_eq!(t, RESUME_BYTES);
    }

    #[test]
    fn resume_threshold_small_and_unknown() {
        // A 60 KB clip never asks for a full chunk it cannot have.
        assert_eq!(
            resume_threshold(60_239, Some(Duration::from_secs(6)), 2.0),
            60_239
        );
        // Unknown duration: a quarter of the file, within [CHUNK, cap].
        assert_eq!(resume_threshold(8 * MIB, None, 2.0), 2 * MIB);
        assert_eq!(resume_threshold(100 * MIB, None, 2.0), RESUME_BYTES);
        assert_eq!(resume_threshold(MIB, None, 2.0), CHUNK as u64 * 4);
        // Zero duration and empty file do not panic or return zero.
        assert!(resume_threshold(0, Some(Duration::ZERO), 2.0) >= 1);
    }
}
