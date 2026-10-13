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
    use std::os::unix::ffi::OsStrExt;
    let mut h = blake3::Hasher::new();
    h.update(SCHEMA.as_bytes());
    h.update(path.as_os_str().as_bytes());
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
    std::fs::rename(&tmp, file).map_err(|e| format!("cache rename: {e}"))
}

/// Tiny version for discreet / screen-share-safe mode; the UI upscales it with
/// linear filtering, which reads as a heavy blur.
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
