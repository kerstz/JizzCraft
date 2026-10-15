//! Background thumbnail service: std threads + a LIFO queue the UI re-ranks
//! every frame, so scrolling never processes stale (off-screen) requests.

use crate::thumbs::{self, ThumbStore};
use image::RgbImage;
use jizzcraft_core::vault::ThumbKeys;
use jizzcraft_core::{MediaKind, MediaMeta};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Counts the live worker threads of every service it is given to. The owner
/// of the library waits for [`Workers::quiesced`] before it converts, drops
/// the keys or shows the lock screen: a detached worker that is still
/// decoding could otherwise write a clear thumbnail after the clean-up.
#[derive(Clone, Debug, Default)]
pub struct Workers(Arc<AtomicUsize>);

impl Workers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Worker threads still running (also while they only wait for a job).
    pub fn live(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    /// Every worker has returned and released its cipher.
    pub fn quiesced(&self) -> bool {
        self.live() == 0
    }

    /// Block until [`Workers::quiesced`] or `timeout`. For tests and tools;
    /// the UI thread polls `quiesced` instead.
    pub fn wait_quiesced(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while !self.quiesced() {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    pub(crate) fn enter(&self) -> WorkerToken {
        self.0.fetch_add(1, Ordering::SeqCst);
        WorkerToken(Arc::clone(&self.0))
    }
}

/// Held by a worker thread for its whole life; released also on a panic.
pub(crate) struct WorkerToken(Arc<AtomicUsize>);

impl Drop for WorkerToken {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Produces the thumbnail image of a request. The default decodes the file;
/// tests inject a slow or counting one.
pub type Decoder = Arc<dyn Fn(&Request) -> Result<RgbImage, String> + Send + Sync>;

fn default_decoder() -> Decoder {
    Arc::new(|req| match req.kind {
        MediaKind::Image => thumbs::image_thumb(&req.path),
        MediaKind::Video => thumbs::video_thumb(&req.path),
    })
}

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
    /// Raised by `shutdown`, read without the lock before every cache write.
    stop: AtomicBool,
}

pub struct ThumbService {
    shared: Arc<Shared>,
    rx: Receiver<Done>,
    workers: Workers,
}

impl ThumbService {
    /// `wake` is called from worker threads whenever a result is ready.
    pub fn new(cache_root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_keys(cache_root, None, wake)
    }

    /// Like `new`; with `keys` (vault mode) the cache is encrypted under
    /// `vault-thumbs/` and nothing is written in clear.
    pub fn with_keys(
        cache_root: PathBuf,
        keys: Option<Arc<ThumbKeys>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::tracked(cache_root, keys, wake, &Workers::new())
    }

    /// Like `with_keys`; the worker threads are counted in `workers`, which
    /// the caller shares with other services and waits on.
    pub fn tracked(
        cache_root: PathBuf,
        keys: Option<Arc<ThumbKeys>>,
        wake: Arc<dyn Fn() + Send + Sync>,
        workers: &Workers,
    ) -> Self {
        Self::with_decoder(cache_root, keys, wake, workers, default_decoder())
    }

    /// Like `tracked`, with the decoder replaced (tests).
    #[doc(hidden)]
    pub fn with_decoder(
        cache_root: PathBuf,
        keys: Option<Arc<ThumbKeys>>,
        wake: Arc<dyn Fn() + Send + Sync>,
        workers: &Workers,
        decoder: Decoder,
    ) -> Self {
        let store = ThumbStore::new(cache_root, keys);
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (tx, rx) = channel();
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        let image_workers = cpus.saturating_sub(1).clamp(1, 6);
        let ctx = WorkerCtx {
            shared: Arc::clone(&shared),
            tx,
            wake,
            store,
            decoder,
        };
        for _ in 0..image_workers {
            spawn_worker(&ctx, workers, MediaKind::Image);
        }
        for _ in 0..2 {
            spawn_worker(&ctx, workers, MediaKind::Video);
        }
        ThumbService {
            shared,
            rx,
            workers: workers.clone(),
        }
    }

    /// Replace the queue with what is on screen now, most important first.
    /// Requests already running are left alone.
    pub fn set_wanted(&self, wanted: Vec<Request>) {
        let mut st = self.shared.state.lock().unwrap();
        if st.shutdown {
            return;
        }
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

    /// Stop taking work and let the workers go: nothing more is read from the
    /// queue and no thumbnail is written after this call (a decode already in
    /// flight finishes, then is discarded). Does not block.
    pub fn shutdown(&self) {
        let mut st = self.shared.state.lock().unwrap();
        st.shutdown = true;
        st.pending.clear();
        drop(st);
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.cv.notify_all();
    }

    /// Every worker of this service (and of the services sharing its
    /// [`Workers`]) has returned. Only true after [`ThumbService::shutdown`]
    /// or a drop, once the decodes in flight have ended.
    pub fn quiesced(&self) -> bool {
        self.workers.quiesced()
    }

    pub fn workers(&self) -> &Workers {
        &self.workers
    }
}

impl Drop for ThumbService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What every worker thread shares.
#[derive(Clone)]
struct WorkerCtx {
    shared: Arc<Shared>,
    tx: Sender<Done>,
    wake: Arc<dyn Fn() + Send + Sync>,
    store: ThumbStore,
    decoder: Decoder,
}

fn spawn_worker(ctx: &WorkerCtx, workers: &Workers, kind: MediaKind) {
    let ctx = ctx.clone();
    let token = workers.enter();
    let spawned = std::thread::Builder::new()
        .name("jizzcraft-thumbs".into())
        .spawn(move || {
            run_worker(ctx, kind);
            // Last: the store (and its cipher) is gone by now.
            drop(token);
        });
    // No thread, no worker: the token went with the closure and the count
    // stays right.
    drop(spawned);
}

fn run_worker(ctx: WorkerCtx, kind: MediaKind) {
    let WorkerCtx {
        shared,
        tx,
        wake,
        store,
        decoder,
    } = ctx;
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
        let done = process(&store, &decoder, &shared.stop, &req);
        shared.state.lock().unwrap().inflight.remove(&req.id);
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        if tx.send(done).is_err() {
            return;
        }
        wake();
    }
}

fn to_rgba(img: &image::RgbImage) -> Rgba {
    let (w, h) = img.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for p in img.pixels() {
        data.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Rgba { w, h, data }
}

fn process(store: &ThumbStore, decoder: &Decoder, stop: &AtomicBool, req: &Request) -> Done {
    let id = store.thumb_id(&req.path, req.size, req.mtime);
    let file = store.thumb_path(&id);
    let meta = if req.kind == MediaKind::Video && req.need_meta {
        crate::probe::probe(&req.path).ok()
    } else {
        None
    };
    let result = (|| {
        if let Some(img) = store.load(&file, &id) {
            return Ok(img);
        }
        let img = decoder(req)?;
        // Once stopped (lock, conversion, exit) nothing may reach the cache:
        // the owner is cleaning it, or has just done so. A cache write
        // failure must not hide a thumbnail we already have.
        if !stop.load(Ordering::SeqCst) {
            let _ = store.save(&file, &id, &img);
        }
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
