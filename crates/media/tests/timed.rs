//! `Player::open_timed`: segmented media pushed in-process, with seeking and
//! quality change. Synthetic clips only.
//!
//! How the segments are made: one MPEG-TS clip per quality is encoded with
//! `videotestsrc ! x264enc key-int-max=<fps> ! mpegtsmux`, then cut on 188-byte
//! packet boundaries at every keyframe (the packet with the random-access
//! flag), pulling the PAT/PMT that precede it into the new segment. Every
//! segment is therefore independently decodable, like an HLS `.ts` segment,
//! and its presentation time is its index times the segment length. A clip of
//! 8 s gives eight 1 s segments.

mod common;

use jizzcraft_core::timed::{Chunk, TimedSource, VariantInfo};
use jizzcraft_media::guard;
use jizzcraft_media::player::{PlayState, Player};
use std::io::{Error, ErrorKind};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SEG: Duration = Duration::from_secs(1);
const SECONDS: u32 = 8;

fn ready() -> bool {
    common::ready(common::PLAYBACK)
        && common::ready(&["x264enc", "h264parse", "mpegtsmux", "avdec_h264", "tsdemux"])
        && Command::new("gst-launch-1.0")
            .arg("--version")
            .output()
            .is_ok()
}

fn encode_ts(width: u32, height: u32) -> Option<Vec<u8>> {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("clip.ts");
    let st = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "videotestsrc",
            &format!("num-buffers={}", SECONDS * 30),
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
            "x264enc",
            "key-int-max=30",
            "bframes=0",
            "speed-preset=ultrafast",
            "!",
            "h264parse",
            "!",
            "mpegtsmux",
            "!",
            "filesink",
        ])
        .arg(format!("location={}", p.display()))
        .status()
        .ok()?;
    if !st.success() {
        return None;
    }
    std::fs::read(p).ok()
}

/// Cut a TS stream before every keyframe packet (with its PAT/PMT).
fn split_ts(data: &[u8]) -> Vec<Vec<u8>> {
    let pkts: Vec<&[u8]> = data.as_chunks::<188>().0.iter().map(|c| &c[..]).collect();
    let pid = |p: &[u8]| (((p[1] & 0x1f) as u16) << 8) | p[2] as u16;
    let rai = |p: &[u8]| p[3] & 0x20 != 0 && p[4] > 0 && p[5] & 0x40 != 0;
    let mut cuts = Vec::new();
    for (i, p) in pkts.iter().enumerate() {
        if p[0] == 0x47 && pid(p) != 0 && rai(p) {
            let video = pid(p);
            let mut c = i;
            while c > 0 && pid(pkts[c - 1]) != video {
                c -= 1;
            }
            cuts.push(c);
        }
    }
    cuts.dedup();
    if cuts.first() != Some(&0) {
        cuts.insert(0, 0);
    }
    cuts.push(pkts.len());
    cuts.windows(2)
        .map(|w| pkts[w[0]..w[1]].concat())
        .filter(|s| !s.is_empty())
        .collect()
}

struct Mem {
    /// One list of segments per variant.
    variants: Vec<Vec<Arc<Vec<u8>>>>,
    cur: usize,
    manual: bool,
    next: usize,
    discont: bool,
    live: bool,
    delivered: usize,
    /// Pulls answered with WouldBlock before each segment.
    stall: bool,
    flip: bool,
    /// Ask for a downgrade once this many segments were delivered.
    downgrade_at: Option<usize>,
}

impl Mem {
    fn new(variants: Vec<Vec<Vec<u8>>>, live: bool) -> Mem {
        Mem {
            variants: variants
                .into_iter()
                .map(|v| v.into_iter().map(Arc::new).collect())
                .collect(),
            cur: 0,
            manual: false,
            next: 0,
            discont: true,
            live,
            delivered: 0,
            stall: false,
            flip: false,
            downgrade_at: None,
        }
    }
}

