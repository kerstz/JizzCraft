//! Background thumbnail service: std threads + a LIFO queue the UI re-ranks
//! every frame, so scrolling never processes stale (off-screen) requests.

use crate::thumbs;
use jizzcraft_core::{MediaKind, MediaMeta};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone)]
pub struct Request {
    pub id: i64,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub size: u64,
    pub mtime: i64,
    /// Probe duration/resolution/codec too (videos missing metadata).
    pub need_meta: bool,
}

pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

pub struct Thumb {
    pub full: Rgba,
    pub blur: Rgba,
}

pub struct Done {
    pub id: i64,
    pub thumb: Result<Thumb, String>,
    pub meta: Option<MediaMeta>,
}

#[derive(Default)]
struct State {
    /// Last element = most important. The last shall be first; it is a LIFO.
    pending: Vec<Request>,
    inflight: HashSet<i64>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

pub struct ThumbService {
    shared: Arc<Shared>,
    rx: Receiver<Done>,
}

impl ThumbService {
    /// `wake` is called from worker threads whenever a result is ready.
    pub fn new(cache_root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            cv: Condvar::new(),
        });
        let (tx, rx) = channel();
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        let image_workers = cpus.saturating_sub(1).clamp(1, 6);
        for _ in 0..image_workers {
            spawn_worker(&shared, &tx, &wake, &cache_root, MediaKind::Image);
        }
        for _ in 0..2 {
            spawn_worker(&shared, &tx, &wake, &cache_root, MediaKind::Video);
        }
        ThumbService { shared, rx }
    }

    /// Replace the queue with what is on screen now, most important first.
    /// Requests already running are left alone.
    pub fn set_wanted(&self, wanted: Vec<Request>) {
        let mut st = self.shared.state.lock().unwrap();
        let inflight = st.inflight.clone();
        st.pending = wanted
            .into_iter()
            .rev()
            .filter(|r| !inflight.contains(&r.id))
            .collect();
        drop(st);
        self.shared.cv.notify_all();
    }

    pub fn try_recv(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }

    pub fn is_busy(&self) -> bool {
        let st = self.shared.state.lock().unwrap();
        !st.pending.is_empty() || !st.inflight.is_empty()
    }
}

impl Drop for ThumbService {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().shutdown = true;
        self.shared.cv.notify_all();
    }
}

fn spawn_worker(
    shared: &Arc<Shared>,
    tx: &Sender<Done>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    cache_root: &std::path::Path,
    kind: MediaKind,
) {
    let (shared, tx, wake, root) = (
        Arc::clone(shared),
        tx.clone(),
        Arc::clone(wake),
        cache_root.to_path_buf(),
    );
    std::thread::spawn(move || {
        loop {
            let req = {
                let mut st = shared.state.lock().unwrap();
                loop {
                    if st.shutdown {
                        return;
                    }
                    if let Some(pos) = st.pending.iter().rposition(|r| r.kind == kind) {
                        let r = st.pending.remove(pos);
                        st.inflight.insert(r.id);
                        break r;
                    }
                    st = shared.cv.wait(st).unwrap();
                }
            };
            let done = process(&root, &req);
            shared.state.lock().unwrap().inflight.remove(&req.id);
            if tx.send(done).is_err() {
                return;
            }
            wake();
        }
    });
}

fn to_rgba(img: &image::RgbImage) -> Rgba {
    let (w, h) = img.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for p in img.pixels() {
        data.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Rgba { w, h, data }
}

fn process(root: &std::path::Path, req: &Request) -> Done {
    let key = thumbs::cache_key(&req.path, req.size, req.mtime);
    let file = thumbs::thumb_file(root, &key);
    let meta = if req.kind == MediaKind::Video && req.need_meta {
        crate::probe::probe(&req.path).ok()
    } else {
        None
    };
    let result = (|| {
        if let Some(img) = thumbs::load_cached(&file) {
            return Ok(img);
        }
        let img = match req.kind {
            MediaKind::Image => thumbs::image_thumb(&req.path)?,
            MediaKind::Video => thumbs::video_thumb(&req.path)?,
        };
        // A cache write failure must not hide a thumbnail we already have.
        let _ = thumbs::save_cached(&file, &img);
        Ok::<_, String>(img)
    })();
    let thumb = result.map(|img| Thumb {
        blur: to_rgba(&thumbs::blurred(&img)),
        full: to_rgba(&img),
    });
    Done {
        id: req.id,
        thumb,
        meta,
    }
}
