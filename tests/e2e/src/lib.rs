//! End-to-end harness: loopback servers that misbehave on purpose, synthetic
//! media made with GStreamer, and a pump that drives the real `Player`.
//! Everything is SFW test patterns on 127.0.0.1.
//!
//! The whole crate is empty without the `network` feature, so the default
//! workspace build and `cargo tree --workspace` stay offline.
#![cfg(feature = "network")]

pub mod drive;
pub mod fixtures;
pub mod http;
pub mod proc;
pub mod swarm;

use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

/// The proxy policy and the GStreamer registry are process-global: scenarios
/// take this lock so they never overlap, whatever `--test-threads` says.
pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Elements the player and the decode pipelines are built from.
pub const PLAYBACK: &[&str] = &[
    "playbin",
    "uridecodebin",
    "appsink",
    "videoconvert",
    "videoscale",
    "videoflip",
];

/// Elements the fixtures are encoded with.
pub const WEBM: &[&str] = &[
    "videotestsrc",
    "audiotestsrc",
    "audioconvert",
    "opusenc",
    "vp8enc",
    "webmmux",
    "vp8dec",
    "matroskademux",
];
pub const TS: &[&str] = &[
    "videotestsrc",
    "x264enc",
    "h264parse",
    "mpegtsmux",
    "avdec_h264",
    "tsdemux",
];
/// Fragmented MP4 video plus a separate AAC track.
pub const FMP4_AV: &[&str] = &[
    "videotestsrc",
    "x264enc",
    "h264parse",
    "mp4mux",
    "avdec_h264",
    "qtdemux",
    "audiotestsrc",
    "audioconvert",
    "avenc_aac",
    "aacparse",
    "avdec_aac",
];
pub const FMP4: &[&str] = &[
    "videotestsrc",
    "x264enc",
    "h264parse",
    "mp4mux",
    "avdec_h264",
    "qtdemux",
];

/// `true` when GStreamer works and every element is installed. Otherwise the
/// scenario returns early with a printed reason; with
/// `JIZZCRAFT_REQUIRE_PLUGINS=1` (CI) it panics instead, because a green run
/// that skipped everything proves nothing.
pub fn ready(elements: &[&str]) -> bool {
    if std::process::Command::new("gst-launch-1.0")
        .arg("--version")
        .output()
        .is_err()
    {
        return skip("gst-launch-1.0 not found");
    }
    if let Err(e) = jizzcraft_media::init() {
        return skip(&e.to_string());
    }
    let missing: Vec<&str> = PLAYBACK
        .iter()
        .chain(elements)
        .copied()
        .filter(|e| gstreamer::ElementFactory::find(e).is_none())
        .collect();
    if !missing.is_empty() {
        return skip(&format!("GStreamer elements not installed: {missing:?}"));
    }
    true
}

fn skip(why: &str) -> bool {
    if std::env::var("JIZZCRAFT_REQUIRE_PLUGINS").is_ok_and(|v| v == "1") {
        panic!("JIZZCRAFT_REQUIRE_PLUGINS=1 and the scenario cannot run: {why}");
    }
    eprintln!("skipped: {why}");
    false
}

/// Fixed sentences only: a classified error is one of the known ones and
/// carries no address, host, port or path. Returns the sentence.
pub fn assert_sentence(raw: &str, want: &str) {
    let got = jizzcraft_fetch::errors::classify(raw);
    assert_eq!(got, want, "raw error: {raw:?}");
    assert_clean(got);
    assert_clean(raw);
}

/// Nothing that identifies where the bytes came from.
pub fn assert_clean(text: &str) {
    let t = text.to_ascii_lowercase();
    for bad in [
        "127.0.0.1",
        "localhost",
        "http://",
        "https://",
        "/v/",
        "/page",
        ".webm",
        ".m3u8",
        ".mpd",
        ".ts",
        ".m4s",
    ] {
        assert!(!t.contains(bad), "{bad:?} leaked into {text:?}");
    }
}

/// A service whose download and state folders are temporary.
pub struct Svc {
    pub svc: jizzcraft_fetch::service::FetchService,
    pub dl: tempfile::TempDir,
    pub st: tempfile::TempDir,
}

impl Svc {
    pub fn new(cfg: jizzcraft_fetch::FetchConfig) -> Svc {
        let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let svc = jizzcraft_fetch::service::FetchService::new(
            jizzcraft_fetch::FetchConfig {
                download_dir: Some(dl.path().into()),
                dht: false,
                ..cfg
            },
            st.path().join("state"),
        )
        .unwrap();
        Svc { svc, dl, st }
    }

    pub fn plain() -> Svc {
        Svc::new(jizzcraft_fetch::FetchConfig::default())
    }

    /// Files (not folders) left in the download folder.
    pub fn downloaded_files(&self) -> usize {
        fn walk(p: &std::path::Path) -> usize {
            std::fs::read_dir(p)
                .map(|d| {
                    d.flatten()
                        .map(|e| {
                            let p = e.path();
                            if p.is_dir() { walk(&p) } else { 1 }
                        })
                        .sum()
                })
                .unwrap_or(0)
        }
        walk(self.dl.path())
    }
}
