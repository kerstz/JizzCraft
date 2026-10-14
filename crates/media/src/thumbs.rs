//! Thumbnail generation and the on-disk cache.
//! Cache lives in the XDG cache dir, keyed by hash(path + size + mtime).
//! Nothing is ever written next to the originals.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use image::{DynamicImage, ImageDecoder, RgbImage, imageops::FilterType};
use std::path::{Path, PathBuf};

pub const THUMB_MAX: u32 = 320;
const BLUR_W: u32 = 10;
const SCHEMA: &str = "thumb-v1";

pub fn cache_key(path: &Path, size: u64, mtime: i64) -> String {
    let mut h = blake3::Hasher::new();
    h.update(SCHEMA.as_bytes());
    h.update(&jizzcraft_core::pathbytes::path_to_bytes(path));
    h.update(&size.to_le_bytes());
    h.update(&mtime.to_le_bytes());
    h.finalize().to_hex().as_str()[..32].to_owned()
}

pub fn thumb_file(cache_root: &Path, key: &str) -> PathBuf {
    cache_root
        .join("thumbs")
        .join(&key[..2])
        .join(format!("{key}.jpg"))
}

pub fn load_cached(file: &Path) -> Option<RgbImage> {
    image::open(file).ok().map(|i| i.to_rgb8())
}

pub fn save_cached(file: &Path, img: &RgbImage) -> Result<(), String> {
    let dir = file.parent().ok_or("bad cache path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cache dir: {e}"))?;
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut out = std::fs::File::create(&tmp).map_err(|e| format!("cache write: {e}"))?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .encode_image(img)
            .map_err(|e| format!("cache encode: {e}"))?;
    }
    jizzcraft_core::fsutil::replace(&tmp, file).map_err(|e| format!("cache rename: {e}"))
}

/// Tiny version for discreet / screen-share-safe mode; the UI upscales it with
/// linear filtering, which reads as a heavy blur. Impressionism, on demand.
pub fn blurred(img: &RgbImage) -> RgbImage {
    let w = BLUR_W.min(img.width()).max(1);
    let h = ((img.height() as f32 * w as f32 / img.width() as f32).round() as u32).max(1);
    image::imageops::resize(img, w, h, FilterType::Triangle)
}

pub fn image_thumb(src: &Path) -> Result<RgbImage, String> {
    let reader = image::ImageReader::open(src)
        .map_err(|e| format!("cannot open image: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("cannot read image: {e}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("unsupported image: {e}"))?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("decode failed: {e}"))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img.thumbnail(THUMB_MAX, THUMB_MAX).to_rgb8())
}

pub(crate) struct NullOnDrop(pub gst::Pipeline);
impl Drop for NullOnDrop {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

/// A paused local-file decode pipeline ending in an RGB appsink no wider or
/// taller than `max_side`. Guard it with `NullOnDrop`.
pub(crate) struct Paused {
    pub pipeline: gst::Pipeline,
    pub sink: gst_app::AppSink,
    pub bus: gst::Bus,
    pub _guard: NullOnDrop,
}

pub(crate) fn open_paused(src: &Path, max_side: u32) -> Result<Paused, String> {
    crate::init()?;
    let uri = crate::file_uri(src)?;
    let mk = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("missing GStreamer element `{name}`"))
    };

    let pipeline = gst::Pipeline::new();
    // A refusal arrives as an error on the bus, like any other failure.
    let _ = crate::guard::watch(pipeline.upcast_ref());
    let dec = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .map_err(|_| "missing GStreamer element `uridecodebin` (gst-plugins-base)".to_string())?;
    let conv = mk("videoconvert")?;
    let scale = mk("videoscale")?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .field("width", gst::IntRange::new(1, max_side as i32))
        .field("height", gst::IntRange::new(1, max_side as i32))
        .build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .max_buffers(1)
        .sync(false)
        .build();

    pipeline
        .add_many([&dec, &conv, &scale, sink.upcast_ref()])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&conv, &scale, sink.upcast_ref()]).map_err(|e| e.to_string())?;

    let conv_sink = conv
        .static_pad("sink")
        .ok_or("videoconvert has no sink pad")?;
    dec.connect_pad_added(move |_, pad| {
        if conv_sink.is_linked() {
            return;
        }
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let is_video = caps
            .structure(0)
            .is_some_and(|s| s.name().starts_with("video/"));
        if is_video {
            let _ = pad.link(&conv_sink);
        }
    });

    let guard = NullOnDrop(pipeline.clone());
    let bus = pipeline.bus().ok_or("pipeline has no bus")?;
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| "cannot pause decode pipeline".to_string())?;
    wait_async_done(&bus, 15)?;
    Ok(Paused {
        pipeline,
        sink,
        bus,
        _guard: guard,
    })
}

