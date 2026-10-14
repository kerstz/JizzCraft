//! Helpers shared by the media tests: plugin checks and synthetic clips.
#![allow(dead_code)] // each test crate uses a different subset

use std::path::Path;
use std::process::Command;

/// Elements the player and the decode pipelines are built from.
pub const PLAYBACK: &[&str] = &[
    "playbin",
    "uridecodebin",
    "appsink",
    "videoconvert",
    "videoscale",
    "videoflip",
];

/// Elements `make_video` encodes with, and the decoder for what it writes.
pub const CLIPS: &[&str] = &[
    "videotestsrc",
    "vp8enc",
    "webmmux",
    "vp8dec",
    "matroskademux",
];

/// `true` when GStreamer works and every element is installed. Otherwise the
/// calling test should return early: headless CI runners without the plugins
/// skip these tests instead of failing them.
pub fn ready(elements: &[&str]) -> bool {
    if let Err(e) = jizzcraft_media::init() {
        eprintln!("skipped: {e}");
        return false;
    }
    let missing: Vec<&str> = elements
        .iter()
        .copied()
        .filter(|e| gstreamer::ElementFactory::find(e).is_none())
        .collect();
    if !missing.is_empty() {
        eprintln!("skipped: GStreamer elements not installed: {missing:?}");
        return false;
    }
    true
}

/// Playback plus synthetic clips (needs `gst-launch-1.0` too).
pub fn clips_ready() -> bool {
    if Command::new("gst-launch-1.0")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: gst-launch-1.0 not found");
        return false;
    }
    ready(PLAYBACK) && ready(CLIPS)
}

/// A moving-ball VP8/WebM clip at 30 fps. Fixed pipeline; only the
/// temporary path is interpolated.
pub fn make_video(path: &Path, frames: u32, width: u32, height: u32) {
    let st = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "videotestsrc",
            &format!("num-buffers={frames}"),
            "pattern=ball",
            "!",
        ])
        .arg(format!(
            "video/x-raw,width={width},height={height},framerate=30/1"
        ))
        .args([
            "!",
            "videoconvert",
            "!",
            "vp8enc",
            "deadline=1",
            "keyframe-max-dist=15",
            "!",
            "webmmux",
            "!",
            "filesink",
        ])
        .arg(format!("location={}", path.display()))
        .status()
        .expect("gst-launch-1.0");
    assert!(st.success());
}
