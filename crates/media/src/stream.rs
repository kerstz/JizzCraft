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

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Handle on the running feeder; dropping it stops and joins the thread.
pub(crate) struct Feeder {
    ctl: Arc<Ctl>,
    handle: Option<JoinHandle<()>>,
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
