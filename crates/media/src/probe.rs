//! Codec/duration/resolution probing through GStreamer's Discoverer.

use gstreamer as gst;
use gstreamer_pbutils as pbutils;
use gstreamer_pbutils::prelude::*;
use jizzcraft_core::MediaMeta;
use std::path::Path;

pub fn probe(path: &Path) -> Result<MediaMeta, String> {
    crate::init()?;
    let uri = crate::file_uri(path)?;
    let disc =
        pbutils::Discoverer::new(gst::ClockTime::from_seconds(10)).map_err(|e| e.to_string())?;
    // The discoverer owns its pipeline: watch it from the file source up,
    // before anything downstream of the source exists.
    disc.connect_source_setup(|_, source| {
        if let Some(top) = crate::guard::top_bin(source) {
            let _ = crate::guard::watch(&top);
        }
    });
    let info = disc.discover_uri(&uri).map_err(|e| e.to_string())?;
    match info.result() {
        pbutils::DiscovererResult::Ok => {}
        pbutils::DiscovererResult::MissingPlugins => {
            return Err("missing GStreamer plugin to read this file — install gst-plugins-good/bad/ugly and gst-libav".into());
        }
        pbutils::DiscovererResult::Timeout => return Err("probe timed out".into()),
        _ => return Err("file could not be read".into()),
    }
    let mut meta = MediaMeta {
        duration_ms: info.duration().map(|d| d.mseconds()),
        ..Default::default()
    };
    if let Some(v) = info.video_streams().first() {
        meta.width = Some(v.width());
        meta.height = Some(v.height());
        if let Some(caps) = v.caps() {
            meta.codec = Some(pbutils::pb_utils_get_codec_description(&caps).to_string());
        }
    }
    Ok(meta)
}