/// Seek a paused pipeline to `ns` and pull the frame there.
pub(crate) fn frame_at(p: &Paused, ns: u64, timeout_s: u64) -> Result<RgbImage, String> {
    let target = gst::ClockTime::from_nseconds(ns);
    p.pipeline
        .seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST,
            target,
        )
        .map_err(|_| "seek failed".to_string())?;
    wait_async_done(&p.bus, timeout_s)?;
    let sample = p
        .sink
        .try_pull_preroll(gst::ClockTime::from_seconds(timeout_s))
        .ok_or("no video frame available")?;
    sample_to_rgb(&sample)
}

/// Dedicated thumbnail pipeline: uridecodebin → videoconvert → videoscale → appsink.
/// Paused, seeked to ~10 %, then the preroll sample is pulled.
pub fn video_thumb(src: &Path) -> Result<RgbImage, String> {
    let p = open_paused(src, THUMB_MAX)?;
    if let Some(dur) = p.pipeline.query_duration::<gst::ClockTime>() {
        let target = gst::ClockTime::from_nseconds(dur.nseconds() / 10);
        if p.pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, target)
            .is_ok()
        {
            // Not fatal if it fails: the first frame is still a fine thumbnail.
            // Usually a black screen or a logo, but a fine one.
            let _ = wait_async_done(&p.bus, 10);
        }
    }
    let sample = p
        .sink
        .try_pull_preroll(gst::ClockTime::from_seconds(5))
        .ok_or("no video frame available")?;
    sample_to_rgb(&sample)
}

pub(crate) fn wait_async_done(bus: &gst::Bus, secs: u64) -> Result<(), String> {
    use gst::MessageView;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut missing: Option<String> = None;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(missing.unwrap_or_else(|| "timed out reading video".into()));
        }
        let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(left.as_millis() as u64),
            &[
                gst::MessageType::AsyncDone,
                gst::MessageType::Error,
                gst::MessageType::Element,
            ],
        ) else {
            continue;
        };
        match msg.view() {
            MessageView::AsyncDone(_) => return Ok(()),
            MessageView::Error(e) => {
                if e.error().message() == crate::guard::REFUSED {
                    return Err(crate::guard::REFUSED.into());
                }
                return Err(missing.unwrap_or_else(|| format!("decode error: {}", e.error())));
            }
            MessageView::Element(_) => {
                if let Some(m) = crate::describe_missing_plugin(&msg) {
                    missing = Some(m);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn sample_to_rgb(sample: &gst::Sample) -> Result<RgbImage, String> {
    let caps = sample.caps().ok_or("sample without caps")?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|e| e.to_string())?;
    let buffer = sample.buffer().ok_or("sample without buffer")?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|e| e.to_string())?;
    let (w, h) = (info.width() as usize, info.height() as usize);
    let stride = frame.plane_stride()[0] as usize;
    let data = frame.plane_data(0).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let row = data
            .get(y * stride..y * stride + w * 3)
            .ok_or("short frame")?;
        out.extend_from_slice(row);
    }
    RgbImage::from_raw(w as u32, h as u32, out).ok_or_else(|| "bad frame size".into())
}

/// Default size limit of the thumbnail and preview cache.
pub const DEFAULT_CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// What a prune removed and what is left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Thumbnails whose media row is gone.
    pub orphans: u64,
    pub orphan_bytes: u64,
    /// Least recently used files removed to fit the limit.
    pub evicted: u64,
    pub evicted_bytes: u64,
    pub kept_files: u64,
    pub kept_bytes: u64,
}

impl PruneReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "orphans": self.orphans, "orphan_bytes": self.orphan_bytes,
            "evicted": self.evicted, "evicted_bytes": self.evicted_bytes,
            "kept_files": self.kept_files, "kept_bytes": self.kept_bytes,
            "removed_bytes": self.orphan_bytes + self.evicted_bytes,
        })
    }
}