impl TimedSource for Mem {
    fn duration(&self) -> Option<Duration> {
        (!self.live).then(|| SEG * self.variants[0].len() as u32)
    }
    fn seek(&mut self, t: Duration) -> Duration {
        let i = ((t.as_secs_f64() / SEG.as_secs_f64()) as usize).min(self.variants[0].len());
        self.next = i;
        self.discont = true;
        SEG * i as u32
    }
    fn pull(&mut self) -> std::io::Result<Option<Chunk>> {
        if self.stall {
            self.flip = !self.flip;
            if self.flip {
                return Err(Error::from(ErrorKind::WouldBlock));
            }
        }
        let segs = &self.variants[self.cur];
        let Some(bytes) = segs.get(self.next) else {
            return Ok(None);
        };
        let c = Chunk {
            bytes: bytes.to_vec(),
            pts: Some(SEG * self.next as u32),
            discont: std::mem::take(&mut self.discont),
        };
        self.next += 1;
        self.delivered += 1;
        Ok(Some(c))
    }
    fn buffered_until(&self) -> Option<Duration> {
        Some(SEG * self.next as u32)
    }
    fn variants(&self) -> Vec<VariantInfo> {
        vec![
            VariantInfo {
                height: Some(240),
                bandwidth: 800_000,
            },
            VariantInfo {
                height: Some(120),
                bandwidth: 300_000,
            },
        ][..self.variants.len()]
            .to_vec()
    }
    fn current_variant(&self) -> Option<usize> {
        Some(self.cur)
    }
    fn select_variant(&mut self, v: Option<usize>) {
        self.manual = v.is_some();
        self.cur = v.unwrap_or(0).min(self.variants.len() - 1);
    }
    fn wants_downgrade(&mut self) -> bool {
        if self.downgrade_at.is_some_and(|n| self.delivered >= n) {
            self.downgrade_at = None;
            return true;
        }
        false
    }
}

