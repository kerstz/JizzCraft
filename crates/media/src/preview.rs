//! Scrub previews: one small frame per time bucket, decoded on demand on its
//! own thread with its own pipeline (never the player's), cached on disk under
//! the XDG cache dir. Idle pipelines are torn down after a while.

use crate::service::{Rgba, Workers};
use crate::thumbs::{self, ThumbStore};
use image::RgbImage;
use jizzcraft_core::vault::ThumbKeys;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PREVIEW_W: u32 = 192;
const SCHEMA: &str = "preview-v1";
const IDLE_TEARDOWN: Duration = Duration::from_secs(20);
const MIN_GAP: Duration = Duration::from_millis(120);

/// Bucket index for `pos` in a clip of `dur`: about 300 buckets, at least 1 s each.
pub fn bucket_for(pos: Duration, dur: Duration) -> u32 {
    let len = bucket_len(dur);
    (pos.as_millis() / len.as_millis().max(1)).min(u32::MAX as u128) as u32
}

pub fn bucket_len(dur: Duration) -> Duration {
    (dur / 300).max(Duration::from_secs(1))
}

pub fn preview_key(path: &Path, size: u64, mtime: i64) -> String {
    let mut h = blake3::Hasher::new();
    h.update(SCHEMA.as_bytes());
    h.update(&jizzcraft_core::pathbytes::path_to_bytes(path));
    h.update(&size.to_le_bytes());
    h.update(&mtime.to_le_bytes());
    h.finalize().to_hex().as_str()[..32].to_owned()
}

pub(crate) fn preview_file(cache_root: &Path, key: &str, bucket: u32) -> PathBuf {
    cache_root
        .join("previews")
        .join(&key[..2])
        .join(key)
        .join(format!("{bucket}.jpg"))
}

pub struct PreviewFrame {
    pub bucket: u32,
    pub full: Rgba,
    /// Tiny version for discreet / share-safe mode.
    pub blur: Rgba,
}

enum Msg {
    Want { bucket: u32, ns: u64 },
    Stop,
}

/// Produces the frame of `(bucket, position in ns)`. The default seeks a
/// GStreamer pipeline; tests inject a slow one.
#[doc(hidden)]
pub type FrameSource = Arc<dyn Fn(u32, u64) -> Result<RgbImage, String> + Send + Sync>;

pub struct PreviewService {
    tx: Sender<Msg>,
    out: Arc<Mutex<Option<PreviewFrame>>>,
    failed: Arc<AtomicBool>,
    /// Raised on drop: checked before every decode and every cache write.
    stop: Arc<AtomicBool>,
    workers: Workers,
}

impl PreviewService {
    /// Spawns the worker thread; nothing is decoded until `request`.
    /// `wake` is called from the worker whenever a frame is ready.
    pub fn new(
        path: PathBuf,
        size: u64,
        mtime: i64,
        cache_root: PathBuf,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::with_keys(path, size, mtime, cache_root, None, wake)
    }

    /// Like `new`; with `keys`, frames are cached encrypted under
    /// `vault-thumbs/` and never written in clear.
    pub fn with_keys(
        path: PathBuf,
        size: u64,
        mtime: i64,
        cache_root: PathBuf,
        keys: Option<Arc<ThumbKeys>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::tracked(
            path,
            size,
            mtime,
            cache_root,
            keys,
            wake,
            &Workers::new(),
            None,
        )
    }

    /// Like `with_keys`; the worker is counted in `workers` (shared with the
    /// thumbnail service and waited on by the owner of the library).
    /// `source` replaces the GStreamer decode (tests).
    #[allow(clippy::too_many_arguments)]
    pub fn tracked(
        path: PathBuf,
        size: u64,
        mtime: i64,
        cache_root: PathBuf,
        keys: Option<Arc<ThumbKeys>>,
        wake: Arc<dyn Fn() + Send + Sync>,
        workers: &Workers,
        source: Option<FrameSource>,
    ) -> Self {
        let store = ThumbStore::new(cache_root, keys);
        let (tx, rx) = channel();
        let out = Arc::new(Mutex::new(None));
        let failed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (o, f, s) = (Arc::clone(&out), Arc::clone(&failed), Arc::clone(&stop));
        let token = workers.enter();
        let spawned = std::thread::Builder::new()
            .name("jizzcraft-preview".into())
            .spawn(move || {
                worker(path, size, mtime, store, rx, o, f, s, wake, source);
                drop(token);
            });
        if spawned.is_err() {
            failed.store(true, Ordering::Relaxed);
        }
        PreviewService {
            tx,
            out,
            failed,
            stop,
            workers: workers.clone(),
        }
    }

