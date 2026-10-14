//! `Player::open_stream`: playing a clip whose bytes arrive over time, the way
//! a download in progress does. Synthetic clips only.

mod common;

use common::clips_ready;
use jizzcraft_media::player::{PlayState, Player};
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A reader over `data` that only has the first `available` bytes. Anything
/// beyond answers `WouldBlock`, like a torrent file whose pieces are missing.
struct Throttled {
    data: Arc<Vec<u8>>,
    pos: u64,
    available: Arc<AtomicU64>,
    /// Highest offset ever served, to prove nothing came from the future.
    served_max: Arc<Mutex<u64>>,
    /// The last bytes are always there: players fetch the index (cues, moov)
    /// from the tail first, and a torrent client prioritises it too.
    tail: u64,
}

impl Read for Throttled {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let avail = self
            .available
            .load(Ordering::SeqCst)
            .min(self.data.len() as u64);
        if self.pos >= self.data.len() as u64 {
            return Ok(0);
        }
        let len = self.data.len() as u64;
        let in_tail = self.pos >= len.saturating_sub(self.tail);
        if self.pos >= avail && !in_tail {
            return Err(Error::from(ErrorKind::WouldBlock));
        }
        let limit = if in_tail { len } else { avail };
        let n = buf.len().min((limit - self.pos) as usize);
        buf[..n].copy_from_slice(&self.data[self.pos as usize..self.pos as usize + n]);
        self.pos += n as u64;
        let mut m = self.served_max.lock().unwrap();
        *m = (*m).max(self.pos);
        Ok(n)
    }
}

impl Seek for Throttled {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let len = self.data.len() as i64;
        let np = match from {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::End(d) => len + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if np < 0 {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        self.pos = np as u64;
        Ok(self.pos)
    }
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

struct Rig {
    player: Player,
    available: Arc<AtomicU64>,
    served_max: Arc<Mutex<u64>>,
    len: u64,
}

fn rig(data: Vec<u8>, initial_pct: u64) -> Rig {
    let len = data.len() as u64;
    let available = Arc::new(AtomicU64::new(len * initial_pct / 100));
    let served_max = Arc::new(Mutex::new(0));
    let reader = Throttled {
        data: Arc::new(data),
        pos: 0,
        available: Arc::clone(&available),
        served_max: Arc::clone(&served_max),
        tail: len / 8,
    };
    let player = Player::open_stream(Box::new(reader), len, 0.0, Arc::new(|| {}));
    Rig {
        player,
        available,
        served_max,
        len,
    }
}

fn clip(frames: u32) -> Vec<u8> {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("s.webm");
    common::make_video(&p, frames, 320, 240);
    std::fs::read(p).unwrap()
}

#[test]
fn plays_while_the_data_arrives_and_never_errors() {
    if !clips_ready() {
        return;
    }
    let r = rig(clip(90), 25);
    // The rest arrives in steps, slower than real time at first.
    let (avail, len) = (Arc::clone(&r.available), r.len);
    let feeder = std::thread::spawn(move || {
        for pct in [40, 55, 70, 85, 100] {
            std::thread::sleep(Duration::from_millis(400));
            avail.store(len * pct / 100, Ordering::SeqCst);
        }
    });
    let mut got_frame = false;
    wait_until("end of stream", 40, || {
        assert_ne!(r.player.state(), PlayState::Error, "{:?}", r.player.error());
        got_frame |= r.player.take_frame().is_some();
        r.player.state() == PlayState::Ended
    });
    assert!(got_frame, "no frame was ever delivered");
    feeder.join().unwrap();
    assert!(*r.served_max.lock().unwrap() <= r.len);
}

#[test]
fn seek_into_a_missing_region_buffers_then_plays() {
    if !clips_ready() {
        return;
    }
    // Keyframes every 15 frames; 6 s clip. Only the first 60 % (plus the
    // index at the tail) is present, so a seek to 4 s lands in a hole.
    let r = rig(clip(180), 60);
    wait_until("first frame", 15, || r.player.take_frame().is_some());
    wait_until("duration", 5, || r.player.duration().is_some());
    r.player.seek(Duration::from_secs(4), true);
    // The data for t=4s is not there: no error, just waiting.
    std::thread::sleep(Duration::from_millis(1200));
    assert_ne!(r.player.state(), PlayState::Error, "{:?}", r.player.error());
    r.available.store(r.len, Ordering::SeqCst);
    wait_until("playback near the seek target", 20, || {
        assert_ne!(r.player.state(), PlayState::Error, "{:?}", r.player.error());
        r.player.take_frame();
        r.player.position() >= Duration::from_millis(3500)
    });
}

#[test]
fn mp4_with_the_index_at_the_end_plays_once_the_tail_is_there() {
    if !clips_ready() || !common::ready(&["x264enc", "mp4mux", "avdec_h264"]) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("t.mp4");
    let st = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "videotestsrc",
            "num-buffers=90",
            "pattern=ball",
            "!",
            "video/x-raw,width=320,height=240,framerate=30/1",
            "!",
            "videoconvert",
            "!",
            "x264enc",
            "!",
            "mp4mux",
            "!",
        ])
        .arg(format!("filesink location={}", p.display()))
        .status();
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("skipped: could not encode an mp4");
        return;
    }
    let data = std::fs::read(&p).unwrap();
    let r = rig(data, 100);
    let mut got = false;
    wait_until("mp4 frame", 20, || {
        assert_ne!(r.player.state(), PlayState::Error, "{:?}", r.player.error());
        got |= r.player.take_frame().is_some();
        got
    });
}

#[test]
fn junk_bytes_end_in_an_error_not_in_loading() {
    if !clips_ready() {
        return;
    }
    // Not media, not a manifest: 300 bytes of noise.
    let noise: Vec<u8> = (0..300u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8 | 0x80)
        .collect();
    let r = rig(noise, 100);
    wait_until("an error", 30, || r.player.state() == PlayState::Error);
}

#[test]
fn rapid_seeks_never_hang() {
    if !clips_ready() {
        return;
    }
    let r = rig(clip(120), 100);
    wait_until("first frame", 15, || r.player.take_frame().is_some());
    wait_until("duration", 5, || r.player.duration().is_some());
    for ms in [3000u64, 500, 3500, 100, 2500, 0, 3900, 1200, 2200, 800] {
        r.player.seek(Duration::from_millis(ms), false);
        std::thread::sleep(Duration::from_millis(60));
    }
    wait_until("end of stream", 30, || {
        assert_ne!(r.player.state(), PlayState::Error, "{:?}", r.player.error());
        r.player.take_frame();
        r.player.state() == PlayState::Ended
    });
}