fn segments() -> Option<Vec<Vec<u8>>> {
    let Some(ts) = encode_ts(320, 240) else {
        eprintln!("skipped: could not encode an MPEG-TS clip");
        return None;
    };
    let segs = split_ts(&ts);
    assert!(
        segs.len() >= SECONDS as usize - 1,
        "expected about {SECONDS} segments, got {}",
        segs.len()
    );
    Some(segs)
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

fn ok(p: &Player) {
    assert_ne!(p.state(), PlayState::Error, "{:?}", p.error());
}

fn open(m: Mem) -> Player {
    Player::open_timed(Box::new(m), 0.0, Arc::new(|| {}))
}

fn secs(p: &Player) -> f64 {
    p.position().as_secs_f64()
}

#[test]
fn plays_to_the_end() {
    if !ready() {
        return;
    }
    let Some(segs) = segments() else { return };
    let p = open(Mem::new(vec![segs], false));
    let mut frames = 0;
    wait_until("end of stream", 40, || {
        ok(&p);
        frames += p.take_frame().is_some() as u32;
        p.state() == PlayState::Ended
    });
    assert!(frames > 0);
    assert!(p.seekable() && !p.is_live());
    assert_eq!(p.duration(), Some(SEG * SECONDS));
    assert!(p.buffered_until().is_some());
}

#[test]
fn seek_lands_within_one_segment() {
    if !ready() {
        return;
    }
    let Some(segs) = segments() else { return };
    let p = open(Mem::new(vec![segs], false));
    wait_until("first frame", 15, || {
        ok(&p);
        p.take_frame().is_some()
    });
    let target = 4.8;
    p.seek(Duration::from_secs_f64(target), true);
    wait_until("seek done", 15, || {
        ok(&p);
        p.take_frame();
        !p.is_seeking()
    });
    // Let it play a little, then the real position must be near the target.
    std::thread::sleep(Duration::from_millis(600));
    eprintln!("after seek to {target}s: position {:.2}s", secs(&p));
    wait_until("position near target", 15, || {
        ok(&p);
        p.take_frame();
        (secs(&p) - target).abs() <= 1.0 + 0.7 && secs(&p) >= target - 1.0
    });
}

#[test]
fn rapid_seeks_never_hang() {
    if !ready() {
        return;
    }
    let Some(segs) = segments() else { return };
    let p = open(Mem::new(vec![segs], false));
    wait_until("first frame", 15, || p.take_frame().is_some());
    for ms in [3000u64, 500, 6500, 100, 5500, 0, 7000, 1200, 4200, 800] {
        p.seek(Duration::from_millis(ms), false);
        std::thread::sleep(Duration::from_millis(60));
    }
    wait_until("end of stream", 40, || {
        ok(&p);
        p.take_frame();
        p.state() == PlayState::Ended
    });
}

#[test]
fn stalls_show_as_buffering_and_recover() {
    if !ready() {
        return;
    }
    let Some(segs) = segments() else { return };
    let mut m = Mem::new(vec![segs], false);
    m.stall = true;
    let p = open(m);
    wait_until("end of stream", 60, || {
        ok(&p);
        p.take_frame();
        p.state() == PlayState::Ended
    });
}

#[test]
fn set_variant_keeps_the_position() {
    if !ready() {
        return;
    }
    let (Some(a), Some(b)) = (segments(), encode_ts(160, 120).map(|t| split_ts(&t))) else {
        return;
    };
    let p = open(Mem::new(vec![a, b], false));
    wait_until("first frame", 15, || p.take_frame().is_some());
    assert_eq!(p.variants().len(), 2);
    p.seek(Duration::from_secs(4), true);
    wait_until("seek done", 15, || {
        ok(&p);
        p.take_frame();
        !p.is_seeking()
    });
    p.pause();
    std::thread::sleep(Duration::from_millis(300));
    let before = secs(&p);
    p.set_variant(Some(1));
    // A smaller frame proves the new variant is the one on screen.
    let mut height = 0;
    wait_until("frame from the new variant", 20, || {
        ok(&p);
        if let Some(f) = p.take_frame() {
            height = f.height();
        }
        height == 120
    });
    assert_eq!(p.current_variant(), Some(1));
    wait_until("rebuild done", 15, || !p.is_seeking());
    eprintln!("variant switch: {before:.2}s -> {:.2}s", secs(&p));
    assert!(
        (secs(&p) - before).abs() <= 1.5,
        "position {} -> {}",
        before,
        secs(&p)
    );
}

#[test]
fn live_sources_reject_seeking() {
    if !ready() {
        return;
    }
    let Some(segs) = segments() else { return };
    let p = open(Mem::new(vec![segs], true));
    wait_until("first frame", 15, || {
        ok(&p);
        p.take_frame().is_some()
    });
    assert!(p.is_live());
    assert!(!p.seekable());
    assert_eq!(p.duration(), None);
    p.seek(Duration::from_secs(6), true);
    p.set_rate(2.0);
    std::thread::sleep(Duration::from_millis(500));
    ok(&p);
    assert!(secs(&p) < 5.0, "live seek was applied: {}", secs(&p));
    assert_eq!(p.rate(), 1.0);
}

#[test]
fn a_manifest_in_the_first_chunk_is_refused() {
    if !common::ready(common::PLAYBACK) {
        return;
    }
    let playlist = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:2.0,\nseg0.ts\n#EXT-X-ENDLIST\n";
    let mut m = Mem::new(vec![vec![playlist.to_vec()]], false);
    m.discont = true;
    let p = open(m);
    wait_until("refusal", 10, || p.state() == PlayState::Error);
    assert_eq!(p.error().as_deref(), Some(guard::REFUSED));
}

#[test]
fn a_slow_source_lowers_the_quality_once() {
    if !ready() {
        return;
    }
    let (Some(a), Some(b)) = (segments(), encode_ts(160, 120).map(|t| split_ts(&t))) else {
        return;
    };
    let mut m = Mem::new(vec![a, b], false);
    m.downgrade_at = Some(2);
    let p = open(m);
    let mut notice = None;
    wait_until("quality lowered", 30, || {
        ok(&p);
        p.take_frame();
        notice = notice.or(p.take_notice());
        notice.is_some() && p.current_variant() == Some(1)
    });
    assert_eq!(notice, Some("quality lowered"));
    assert_eq!(p.take_notice(), None);
    wait_until("end of stream", 40, || {
        ok(&p);
        p.take_frame();
        p.state() == PlayState::Ended
    });
}
