mod common;

use common::clips_ready;
use jizzcraft_media::player::{PlayState, Player};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn make_video(path: &std::path::Path, frames: u32) {
    common::make_video(path, frames, 640, 360);
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
    if !clips_ready() {
        return;
    }
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

#[test]
fn seek_requested_before_preroll_is_applied() {
    if !clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("s.webm");
    make_video(&src, 150); // 5 s
    let p = Player::open(&src, 0.5, wake());
    p.seek(Duration::from_secs(3), true); // immediately, pipeline not prerolled yet
    wait_until("frame", 10, || p.take_frame().is_some());
    wait_until("seek landed", 10, || {
        p.position() >= Duration::from_millis(2900) && !p.is_seeking()
    });
}

fn start(p: &Player) {
    wait_until("first frame", 10, || p.take_frame().is_some());
    wait_until("duration", 5, || p.duration().is_some());
}

#[test]
fn speed_changes_playhead_rate_and_survives_seeks() {
    if !clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("r.webm");
    make_video(&src, 300); // 10 s
    let p = Player::open(&src, 0.0, wake());
    start(&p);
    assert_eq!(p.rate(), 1.0);

    p.set_rate(2.0);
    wait_until("rate applied", 5, || !p.is_seeking());
    let t0 = Instant::now();
    let a = p.position();
    std::thread::sleep(Duration::from_millis(1000));
    let advanced = p.position().saturating_sub(a).as_secs_f64();
    let wall = t0.elapsed().as_secs_f64();
    let ratio = advanced / wall;
    assert!((1.5..2.6).contains(&ratio), "playhead ratio {ratio:.2}");

    // A relative seek must not reset the rate to 1.0.
    p.seek_relative(1);
    wait_until("seek settled", 5, || !p.is_seeking());
    let a = p.position();
    std::thread::sleep(Duration::from_millis(1000));
    let ratio = p.position().saturating_sub(a).as_secs_f64();
    assert!(ratio > 1.4, "rate lost after seek: {ratio:.2} s per second");
    assert_eq!(p.rate(), 2.0);

    p.set_rate(100.0);
    assert_eq!(p.rate(), 4.0, "clamped");
    p.set_rate(f64::NAN);
    assert_eq!(p.rate(), 1.0);
}

#[test]
fn frame_step_forward_and_back() {
    if !clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("f.webm");
    make_video(&src, 150);
    let p = Player::open(&src, 0.0, wake());
    start(&p);
    p.pause();
    wait_until("paused", 5, || p.state() == PlayState::Paused);
    p.seek(Duration::from_millis(1000), true);
    wait_until("settled", 5, || !p.is_seeking());
    while p.take_frame().is_some() {}
    let before = p.position();

    p.step_frame(true, 30.0);
    wait_until("stepped frame", 5, || p.take_frame().is_some());
    std::thread::sleep(Duration::from_millis(120));
    let fwd = p.position();
    let d = fwd.saturating_sub(before).as_millis() as u64;
    assert!((20..=80).contains(&d), "forward step moved {d} ms");
    assert_eq!(p.state(), PlayState::Paused);

    p.step_frame(false, 30.0);
    // The command is handled on the control thread: wait for the playhead.
    wait_until("stepped back", 5, || p.position() < fwd);
    wait_until("settled back", 5, || !p.is_seeking());
    let back = p.position();
    let d = fwd.saturating_sub(back).as_millis() as u64;
    assert!((20..=80).contains(&d), "back step moved {d} ms");
}

#[test]
fn ab_loop_keeps_playhead_inside_the_window() {
    if !clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("l.webm");
    make_video(&src, 300);
    let p = Player::open(&src, 0.0, wake());
    start(&p);
    p.set_loop(
        Some(Duration::from_millis(500)),
        Some(Duration::from_millis(1500)),
    );
    assert_eq!(
        p.loop_points(),
        (
            Some(Duration::from_millis(500)),
            Some(Duration::from_millis(1500))
        )
    );
    p.seek(Duration::from_millis(600), true);
    let end = Instant::now() + Duration::from_secs(4);
    let mut lo = u128::MAX;
    let mut hi = 0u128;
    while Instant::now() < end {
        let m = p.position().as_millis();
        lo = lo.min(m);
        hi = hi.max(m);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(lo >= 400, "left the loop below A: {lo} ms");
    assert!(hi <= 1900, "left the loop above B: {hi} ms");
    p.set_loop(None, None);
    assert_eq!(p.loop_points(), (None, None));
}

#[test]
fn a_only_loop_restarts_at_end_of_stream() {
    if !clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("e.webm");
    make_video(&src, 60); // 2 s
    let p = Player::open(&src, 0.0, wake());
    start(&p);
    p.set_loop(Some(Duration::from_millis(500)), None);
    p.seek(Duration::from_millis(1500), true);
    // Without a loop this reaches Ended; with one it must come back below A+.
    let end = Instant::now() + Duration::from_secs(6);
    let mut wrapped = false;
    while Instant::now() < end {
        if p.state() == PlayState::Ended {
            panic!("A-only loop must not end");
        }
        if p.position() < Duration::from_millis(1300) && p.position() >= Duration::from_millis(400)
        {
            wrapped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(wrapped, "playhead never wrapped back to A");
}
