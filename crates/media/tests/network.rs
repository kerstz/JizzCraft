//! A "video" that is really a streaming playlist must not make JizzCraft touch
//! the network. Each trap file names a local TCP listener; every code path that
//! parses media must finish without a single connection reaching it.

mod common;

use jizzcraft_core::MediaKind;
use jizzcraft_media::integrity::{CheckMode, check_media};
use jizzcraft_media::player::{PlayState, Player};
use jizzcraft_media::preview::PreviewService;
use jizzcraft_media::{guard, probe, thumbs};
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// A listener nobody should ever call. Connections wait in the kernel backlog
/// until `connections` drains it, so late attempts are counted too.
struct Trap {
    listener: TcpListener,
    port: u16,
}

impl Trap {
    fn new() -> Trap {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        Trap { listener, port }
    }

    fn connections(&self) -> usize {
        // Give anything started in the background a moment to try.
        std::thread::sleep(Duration::from_millis(1500));
        let mut n = 0;
        loop {
            match self.listener.accept() {
                Ok(_) => n += 1,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return n,
                Err(e) => panic!("accept: {e}"),
            }
        }
    }

    /// Trap files with video extensions, as a downloader might have saved them.
    fn files(&self, dir: &Path) -> Vec<PathBuf> {
        let base = format!("http://127.0.0.1:{}", self.port);
        let hls_media = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:2.0,\n{base}/seg0.ts\n#EXTINF:2.0,\n{base}/seg1.ts\n#EXT-X-ENDLIST\n"
        );
        let hls_master = format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=320x240\n{base}/v.m3u8\n"
        );
        let dash = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT4S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <BaseURL>{base}/</BaseURL>
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <Representation id="v" bandwidth="200000" width="320" height="240" codecs="avc1.42c00d">
        <BaseURL>v.mp4</BaseURL>
        <SegmentBase indexRange="0-99"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"#
        );
        [
            ("media-playlist.mp4", hls_media),
            ("master-playlist.mkv", hls_master),
            ("manifest.webm", dash),
        ]
        .into_iter()
        .map(|(name, body)| {
            let p = dir.join(name);
            std::fs::write(&p, body).expect("write trap");
            p
        })
        .collect()
    }
}

fn assert_no_connections(trap: &Trap, what: &str) {
    let n = trap.connections();
    assert_eq!(n, 0, "{what}: {n} connection(s) reached the network trap");
}

#[test]
fn player_never_connects() {
    if !common::ready(common::PLAYBACK) {
        return;
    }
    let trap = Trap::new();
    let dir = tempfile::tempdir().unwrap();
    for f in trap.files(dir.path()) {
        let p = Player::open(&f, 0.0, Arc::new(|| {}));
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end && p.state() != PlayState::Error {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(p.state(), PlayState::Error, "{}", f.display());
        assert_eq!(p.error().as_deref(), Some(guard::REFUSED));
        drop(p);
    }
    assert_no_connections(&trap, "player");
}

/// The in-process stream source is the one place an `appsrc` is allowed. A
/// playlist pushed through it must be refused like any other, not followed.
#[test]
fn stream_player_never_connects() {
    if !common::ready(common::PLAYBACK) || !common::ready(&["appsrc"]) {
        return;
    }
    let trap = Trap::new();
    let dir = tempfile::tempdir().unwrap();
    for f in trap.files(dir.path()) {
        let bytes = std::fs::read(&f).unwrap();
        let len = bytes.len() as u64;
        let p = Player::open_stream(
            Box::new(std::io::Cursor::new(bytes)),
            len,
            0.0,
            Arc::new(|| {}),
        );
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end && p.state() != PlayState::Error {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(p.state(), PlayState::Error, "{}", f.display());
        assert_eq!(p.error().as_deref(), Some(guard::REFUSED));
        drop(p);
    }
    assert_no_connections(&trap, "stream player");
}

#[test]
fn thumbnails_and_probe_never_connect() {
    if !common::ready(common::PLAYBACK) {
        return;
    }
    let trap = Trap::new();
    let dir = tempfile::tempdir().unwrap();
    for f in trap.files(dir.path()) {
        assert_eq!(
            thumbs::video_thumb(&f).err().as_deref(),
            Some(guard::REFUSED)
        );
        assert!(probe::probe(&f).is_err(), "{}", f.display());
    }
    assert_no_connections(&trap, "thumbnails/probe");
}

#[test]
fn integrity_checks_never_connect() {
    if !common::ready(common::PLAYBACK) {
        return;
    }
    let trap = Trap::new();
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    for f in trap.files(dir.path()) {
        for mode in [CheckMode::Quick, CheckMode::Full] {
            let r = check_media(&f, MediaKind::Video, mode, &cancel);
            assert_eq!(r, Err("remote-media"), "{} {mode:?}", f.display());
        }
    }
    assert_no_connections(&trap, "integrity");
}

#[test]
fn scrub_previews_never_connect() {
    if !common::ready(common::PLAYBACK) {
        return;
    }
    let trap = Trap::new();
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for f in trap.files(dir.path()) {
        let svc = PreviewService::new(f, 1, 0, cache.path().to_path_buf(), Arc::new(|| {}));
        svc.request(0, Duration::ZERO);
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end && !svc.failed() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(svc.failed());
    }
    assert_no_connections(&trap, "previews");
}
