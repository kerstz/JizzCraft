use jizzcraft_media::player::{PlayState, Player};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn make_video(path: &std::path::Path, frames: u32) {
    let st = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "videotestsrc",
            &format!("num-buffers={frames}"),
            "pattern=ball",
            "!",
        ])
        .args([
            "video/x-raw,width=640,height=360,framerate=30/1",
            "!",
            "videoconvert",
            "!",
        ])
        .args([
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
        .unwrap();
    assert!(st.success());
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn wake() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

#[test]
fn play_seek_eos_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("p.webm");
    make_video(&src, 90); // 3 s
    let mut p = Player::open(&src, 0.5, wake());

    // First frame arrives with the right geometry.
    let mut first = None;
    wait_until("first frame", 10, || {
        first = p.take_frame();
        first.is_some()
    });
    let f = first.unwrap();
    assert_eq!((f.width(), f.height()), (640, 360));
    assert!(f.stride() >= 640 * 4);
    assert_eq!(f.pixel_aspect(), (1, 1));
    assert!(f.with_pixels(|d| d.len() >= f.stride() * 360).unwrap());

    wait_until("duration", 5, || p.duration().is_some());
    let d = p.duration().unwrap();
    assert!(
        (2800..3300).contains(&(d.as_millis() as u64)),
        "duration {d:?}"
    );

    // Pause holds: no new frames while paused.
    p.pause();
    wait_until("paused", 5, || p.state() == PlayState::Paused);
    std::thread::sleep(Duration::from_millis(150));
    while p.take_frame().is_some() {}
    let delivered = p.counters().delivered;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        p.counters().delivered,
        delivered,
        "frames delivered while paused"
    );

    // Seek while paused shows the target frame (preroll) and moves the playhead.
    p.seek(Duration::from_secs(2), true);
    wait_until("frame after seek", 5, || p.take_frame().is_some());
    wait_until("seek settled", 5, || !p.is_seeking());
    let pos = p.position();
    assert!(
        (1900..2200).contains(&(pos.as_millis() as u64)),
        "position {pos:?}"
    );

    // Burst of coalesced scrub seeks must not wedge the player.
    for ms in (0..2500).step_by(100) {
        p.seek(Duration::from_millis(ms), false);
    }
    p.seek(Duration::from_millis(1000), true);
    wait_until("burst settled", 5, || !p.is_seeking());

    // Play to the end: EOS holds, then replay works.
    p.seek(Duration::from_millis(2300), true);
    p.play();
    wait_until("eos", 10, || p.state() == PlayState::Ended);
    assert!(p.position() >= Duration::from_millis(2800));
    p.replay();
    wait_until("playing again", 5, || p.state() == PlayState::Playing);
    wait_until("moving", 5, || {
        p.position() < Duration::from_secs(2) && p.take_frame().is_some()
    });
    p.set_volume(0.2);
    p.set_muted(true);
}

#[test]
fn missing_file_reports_readable_error() {
    let p = Player::open(std::path::Path::new("/nonexistent/clip.mkv"), 1.0, wake());
    wait_until("error", 5, || p.state() == PlayState::Error);
    let e = p.error().unwrap();
    assert!(!e.is_empty());
}

#[test]
fn garbage_file_errors_without_hanging() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("g.mp4");
    std::fs::write(&src, vec![0u8; 4096]).unwrap();
    let p = Player::open(&src, 1.0, wake());
    wait_until("error", 10, || p.state() == PlayState::Error);
    assert!(p.error().is_some());
}