    /// Stop decoding and writing; the worker leaves after the decode in
    /// flight. Does not block.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Msg::Stop);
    }

    /// The worker thread (and every one sharing the same [`Workers`]) ended.
    pub fn quiesced(&self) -> bool {
        self.workers.quiesced()
    }

    /// Latest request wins; older ones still queued are skipped.
    pub fn request(&self, bucket: u32, pos: Duration) {
        let _ = self.tx.send(Msg::Want {
            bucket,
            ns: pos.as_nanos() as u64,
        });
    }

    pub fn take(&self) -> Option<PreviewFrame> {
        self.out.lock().unwrap().take()
    }

    /// The pipeline could not be opened (no previews for this file).
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl Drop for PreviewService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn to_rgba(img: &RgbImage) -> Rgba {
    let (w, h) = img.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for p in img.pixels() {
        data.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Rgba { w, h, data }
}

#[allow(clippy::too_many_arguments)]
fn worker(
    path: PathBuf,
    size: u64,
    mtime: i64,
    store: ThumbStore,
    rx: Receiver<Msg>,
    out: Arc<Mutex<Option<PreviewFrame>>>,
    failed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
    source: Option<FrameSource>,
) {
    let stopped = || stop.load(Ordering::SeqCst);
    let mut pipe: Option<thumbs::Paused> = None;
    let mut last_decode = Instant::now() - MIN_GAP;
    loop {
        let msg = match rx.recv_timeout(IDLE_TEARDOWN) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => {
                pipe = None; // idle: release the decoder
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if stopped() {
            return;
        }
        let (mut bucket, mut ns) = match msg {
            Msg::Stop => return,
            Msg::Want { bucket, ns } => (bucket, ns),
        };
        // Drain: only the newest request matters. Scrubbing users are fickle.
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Stop => return,
                Msg::Want { bucket: b, ns: n } => {
                    bucket = b;
                    ns = n;
                }
            }
        }
        if stopped() {
            return;
        }
        let (id, file) = store.preview_slot(&path, size, mtime, bucket);
        let img = if let Some(img) = store.load(&file, &id) {
            img
        } else if let Some(src) = &source {
            match src(bucket, ns) {
                Ok(img) => {
                    if stopped() {
                        return;
                    }
                    let _ = store.save(&file, &id, &img);
                    img
                }
                Err(_) => continue,
            }
        } else {
            let gap = last_decode.elapsed();
            if gap < MIN_GAP {
                std::thread::sleep(MIN_GAP - gap);
            }
            if stopped() {
                return;
            }
            if pipe.is_none() {
                match thumbs::open_paused(&path, PREVIEW_W) {
                    Ok(p) => pipe = Some(p),
                    Err(_) => {
                        failed.store(true, Ordering::Relaxed);
                        continue;
                    }
                }
            }
            let Some(p) = pipe.as_ref() else { continue };
            last_decode = Instant::now();
            match thumbs::frame_at(p, ns, 3) {
                Ok(img) => {
                    // Locked, converting or closing: nothing reaches the cache.
                    if stopped() {
                        return;
                    }
                    let _ = store.save(&file, &id, &img);
                    img
                }
                Err(_) => {
                    pipe = None; // rebuild next time
                    continue;
                }
            }
        };
        if stopped() {
            return;
        }
        *out.lock().unwrap() = Some(PreviewFrame {
            bucket,
            blur: to_rgba(&thumbs::blurred(&img)),
            full: to_rgba(&img),
        });
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_scale_with_duration() {
        let short = Duration::from_secs(60);
        assert_eq!(bucket_len(short), Duration::from_secs(1));
        assert_eq!(bucket_for(Duration::from_millis(5400), short), 5);
        let long = Duration::from_secs(3600);
        assert_eq!(bucket_len(long), Duration::from_secs(12));
        assert_eq!(bucket_for(Duration::from_secs(25), long), 2);
        // About 300 buckets whatever the length.
        assert!(bucket_for(long, long) <= 301);
    }
}
