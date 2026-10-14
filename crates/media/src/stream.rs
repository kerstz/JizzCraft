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
/// How long the reader may have nothing to give before playback is paused.
const STALL_AFTER: Duration = Duration::from_millis(300);
/// New data needed after a stall before playback resumes.
const RESUME_BYTES: u64 = 2 * 1024 * 1024;
/// Poll interval while waiting for data or for the appsrc to want more.
const TICK: Duration = Duration::from_millis(20);

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
pub(crate) fn start(src: &gst_app::AppSrc, mut reader: Box<dyn ReadSeek>, len: u64) -> Feeder {
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
        .spawn(move || feed(&app, &mut *reader, len, &c))
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
            feed_timed(&app, &mut *source, first, &stats, &c);
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
                let since = *idle_since.get_or_insert_with(Instant::now);
                if !stalled && since.elapsed() >= STALL_AFTER {
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

fn feed(app: &gst_app::AppSrc, reader: &mut dyn ReadSeek, len: u64, ctl: &Ctl) {
    let mut pos: u64 = 0;
    let mut stalled = false;
    let mut idle_since: Option<Instant> = None;
    let mut since_stall: u64 = 0;
    let mut buf = vec![0u8; CHUNK];

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
                idle_since = None;
                if stalled {
                    since_stall += n as u64;
                    if since_stall >= RESUME_BYTES || pos >= len {
                        stalled = false;
                        post_buffering(app, 100);
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if !stalled && since.elapsed() >= STALL_AFTER {
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