/// Cache keys of every thumbnail a library row can still ask for.
pub fn live_keys(db: &jizzcraft_core::Db) -> Result<std::collections::HashSet<String>, String> {
    let rows = db
        .query(&jizzcraft_core::Filter {
            include_missing: true,
            hidden: jizzcraft_core::HiddenMode::Include,
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|m| cache_key(&m.path, m.size, m.mtime))
        .collect())
}

/// Prune the default cache directory to `max_bytes` (LRU), without orphan
/// detection (that needs the library: see `prune_cache_in`).
pub fn prune_cache(max_bytes: u64) -> PruneReport {
    prune_cache_in(&jizzcraft_core::paths::cache_dir(), None, max_bytes)
}

/// Remove orphaned thumbnails (key not in `live`, when given), then evict the
/// least recently used files (newest of atime/mtime) until the cache fits in
/// `max_bytes`. Only regular files under `thumbs/` and `previews/` are touched.
pub fn prune_cache_in(
    root: &Path,
    live: Option<&std::collections::HashSet<String>>,
    max_bytes: u64,
) -> PruneReport {
    use std::time::SystemTime;
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                walk(&e.path(), out);
            } else if md.is_file() {
                let t = md
                    .modified()
                    .ok()
                    .into_iter()
                    .chain(md.accessed().ok())
                    .max()
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((e.path(), md.len(), t));
            }
        }
    }
    let mut rep = PruneReport::default();
    let mut files = Vec::new();
    let mut thumbs = Vec::new();
    walk(&root.join("thumbs"), &mut thumbs);
    walk(&root.join("previews"), &mut files);

    for (p, len, t) in thumbs {
        let orphan = live.is_some_and(|l| {
            p.extension().is_some_and(|x| x == "jpg")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|k| !l.contains(k))
        });
        if orphan && std::fs::remove_file(&p).is_ok() {
            rep.orphans += 1;
            rep.orphan_bytes += len;
        } else {
            files.push((p, len, t));
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.2);
    let mut keep_from = 0;
    for (p, len, _) in &files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(p).is_ok() {
            total -= len;
            rep.evicted += 1;
            rep.evicted_bytes += len;
        }
        keep_from += 1;
    }
    rep.kept_files = (files.len() - keep_from) as u64;
    rep.kept_bytes = total;
    rep
}

/// `cache.prune {max_bytes?}`: drop orphaned thumbnails, then evict LRU files
/// until the cache fits (default `DEFAULT_CACHE_LIMIT`).
pub fn register_cache_prune<C: jizzcraft_core::HasLibrary>(reg: &mut jizzcraft_core::Registry<C>) {
    reg.register(
        "cache.prune",
        "Prune the thumbnail cache: {max_bytes?} (default 2 GiB)",
        |c, a| {
            let max = a
                .get("max_bytes")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(DEFAULT_CACHE_LIMIT);
            let live = live_keys(c.db()).map_err(jizzcraft_core::Error::Invalid)?;
            let rep = prune_cache_in(&jizzcraft_core::paths::cache_dir(), Some(&live), max);
            Ok(rep.to_json())
        },
    );
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[test]
    fn orphans_go_then_oldest_until_it_fits() {
        let d = tempfile::tempdir().unwrap();
        let mk = |key: &str, age: u64| {
            let f = thumb_file(d.path(), key);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, vec![0u8; 100]).unwrap();
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::File::options()
                .write(true)
                .open(&f)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
                .unwrap();
            f
        };
        let a = mk("aa000000000000000000000000000001", 300);
        let b = mk("bb000000000000000000000000000002", 200);
        let c = mk("cc000000000000000000000000000003", 100);
        let gone = mk("dd000000000000000000000000000004", 10);
        let live: std::collections::HashSet<String> = [
            "aa000000000000000000000000000001",
            "bb000000000000000000000000000002",
            "cc000000000000000000000000000003",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let r = prune_cache_in(d.path(), Some(&live), 200);
        assert_eq!(
            (r.orphans, r.evicted, r.kept_files, r.kept_bytes),
            (1, 1, 2, 200)
        );
        assert!(!gone.exists() && !a.exists() && b.exists() && c.exists());
    }
}
