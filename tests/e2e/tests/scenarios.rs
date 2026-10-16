//! End-to-end scenarios (WP7). Each test is one claim about the whole stack:
//! loopback server or swarm -> `fetch` -> real `Player` (GStreamer).
//! `cargo xtask e2e` runs them with `--test-threads=1`.
#![cfg(feature = "network")]
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jizzcraft_e2e::drive::{Pump, wake};
use jizzcraft_e2e::http::{Reply, Req, Server};
use jizzcraft_e2e::proc::Baseline;
use jizzcraft_e2e::{
    FMP4, FMP4_AV, Svc, TS, WEBM, assert_clean, assert_sentence, fixtures, ready, serial,
};
use jizzcraft_fetch::config::ResumeOnStart;
use jizzcraft_fetch::download::manager::DownloadRequest;
use jizzcraft_fetch::download::{JobState, JobView};
use jizzcraft_fetch::errors;
use jizzcraft_fetch::resolver::ResolverDef;
use jizzcraft_fetch::segmented::{AbrConfig, open_hls};
use jizzcraft_fetch::service::{ExtractMode, FetchService, Source, StreamOpts, StreamTicket};
use jizzcraft_fetch::{FetchConfig, Indexer, Secret};
use jizzcraft_media::player::{PlayState, Player};

/// One request unit of the range reader.
const UNIT: u64 = jizzcraft_fetch::stream::UNIT;
const MP4: &str = "video/mp4";
const M3U8: &str = "application/vnd.apple.mpegurl";

fn open(s: &Svc, url: String) -> Result<StreamTicket, String> {
    s.svc
        .open_any(url, StreamOpts::default())
        .recv_timeout(Duration::from_secs(30))
        .expect("open_any answered")
}

fn live(t: StreamTicket) -> Player {
    let StreamTicket::Live { reader, len, .. } = t else {
        panic!("expected a seekable live stream");
    };
    Player::open_stream(Box::new(reader), len, 0.0, wake())
}

fn timed(t: StreamTicket) -> Player {
    let StreamTicket::Timed { source, .. } = t else {
        panic!("expected a timed stream");
    };
    Player::open_timed(source, 0.0, wake())
}

fn big_clip() -> Arc<Vec<u8>> {
    fixtures::webm(180, 320, 240, "snow", 2_000_000).expect("encode the big WebM")
}

/// The wait for the first frame that every scenario promises.
const FIRST_FRAME: Duration = Duration::from_secs(20);

fn first_frame(pump: &mut Pump) {
    let t = pump.first_frame(40);
    assert!(t < FIRST_FRAME, "first frame took {t:?}");
}

// ---- 1 ----------------------------------------------------------------------

#[test]
fn s01_range_http_plays_to_the_end() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    let srv = Server::files(vec![("/v/clip.webm", "video/webm", clip.clone())]);
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let t = open(&s, srv.url("/v/clip.webm")).expect("ticket");
        let StreamTicket::Live { len, .. } = &t else {
            panic!("expected a live stream");
        };
        assert_eq!(*len, clip.len() as u64);
        let p = live(t);
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        let tt = Instant::now();
        pump.to_end(60);
        eprintln!("to_end took {:?}", tt.elapsed());
        let n = p.counters().delivered;
        eprintln!(
            "frames: delivered {n}, taken {}, {:?}, dur {:?}",
            pump.frames,
            p.counters(),
            p.duration()
        );
        assert!(n > 100, "only {n} frames");
        assert!(p.duration().is_some_and(|d| d > Duration::from_secs(4)));
        assert!(p.position() >= Duration::from_secs(4));
        assert!(srv.ranged.load(Ordering::SeqCst) > 0, "no Range request");
        assert_eq!(s.downloaded_files(), 0, "a stream wrote to the folder");
        pump.assert_memory();
        drop(p);
    }
    base.assert_settled();
}

// ---- 2 ----------------------------------------------------------------------

#[test]
fn s02_seek_into_data_that_has_not_arrived() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    // Well beyond the 3 MiB the reader keeps ahead of its position, so that
    // the end of the file really is not there when the seek happens.
    let clip = fixtures::webm(240, 320, 240, "snow", 8_000_000).expect("encode the huge WebM");
    let len = clip.len() as u64;
    assert!(len > 6 << 20, "clip too small: {len}");
    let release = Arc::new(AtomicBool::new(false));
    let (c2, r2) = (clip.clone(), release.clone());
    let srv = Server::start(Arc::new(move |r: &Req| {
        let rep = Reply::shared("video/webm", c2.clone());
        match r.range {
            // Head, read-ahead and tail (the index) are served. Any other
            // span is withheld until released.
            Some((a, Some(b))) if a >= 4 << 20 && b + 1 < len && !r2.load(Ordering::SeqCst) => {
                rep.hold(r2.clone())
            }
            _ => rep,
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = live(open(&s, srv.url("/v/clip.webm")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.until("duration", 10, |p| p.duration().is_some());
        p.seek(Duration::from_secs(6), true);
        pump.rebase();
        pump.spin(Duration::from_millis(1500));
        // Waiting, not failing, and not pretending to have arrived.
        assert_ne!(p.state(), PlayState::Ended);
        let before = p.counters().delivered;
        release.store(true, Ordering::SeqCst);
        pump.until("playback near the seek target", 30, |p| {
            p.position() >= Duration::from_millis(5800)
        });
        pump.to_end(40);
        assert!(
            p.counters().delivered > before + 30,
            "no frames after the data arrived"
        );
        assert_eq!(s.downloaded_files(), 0);
        pump.assert_memory();
        drop(p);
    }
    base.assert_settled();
}

// ---- 3 ----------------------------------------------------------------------

#[test]
fn s03_64_kbit_server_still_plays() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    // A small clip: about 8 s of transfer at 8000 B/s, longer than any
    // connect or first-byte timeout, shorter than the stall timeout between
    // chunks. Only a stall may end it.
    let clip = fixtures::webm(150, 320, 240, "ball", 130_000).expect("encode the small WebM");
    let srv = Server::start(Arc::new({
        let c = clip.clone();
        move |_: &Req| Reply::shared("video/webm", c.clone()).bps(8000)
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let t0 = Instant::now();
        let p = live(open(&s, srv.url("/v/slow.webm")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.to_end(90);
        let took = t0.elapsed();
        let floor = Duration::from_secs_f64(clip.len() as f64 / 8000.0 * 0.5);
        eprintln!("64 kbit/s: {} bytes in {took:?}", clip.len());
        assert!(took >= floor, "the server was not slow: {took:?}");
        assert!(pump.frames > 10);
        assert_eq!(s.downloaded_files(), 0);
        drop(p);
    }
    base.assert_settled();
}

// ---- 4 ----------------------------------------------------------------------

#[test]
fn s04_dropped_connection_mid_body_recovers() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    let cuts = Arc::new(AtomicUsize::new(0));
    let (c2, k2) = (clip.clone(), cuts.clone());
    let srv = Server::start(Arc::new(move |r: &Req| {
        let rep = Reply::shared("video/webm", c2.clone());
        match r.range {
            // Two body-carrying requests lose their connection halfway.
            Some((a, _)) if a >= UNIT && k2.fetch_add(1, Ordering::SeqCst) < 2 => rep.cut(),
            _ => rep,
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = live(open(&s, srv.url("/v/clip.webm")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.to_end(90);
        assert!(cuts.load(Ordering::SeqCst) >= 2, "nothing was cut");
        assert!(pump.frames > 20);
        drop(p);
    }
    base.assert_settled();
}

// ---- 5 ----------------------------------------------------------------------

/// Records the last real I/O error a reader returned, so a test can check the
/// sentence the product would show.
struct Spy<R> {
    inner: R,
    last: Arc<Mutex<Option<String>>>,
}

impl<R: Read> Read for Spy<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let r = self.inner.read(buf);
        if let Err(e) = &r
            && e.kind() != std::io::ErrorKind::WouldBlock
        {
            *self.last.lock().unwrap() = Some(e.to_string());
        }
        r
    }
}

impl<R: Seek> Seek for Spy<R> {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(p)
    }
}

#[test]
fn s05_expiring_url_fails_with_the_expired_sentence() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    // The sniff and the probe are served; later requests get 403.
    let srv = Server::start(Arc::new({
        let c = clip.clone();
        move |r: &Req| {
            if r.index >= 3 {
                Reply::status(403)
            } else {
                Reply::shared("video/webm", c.clone())
            }
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let StreamTicket::Live { reader, len, .. } =
            open(&s, srv.url("/v/signed.webm?sig=abc")).expect("ticket")
        else {
            panic!("live");
        };
        let last = Arc::new(Mutex::new(None));
        let spy = Spy {
            inner: reader,
            last: last.clone(),
        };
        let p = Player::open_stream(Box::new(spy), len, 0.0, wake());
        let end = Instant::now() + Duration::from_secs(60);
        while p.state() != PlayState::Error {
            p.take_frame();
            assert!(Instant::now() < end, "the player never gave up");
            std::thread::sleep(Duration::from_millis(10));
        }
        let raw = last.lock().unwrap().clone().expect("a reader error");
        assert_sentence(&raw, errors::EXPIRED);
        if let Some(e) = p.error() {
            assert_clean(&e);
        }
        drop(p);
    }
    base.assert_settled();
}

#[cfg(unix)]
#[test]
fn s05b_expiring_url_recovers_through_the_resolver() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("ran-once");
    let srv_cell: Arc<Mutex<u16>> = Arc::default();
    let srv = Server::start(Arc::new({
        let (c, cell) = (clip.clone(), srv_cell.clone());
        let _ = cell;
        move |r: &Req| match r.path.as_str() {
            "/page" => Reply::ok("text/html", "<html><body>a page</body></html>"),
            // The first address dies after a few requests, the second lives.
            p if p.starts_with("/a/") && r.nth >= 2 => {
                let _ = p;
                Reply::status(403)
            }
            _ => Reply::shared("video/webm", c.clone()),
        }
    }));
    *srv_cell.lock().unwrap() = srv.port;
    let script = format!(
        r#"if [ -f '{ran}' ]; then printf '{{"formats":[{{"url":"{b}"}}]}}'; else : > '{ran}'; printf '{{"formats":[{{"url":"{a}"}}]}}'; fi"#,
        ran = ran.display(),
        a = srv.url("/a/clip.webm"),
        b = srv.url("/b/clip.webm"),
    );
    let def = ResolverDef {
        id: 1,
        name: "mine".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            script,
            "sh".into(),
            "{url}".into(),
        ],
        timeout_secs: 10,
        enabled: true,
        honours_proxy: false,
    };
    let base = Baseline::take();
    {
        let s = Svc::new(FetchConfig {
            resolvers: vec![def],
            ..Default::default()
        });
        let p = live(open(&s, srv.url("/page")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.to_end(90);
        assert!(
            srv.hits_for("/b/clip.webm") > 0,
            "the link was never renewed"
        );
        assert_eq!(s.downloaded_files(), 0);
        drop(p);
    }
    base.assert_settled();
}

// ---- 6 7 8: HLS -------------------------------------------------------------

fn hls_server(
    segs: &[Vec<u8>],
    ext: &str,
    ctype: &'static str,
    extra: &str,
    init: Option<&[u8]>,
) -> Server {
    let mut routes: Vec<(String, &str, Arc<Vec<u8>>)> = vec![(
        "/v/index.m3u8".into(),
        M3U8,
        Arc::new(fixtures::playlist(segs.len(), 1.0, "seg", ext, extra).into_bytes()),
    )];
    for (i, s) in segs.iter().enumerate() {
        routes.push((format!("/v/seg{i}.{ext}"), ctype, Arc::new(s.clone())));
    }
    if let Some(i) = init {
        routes.push(("/v/init.mp4".into(), MP4, Arc::new(i.to_vec())));
    }
    routes.push((
        "/v/key.bin".into(),
        "application/octet-stream",
        Arc::new(KEY.to_vec()),
    ));
    Server::start(Arc::new(move |r: &Req| {
        match routes.iter().find(|(p, _, _)| *p == r.path) {
            Some((_, t, b)) => {
                let mut rep = Reply::shared(t, b.clone());
                rep.ranges = true;
                rep
            }
            None => Reply::status(404),
        }
    }))
}

const KEY: [u8; 16] = *b"0123456789abcdef";

fn play_hls(s: &Svc, srv: &Server, seconds: u32) -> (u64, usize) {
    let p = timed(open(s, srv.url("/v/index.m3u8")).expect("ticket"));
    let mut pump = Pump::new(&p);
    first_frame(&mut pump);
    assert!(p.seekable() && !p.is_live());
    pump.until("duration", 10, |p| p.duration().is_some());
    let d = p.duration().unwrap().as_secs_f64();
    assert!((d - f64::from(seconds)).abs() < 1.5, "duration {d}");
    pump.to_end(60);
    assert_eq!(s.downloaded_files(), 0);
    let r = (pump.frames, srv.accepted.load(Ordering::SeqCst));
    drop(p);
    r
}

#[test]
fn s06_hls_ts_vod_plays_and_seeks_within_a_segment() {
    let _g = serial();
    if !ready(TS) {
        return;
    }
    let segs = fixtures::ts_segments(320, 240, 6).expect("encode TS");
    let srv = hls_server(&segs, "ts", "video/mp2t", "", None);
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = timed(open(&s, srv.url("/v/index.m3u8")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        assert!(p.seekable() && !p.is_live());
        pump.until("duration", 10, |p| p.duration().is_some());
        let target = 3.8;
        p.seek(Duration::from_secs_f64(target), true);
        pump.rebase();
        pump.until("seek done", 20, |p| !p.is_seeking());
        pump.spin(Duration::from_millis(600));
        pump.until("position near the target", 20, |p| {
            let t = p.position().as_secs_f64();
            t >= target - 1.0 && t <= target + 1.7
        });
        pump.to_end(60);
        assert!(pump.frames > 10);
        assert_eq!(s.downloaded_files(), 0);
        pump.assert_memory();
        drop(p);
    }
    base.assert_settled();
}

/// A live HLS playlist that grows one segment per second, then ends.
fn live_server(segs: &[Vec<u8>], window: usize) -> Server {
    let segs: Vec<Arc<Vec<u8>>> = segs.iter().cloned().map(Arc::new).collect();
    let t0 = Instant::now();
    Server::start(Arc::new(move |r: &Req| {
        let n = segs.len();
        // Four segments at once, then one more each second.
        let head = (4 + t0.elapsed().as_secs() as usize).min(n);
        if r.path == "/v/live.m3u8" {
            let first = head.saturating_sub(window);
            let mut p = format!(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n"
            );
            for i in first..head {
                p.push_str(&format!("#EXTINF:1.000,\nseg{i}.ts\n"));
            }
            if head == n {
                p.push_str("#EXT-X-ENDLIST\n");
            }
            return Reply::ok(M3U8, p.into_bytes());
        }
        let found = r
            .path
            .strip_prefix("/v/seg")
            .and_then(|r| r.strip_suffix(".ts"))
            .and_then(|i| i.parse::<usize>().ok())
            .filter(|i| *i < head && *i + window >= head);
        match found {
            Some(i) => Reply::shared("video/mp2t", segs[i].clone()),
            None => Reply::status(404),
        }
    }))
}

#[test]
fn s06b_live_hls_plays_from_the_edge_and_ends_cleanly() {
    let _g = serial();
    if !ready(TS) {
        return;
    }
    let segs = fixtures::ts_segments(320, 240, 7).expect("encode TS");
    let srv = live_server(&segs, 4);
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = timed(open(&s, srv.url("/v/live.m3u8")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        // A live stream has no length and cannot seek.
        assert!(p.is_live() && !p.seekable());
        assert_eq!(p.duration(), None);
        p.seek(Duration::from_secs(3), true);
        // The playlist keeps being reloaded until ENDLIST, then it ends.
        pump.to_end(60);
        assert!(pump.frames > 20, "frames {}", pump.frames);
        assert!(srv.hits_for("/v/live.m3u8") >= 3);
        assert_eq!(s.downloaded_files(), 0);
        pump.assert_memory();
        drop(p);
    }
    // Closing the player closes the reload loop: no request after the end.
    let hits = srv.hits_for("/v/live.m3u8");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(srv.hits_for("/v/live.m3u8"), hits);
    base.assert_settled();
}

#[test]
fn s07_hls_fmp4_with_a_map() {
    let _g = serial();
    if !ready(FMP4) {
        return;
    }
    let f = fixtures::fmp4(320, 240, 6).expect("encode fMP4");
    let (init, frags) = (&f.init, &f.fragments);
    let srv = hls_server(
        frags,
        "m4s",
        MP4,
        "#EXT-X-MAP:URI=\"init.mp4\"\n",
        Some(init),
    );
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let (frames, _) = play_hls(&s, &srv, frags.len() as u32);
        assert!(frames > 10);
        assert!(srv.hits_for("/v/init.mp4") >= 1);
    }
    base.assert_settled();
}

#[test]
fn s08_hls_aes_128() {
    use aes::Aes128;
    use cbc::cipher::block_padding::Pkcs7;
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};
    let _g = serial();
    if !ready(TS) {
        return;
    }
    let segs = fixtures::ts_segments(320, 240, 6).expect("encode TS");
    // No IV in the playlist: the media sequence number is the IV.
    let enc: Vec<Vec<u8>> = segs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let iv = (i as u128).to_be_bytes();
            cbc::Encryptor::<Aes128>::new((&KEY).into(), (&iv).into())
                .encrypt_padded_vec_mut::<Pkcs7>(s)
        })
        .collect();
    assert_ne!(enc[0], segs[0]);
    let srv = hls_server(
        &enc,
        "ts",
        "video/mp2t",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n",
        None,
    );
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let (frames, _) = play_hls(&s, &srv, segs.len() as u32);
        assert!(frames > 10);
        assert!(srv.hits_for("/v/key.bin") >= 1, "the key was never fetched");
    }
    base.assert_settled();
}

// ---- 9 ----------------------------------------------------------------------

#[test]
fn s09_abr_goes_down_when_throttled_and_up_when_released() {
    let _g = serial();
    if !ready(TS) {
        return;
    }
    let secs = 14;
    let (hi, lo) = (
        fixtures::ts_segments(320, 240, secs).expect("encode high"),
        fixtures::ts_segments(160, 120, secs).expect("encode low"),
    );
    let n = hi.len().min(lo.len());
    let avg = hi.iter().map(Vec::len).sum::<usize>() / hi.len();
    // High segments take ~3 s each while the network is poor: far below
    // their declared bitrate and below real time.
    let poor = (avg as u64 / 3).max(1);
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=160x120\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=1500000,RESOLUTION=320x240\nhigh.m3u8\n";
    let (hp, lp) = (
        fixtures::playlist(n, 1.0, "high", "ts", "").into_bytes(),
        fixtures::playlist(n, 1.0, "low", "ts", "").into_bytes(),
    );
    let born = Instant::now();
    let slow_for = Duration::from_secs(6);
    let (hi2, lo2) = (hi.clone(), lo.clone());
    let srv = Server::start(Arc::new(move |r: &Req| {
        let p = r.path.as_str();
        let idx = |pre: &str| {
            p.strip_prefix(pre)
                .and_then(|x| x.strip_suffix(".ts"))
                .and_then(|x| x.parse::<usize>().ok())
        };
        if p == "/master.m3u8" {
            Reply::ok(M3U8, master)
        } else if p == "/low.m3u8" {
            Reply::ok(M3U8, lp.clone())
        } else if p == "/high.m3u8" {
            Reply::ok(M3U8, hp.clone())
        } else if let Some(i) = idx("/low") {
            Reply::ok("video/mp2t", lo2[i].clone())
        } else if let Some(i) = idx("/high") {
            let rep = Reply::ok("video/mp2t", hi2[i].clone());
            if born.elapsed() < slow_for {
                rep.bps(poor)
            } else {
                rep
            }
        } else {
            Reply::status(404)
        }
    }));
    let base = Baseline::take();
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        // The product thresholds are tuned for minutes; the scenario has 14 s.
        let cfg = AbrConfig {
            down_gap: Duration::from_secs(1),
            up_buffer: Duration::from_secs(3),
            up_hold: Duration::from_secs(1),
            up_gap: Duration::from_secs(2),
            ..AbrConfig::default()
        };
        let _svc = Svc::plain(); // publishes the default proxy policy
        let r = rt
            .block_on(open_hls(
                rt.handle().clone(),
                reqwest::Client::new(),
                srv.url("/master.m3u8"),
                Default::default(),
                Vec::new(),
            ))
            .expect("open hls")
            .with_abr(cfg);
        use jizzcraft_core::timed::TimedSource;
        assert_eq!(
            r.current_variant(),
            Some(1),
            "it must start on the best one"
        );
        let p = Player::open_timed(Box::new(r), 0.0, wake());
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        assert_eq!(p.variants().len(), 2);
        let mut seen = vec![p.current_variant().unwrap()];
        pump.until("the end", 90, |p| {
            let v = p.current_variant().unwrap();
            if seen.last() != Some(&v) {
                seen.push(v);
            }
            p.state() == PlayState::Ended
        });
        eprintln!("variants seen: {seen:?}");
        assert!(
            seen.windows(2).any(|w| w == [1, 0]) && seen.ends_with(&[1]),
            "expected high, low, high; got {seen:?}"
        );
        drop(p);
        drop(rt);
    }
    base.assert_settled();
}

// ---- 10 ---------------------------------------------------------------------

#[test]
fn s10_dash_segment_template_muxed() {
    let _g = serial();
    if !ready(FMP4) {
        return;
    }
    let f = fixtures::fmp4(320, 240, 6).expect("encode fMP4");
    let (init, frags) = (&f.init, &f.fragments);
    let mpd = format!(
        r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT{n}S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
<Period><AdaptationSet mimeType="video/mp4" segmentAlignment="true">
<Representation id="v1" bandwidth="800000" width="320" height="240" codecs="avc1.42c01e">
<SegmentTemplate timescale="1" duration="1" startNumber="0" initialization="init.mp4" media="c$Number$.m4s"/>
</Representation></AdaptationSet></Period></MPD>"#,
        n = frags.len()
    );
    let mut routes: Vec<(String, &str, Arc<Vec<u8>>)> = vec![
        (
            "/v/manifest.mpd".into(),
            "application/dash+xml",
            Arc::new(mpd.into_bytes()),
        ),
        ("/v/init.mp4".into(), MP4, Arc::new(init.clone())),
    ];
    for (i, fr) in frags.iter().enumerate() {
        routes.push((format!("/v/c{i}.m4s"), MP4, Arc::new(fr.clone())));
    }
    let srv = Server::start(Arc::new(move |r: &Req| {
        match routes.iter().find(|(p, _, _)| *p == r.path) {
            Some((_, t, b)) => Reply::shared(t, b.clone()),
            None => Reply::status(404),
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = timed(open(&s, srv.url("/v/manifest.mpd")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.until("duration", 10, |p| p.duration().is_some());
        pump.to_end(60);
        assert!(pump.frames > 10);
        assert_eq!(s.downloaded_files(), 0);
        drop(p);
    }
    base.assert_settled();
}

#[test]
fn s10b_dash_separate_audio_and_video_tracks() {
    let _g = serial();
    if !ready(FMP4_AV) {
        return;
    }
    let v = fixtures::fmp4(320, 240, 6).expect("encode fMP4 video");
    let a = fixtures::fmp4_audio(6).expect("encode fMP4 audio");
    let n = v.fragments.len().min(a.fragments.len());
    let mpd = format!(
        r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT{n}S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
<Period><AdaptationSet mimeType="video/mp4" segmentAlignment="true">
<Representation id="v1" bandwidth="800000" width="320" height="240" codecs="avc1.42c01e">
<SegmentTemplate timescale="1" duration="1" startNumber="0" initialization="vinit.mp4" media="v$Number$.m4s"/>
</Representation></AdaptationSet>
<AdaptationSet mimeType="audio/mp4" lang="en"><Representation id="a1" bandwidth="64000" codecs="mp4a.40.2">
<SegmentTemplate timescale="1" duration="1" startNumber="0" initialization="ainit.mp4" media="a$Number$.m4s"/>
</Representation></AdaptationSet></Period></MPD>"#
    );
    let mut routes: Vec<(String, &str, Arc<Vec<u8>>)> = vec![
        (
            "/v/manifest.mpd".into(),
            "application/dash+xml",
            Arc::new(mpd.into_bytes()),
        ),
        ("/v/vinit.mp4".into(), MP4, Arc::new(v.init.clone())),
        ("/v/ainit.mp4".into(), MP4, Arc::new(a.init.clone())),
    ];
    for i in 0..n {
        routes.push((
            format!("/v/v{i}.m4s"),
            MP4,
            Arc::new(v.fragments[i].clone()),
        ));
        routes.push((
            format!("/v/a{i}.m4s"),
            MP4,
            Arc::new(a.fragments[i].clone()),
        ));
    }
    let srv = Server::start(Arc::new(move |r: &Req| {
        match routes.iter().find(|(p, _, _)| *p == r.path) {
            Some((_, t, b)) => Reply::shared(t, b.clone()),
            None => Reply::status(404),
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = timed(open(&s, srv.url("/v/manifest.mpd")).expect("ticket"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.until("duration", 10, |p| p.duration().is_some());
        // A seek lands on both tracks, then the clip plays to the end.
        p.seek(Duration::from_secs(2), true);
        pump.to_end(60);
        assert!(pump.frames > 10, "frames {}", pump.frames);
        assert!(srv.hits_for("/v/ainit.mp4") >= 1, "audio never requested");
        assert!(srv.hits_for("/v/a3.m4s") >= 1);
        assert_eq!(s.downloaded_files(), 0);
        pump.assert_memory();
        drop(p);
    }
    base.assert_settled();
}

// ---- 11 13: swarm -----------------------------------------------------------

fn file_in(dir: &std::path::Path, bytes: &[u8]) -> Source {
    let p = dir.join("t.torrent");
    std::fs::write(&p, bytes).unwrap();
    Source::TorrentFile(p)
}

fn wait_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn s11_three_peer_swarm_plays_while_downloading() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    let sw = jizzcraft_e2e::swarm::swarm(3, 100 * 1024, clip.clone());
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let listed = s
            .svc
            .list_files_with_peers(file_in(s.st.path(), &sw.torrent), sw.peers.clone())
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("list");
        let id = s
            .svc
            .add_listed(listed, Some(vec![0]))
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("add");
        let t = s
            .svc
            .open_reader(id, 0)
            .recv_timeout(Duration::from_secs(40))
            .unwrap()
            .expect("reader");
        let StreamTicket::Live {
            reader,
            len,
            status,
            ..
        } = t
        else {
            panic!("the file cannot be complete yet");
        };
        let status = status.expect("a torrent has a status");
        let p = Player::open_stream(Box::new(reader), len, 0.0, wake());
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        s.svc.list();
        std::thread::sleep(Duration::from_millis(600));
        let at_first = s.svc.list().first().map(|t| t.progress()).unwrap_or(0.0);
        eprintln!("progress at the first frame: {at_first:.2}");
        assert!(at_first < 0.95, "it was not play-while-downloading");

        // Player pause and resume: the picture stops, then moves again.
        p.pause();
        pump.spin(Duration::from_millis(400));
        let a = p.position();
        pump.spin(Duration::from_millis(700));
        let b = p.position();
        assert!(
            b.saturating_sub(a) < Duration::from_millis(150),
            "{a:?} -> {b:?}"
        );
        p.play();
        pump.until("movement after resume", 20, |p| {
            p.position() > b + Duration::from_millis(300)
        });

        // The torrent itself paused and resumed: reported, then moving.
        s.svc.pause(id);
        wait_for(10, "paused flag", || status.paused.load(Ordering::Relaxed));
        s.svc.resume(id);
        wait_for(10, "resumed flag", || {
            !status.paused.load(Ordering::Relaxed)
        });

        // A seek into pieces that are still missing.
        pump.until("duration", 10, |p| p.duration().is_some());
        p.seek(Duration::from_millis(4500), true);
        pump.rebase();
        pump.until("playback near the seek target", 60, |p| {
            p.position() >= Duration::from_millis(4200)
        });
        pump.to_end(60);
        pump.assert_memory();
        assert!(status.error().is_none());
        drop(p);
    }
    base.assert_settled();
}

// ---- 12 ---------------------------------------------------------------------

#[test]
fn s12_magnet_with_no_peers_gives_a_sentence_soon() {
    let _g = serial();
    let base = Baseline::take();
    {
        let s = Svc::plain();
        s.svc.set_metadata_timeout(Duration::from_secs(3));
        let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        let t0 = Instant::now();
        let (rx, probe) = s
            .svc
            .list_files_probed(Source::Magnet(magnet.into()), Vec::new());
        let raw = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .err()
            .expect("must fail");
        assert!(t0.elapsed() < Duration::from_secs(15), "{:?}", t0.elapsed());
        let sentence = errors::classify(&raw);
        eprintln!("no peers: {raw:?} -> {sentence}");
        assert!(
            sentence == errors::NO_PEERS || sentence == errors::METADATA_TIMEOUT,
            "raw {raw:?} -> {sentence:?}"
        );
        assert_clean(sentence);
        assert!(
            !raw.contains("0123456789abcdef"),
            "the info hash is in the error"
        );
        assert_eq!(probe.peers(), 0);
    }
    base.assert_settled();
}

// ---- 13 ---------------------------------------------------------------------

#[test]
fn s13_restart_keeps_the_torrent_and_ask_does_not_resume_it() {
    use jizzcraft_fetch::service::TorrentState;
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let sw = jizzcraft_e2e::swarm::swarm(1, 100 * 1024, big_clip());
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let make = || {
        FetchService::new(
            FetchConfig {
                download_dir: Some(dl.path().into()),
                dht: false,
                resume_on_start: ResumeOnStart::Ask,
                ..Default::default()
            },
            st.path().join("state"),
        )
        .unwrap()
    };
    let base = Baseline::take();
    {
        let svc = make();
        let listed = svc
            .list_files_with_peers(file_in(st.path(), &sw.torrent), sw.peers.clone())
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        svc.add_listed(listed, None)
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        wait_for(30, "some progress", || {
            svc.list()
                .first()
                .is_some_and(|t| t.progress_bytes > 50_000)
        });
        // Let the persistence file settle before the "crash".
        std::thread::sleep(Duration::from_secs(1));
    }
    {
        let svc = make();
        svc.start()
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        wait_for(10, "torrent restored", || !svc.list().is_empty());
        wait_for(10, "restored torrent paused", || {
            svc.list()
                .first()
                .is_some_and(|t| t.state == TorrentState::Paused)
        });
        let first = svc.list()[0].clone();
        std::thread::sleep(Duration::from_secs(2));
        svc.list();
        std::thread::sleep(Duration::from_millis(300));
        let again = svc.list()[0].clone();
        assert_eq!(again.state, TorrentState::Paused, "it woke up by itself");
        assert!(again.progress_bytes.abs_diff(first.progress_bytes) < 64 * 1024);
        let n = svc
            .resume_all()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert_eq!(n, 1);
        wait_for(10, "torrent running again", || {
            svc.list()
                .first()
                .is_some_and(|t| t.state != TorrentState::Paused)
        });
    }
    drop(sw);
    base.assert_settled();
}

// ---- 14 ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn s14_kill_switch_means_zero_connections() {
    let _g = serial();
    // Everything here points at one counted loopback server.
    let srv = Server::start(Arc::new(|_: &Req| {
        Reply::ok(
            "application/json",
            r#"{"data":[{"name":"Clip","play":"http://127.0.0.1:1/x.mp4"}]}"#,
        )
    }));
    let marker = tempfile::tempdir().unwrap();
    let ran = marker.path().join("resolver-ran");
    let resolver = ResolverDef {
        id: 1,
        name: "r".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(": > '{}'; exit 1", ran.display()),
            "sh".into(),
            "{url}".into(),
        ],
        timeout_secs: 5,
        enabled: true,
        honours_proxy: false,
    };
    let mut def = jizzcraft_fetch::sources::parse_import(
        format!(
            r#"{{"name":"mine","search_url":"{}?q={{query}}","items":"/data","fields":{{"title":"/name","media":"/play"}}}}"#,
            srv.url("/s")
        )
        .as_bytes(),
    )
    .unwrap();
    def.id = 1;
    let torrent = {
        // Real metainfo, so that only the policy can say no.
        let sw = jizzcraft_e2e::swarm::swarm(1, 1024 * 1024, Arc::new(vec![7u8; 100_000]));
        sw.torrent.clone()
    };
    let base = Baseline::take();
    {
        let s = Svc::new(FetchConfig {
            require_proxy: true,
            proxy: None,
            resolvers: vec![resolver],
            sources: vec![def],
            indexers: vec![Indexer {
                id: 1,
                name: "ix".into(),
                url: srv.url("/api"),
                api_key: Secret::new("K"),
                enabled: true,
            }],
            ..Default::default()
        });
        let wait = Duration::from_secs(20);
        let blocked = |r: Result<StreamTicket, String>, what: &str| {
            let e = r.err().unwrap_or_else(|| panic!("{what} was allowed"));
            assert_sentence(&e, errors::PROXY_REQUIRED);
        };
        blocked(open(&s, srv.url("/m.m3u8")), "open_any");
        blocked(open(&s, srv.url("/page")), "open_any (page, resolver)");
        blocked(
            s.svc
                .stream_url(srv.url("/v/clip.mp4"))
                .recv_timeout(wait)
                .unwrap(),
            "stream_url",
        );
        blocked(
            s.svc
                .play_http(srv.url("/v/clip.mp4"))
                .recv_timeout(wait)
                .unwrap(),
            "play_http",
        );
        blocked(
            s.svc
                .stream_via_resolver(1, srv.url("/page"))
                .recv_timeout(wait)
                .unwrap(),
            "stream_via_resolver",
        );
        // The page reader and the save queue obey the switch too.
        for mode in [
            ExtractMode::Auto,
            ExtractMode::Builtin,
            ExtractMode::Resolvers,
        ] {
            let e = s
                .svc
                .extract(srv.url("/page"), mode)
                .recv_timeout(wait)
                .unwrap()
                .expect_err("a page was read with no proxy");
            assert_sentence(&e, errors::PROXY_REQUIRED);
        }
        for p in ["/v/clip.mp4", "/page", "/v/index.m3u8", "/v/s.mpd"] {
            s.svc
                .download_start(DownloadRequest::url(srv.url(p)))
                .unwrap();
        }
        let t0 = Instant::now();
        loop {
            let l = s.svc.download_list();
            if l.len() == 4 && l.iter().all(|j| j.state.is_over()) {
                for j in &l {
                    assert_eq!(j.state, JobState::Failed);
                    assert_eq!(j.error, Some(errors::PROXY_REQUIRED));
                }
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "saves never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(s.downloaded_files(), 0);
        let out = s
            .svc
            .search(None, "q", &[], 10, Arc::default())
            .recv_timeout(wait)
            .unwrap();
        assert_eq!(out.queried, 2);
        assert_eq!(out.failed, out.queried, "a search was answered");
        assert!(out.results.is_empty());
        // Torrents: with the server's address as a peer, and as a tracker-less
        // magnet.
        let r = s
            .svc
            .list_files_with_peers(file_in(s.st.path(), &torrent), vec![srv.addr()])
            .recv_timeout(wait)
            .unwrap();
        assert!(r.is_err(), "a torrent was listed with no proxy");
        let (rx, _p) = s.svc.list_files_probed(
            Source::Magnet("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into()),
            vec![srv.addr()],
        );
        assert!(rx.recv_timeout(wait).unwrap().is_err());
        std::thread::sleep(Duration::from_millis(500));
        assert!(!ran.exists(), "a resolver was started without a proxy");
    }
    srv.quiesce();
    assert_eq!(
        srv.accepted.load(Ordering::SeqCst),
        0,
        "a socket reached the server: {:?}",
        srv.paths()
    );
    base.assert_settled();
}

// ---- 15 ---------------------------------------------------------------------

/// Env var that carries the mount point into the re-executed test binary.
const FULL_DIR: &str = "JIZZCRAFT_E2E_FULLDISK_DIR";

/// Fill `dir` until the write fails with ENOSPC, then give `left` bytes back.
fn fill_all_but(dir: &std::path::Path, left: u64) -> std::path::PathBuf {
    use std::io::Write;
    let p = dir.join("filler.bin");
    let mut f = std::fs::File::create(&p).unwrap();
    let chunk = vec![0u8; 16 * 1024];
    let mut n = 0u64;
    while f.write_all(&chunk).is_ok() {
        n += chunk.len() as u64;
    }
    let _ = f.flush();
    f.set_len(n.saturating_sub(left)).unwrap();
    p
}

/// The raw status label of torrent `id` (`"disk full"`, `"error"`, or empty).
fn t_err(svc: &FetchService, id: usize) -> String {
    svc.list()
        .into_iter()
        .find(|t| t.id == id)
        .and_then(|t| t.error)
        .unwrap_or_default()
}

fn tree_files(p: &std::path::Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(d) = std::fs::read_dir(p) {
        for e in d.flatten() {
            let q = e.path();
            if q.is_dir() {
                v.extend(tree_files(&q));
            } else {
                v.push(q.display().to_string());
            }
        }
    }
    v
}

/// The scenario itself. Runs inside a user+mount namespace where `dir` is a
/// 4 MiB tmpfs, so ENOSPC is real and no privilege is needed.
fn disk_full_body(dir: &std::path::Path) {
    if !ready(WEBM) {
        return;
    }
    let dl = dir.join("dl");
    std::fs::create_dir(&dl).unwrap();

    // 1. An HTTP source without Range is downloaded to a `.part` file: the
    // volume fills up halfway.
    let body = big_clip();
    assert!(body.len() > 512 * 1024 && body.len() < 3 * 1024 * 1024);
    let b2 = body.clone();
    let srv = Server::start(Arc::new(move |_: &Req| {
        let mut r = Reply::shared("video/webm", b2.clone());
        r.ranges = false;
        r
    }));
    let filler = fill_all_but(dir, body.len() as u64 / 2);
    let base = Baseline::take();
    {
        let st = tempfile::tempdir().unwrap();
        let svc = FetchService::new(
            FetchConfig {
                download_dir: Some(dl.clone()),
                dht: false,
                ..Default::default()
            },
            st.path().join("state"),
        )
        .unwrap();
        let ticket = svc
            .open_any(srv.url("/v/clip.webm"), StreamOpts::default())
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("ticket");
        let StreamTicket::Live {
            mut reader, len, ..
        } = ticket
        else {
            panic!("expected a live download");
        };
        assert_eq!(len, body.len() as u64);
        let mut sink = vec![0u8; 64 * 1024];
        let err = loop {
            match reader.read(&mut sink) {
                Ok(0) => panic!("the download finished on a full disk"),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => break e,
            }
        };
        assert_sentence(&err.to_string(), errors::DISK_FULL);
        drop(reader);
        wait_for(10, "partial file removed", || tree_files(&dl).is_empty());

        // 2. Space comes back: the same address now downloads completely.
        std::fs::remove_file(&filler).unwrap();
        let ticket = svc
            .open_any(srv.url("/v/clip.webm"), StreamOpts::default())
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("ticket after freeing space");
        let StreamTicket::Live { mut reader, .. } = ticket else {
            panic!("expected a live download");
        };
        let mut got = Vec::new();
        let t0 = Instant::now();
        while got.len() < body.len() {
            assert!(t0.elapsed() < Duration::from_secs(30), "retry too slow");
            match reader.read(&mut sink) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&sink[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("retry failed: {}", errors::classify(&e.to_string())),
            }
        }
        assert!(got == *body, "resumed download differs");
        drop(reader);
        wait_for(10, "final file in place", || {
            let f = tree_files(&dl);
            f.len() == 1 && !f[0].ends_with(".part")
        });
        assert_eq!(std::fs::read(&tree_files(&dl)[0]).unwrap(), *body);
    }
    drop(srv);
    base.assert_settled();
    for f in tree_files(&dl) {
        std::fs::remove_file(f).unwrap();
    }

    // 2b. The library database on the same volume: a write that does not fit
    // fails with SQLITE_FULL, the file stays consistent and writes work again
    // once space is back.
    {
        use jizzcraft_core::db::Db;
        let db = Db::open(&dir.join("lib.db")).expect("open db");
        let c = db.conn_for_tests();
        c.execute("CREATE TABLE IF NOT EXISTS pad (b BLOB)", [])
            .unwrap();
        let filler = fill_all_but(dir, 64 * 1024);
        let blob = vec![7u8; 256 * 1024];
        let mut failed = None;
        for _ in 0..8 {
            if let Err(e) = c.execute("INSERT INTO pad VALUES (?1)", [&blob]) {
                failed = Some(e.to_string());
                break;
            }
        }
        let e = failed.expect("a full volume must fail a write");
        assert!(
            e.to_ascii_lowercase().contains("full") || e.contains("disk"),
            "unexpected database error"
        );
        db.quick_check().expect("database damaged by a full disk");
        std::fs::remove_file(&filler).unwrap();
        c.execute("INSERT INTO pad VALUES (?1)", [&blob])
            .expect("write after freeing space");
        db.quick_check().expect("database damaged after recovery");
    }
    for f in tree_files(dir) {
        if !f.starts_with(&*dl.display().to_string()) {
            let _ = std::fs::remove_file(f);
        }
    }

    // 3. A torrent on the same volume. The free-space query is made to lie
    // (plenty of room) so that the preflight and the monitor let the download
    // run into a real ENOSPC: the engine then asserts on the partial write.
    // That must not take the process down, and the torrent must end up
    // paused with the typed "disk full" status, then finish once space is
    // back.
    {
        use std::sync::atomic::AtomicBool;
        let clip = big_clip();
        let sw = jizzcraft_e2e::swarm::swarm(2, 100 * 1024, clip.clone());
        let st = tempfile::tempdir().unwrap();
        let svc = FetchService::new(
            FetchConfig {
                download_dir: Some(dl.clone()),
                dht: false,
                ..Default::default()
            },
            st.path().join("state"),
        )
        .unwrap();
        svc.set_stall_window(Duration::from_secs(3));
        // The volume is smaller than the usual 64 MiB margin.
        svc.set_run_margin(0);
        let lie = Arc::new(AtomicBool::new(true));
        let l2 = lie.clone();
        svc.set_free_space_fn(move |p| {
            if l2.load(Ordering::Relaxed) {
                Some(u64::MAX / 2)
            } else {
                jizzcraft_fetch::diskspace::free_bytes(p)
            }
        });
        let listed = svc
            .list_files_with_peers(file_in(st.path(), &sw.torrent), sw.peers.clone())
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("list");
        let filler = fill_all_but(dir, 300 * 1024);
        let id = svc
            .add_listed(listed, None)
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("add");
        // The volume really fills up under the running torrent.
        wait_for(60, "ENOSPC reached", || {
            jizzcraft_fetch::diskspace::free_bytes(dir).is_some_and(|f| f < 32 * 1024)
        });
        svc.list();
        std::thread::sleep(Duration::from_secs(1));
        let t = svc.list().into_iter().find(|t| t.id == id).unwrap();
        assert!(
            t.progress_bytes < t.total_bytes,
            "it finished on a full disk"
        );

        // The engine is told the truth; the watchdog does the rest.
        lie.store(false, Ordering::Relaxed);
        wait_for(30, "typed disk full", || {
            svc.list().iter().any(|t| {
                t.id == id
                    && t.state == jizzcraft_fetch::service::TorrentState::Paused
                    && t.error.as_deref() == Some("disk full")
            })
        });
        assert_sentence(t_err(&svc, id).as_str(), errors::DISK_FULL);
        // With no room it refuses to start again, and says why.
        svc.resume(id);
        std::thread::sleep(Duration::from_millis(1500));
        svc.list();
        std::thread::sleep(Duration::from_millis(300));
        let t = svc.list().into_iter().find(|t| t.id == id).unwrap();
        assert_eq!(t.error.as_deref(), Some("disk full"));
        assert_eq!(t.state, jizzcraft_fetch::service::TorrentState::Paused);

        // Space comes back: it resumes and completes.
        std::fs::remove_file(&filler).unwrap();
        svc.resume(id);
        wait_for(90, "torrent complete after freeing space", || {
            svc.list()
                .iter()
                .any(|t| t.total_bytes > 0 && t.progress_bytes == t.total_bytes)
        });
        // (A torrent that the engine had put in its error state is added
        // again over its own files, so its id may have changed.)
        let t = svc.list().into_iter().next().unwrap();
        assert!(t.error.is_none(), "{:?}", t.error);
        assert_eq!(
            std::fs::read(dl.join(jizzcraft_e2e::swarm::FILE_NAME)).unwrap(),
            *clip,
            "resumed torrent differs"
        );
    }
}

/// Disk full, for real: a small tmpfs inside an unprivileged user+mount
/// namespace (`unshare -Urm`). The test binary re-executes itself in there,
/// so no root, no loop device and no change to the host.
///
/// It skips, with a printed reason, where user namespaces are unavailable;
/// `JIZZCRAFT_REQUIRE_NAMESPACE=1` (CI) turns that skip into a failure.
#[cfg(target_os = "linux")]
#[test]
fn s15_disk_full_is_a_sentence_and_leaves_nothing_behind() {
    let _g = serial();
    if let Ok(dir) = std::env::var(FULL_DIR) {
        disk_full_body(std::path::Path::new(&dir));
        return;
    }
    let skip = |why: &str| {
        if std::env::var("JIZZCRAFT_REQUIRE_NAMESPACE").is_ok_and(|v| v == "1") {
            panic!("JIZZCRAFT_REQUIRE_NAMESPACE=1 and the scenario cannot run: {why}");
        }
        eprintln!("skipped: {why}");
    };
    let probe = std::process::Command::new("unshare")
        .args([
            "-Urm",
            "sh",
            "-c",
            "d=$(mktemp -d) && mount -t tmpfs -o size=1m tmpfs \"$d\"",
        ])
        .output();
    match probe {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return skip(&format!(
                "cannot mount a tmpfs in a user namespace: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Err(e) => return skip(&format!("unshare not available: {e}")),
    }
    let mnt = tempfile::tempdir().unwrap();
    let me = std::env::current_exe().unwrap();
    let out = std::process::Command::new("unshare")
        .args(["-Urm", "sh", "-c"])
        .arg("mount -t tmpfs -o size=4m tmpfs \"$1\" && shift && exec \"$@\"")
        .arg("sh")
        .arg(mnt.path())
        .arg(&me)
        .args([
            "s15_disk_full_is_a_sentence_and_leaves_nothing_behind",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(FULL_DIR, mnt.path())
        .output()
        .expect("run the namespaced test");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "disk-full scenario failed:\n{text}");
    if text.contains("skipped:") {
        eprintln!("skipped inside the namespace (see reason above)");
    }
}

// ---- 16 - 19: the page reader and the save queue ----------------------------

fn page(body: &str) -> Reply {
    Reply::ok("text/html", body.as_bytes())
}

/// Wait until every listed job is over; returns the final list.
fn settle(s: &Svc, secs: u64) -> Vec<JobView> {
    let t0 = Instant::now();
    loop {
        let l = s.svc.download_list();
        if !l.is_empty() && l.iter().all(|j| j.state.is_over()) {
            return l;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "saves never ended"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn saved(s: &Svc) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(s.dl.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

#[test]
fn s16_page_og_video_plays_through_the_builtin_reader() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    let c2 = clip.clone();
    let srv = Server::start(Arc::new(move |r: &Req| match r.path.as_str() {
        "/watch" => page(
            r#"<html><head><meta property="og:title" content="A Page Clip">
               <meta property="og:video" content="/v/clip.webm"></head><body>hi</body></html>"#,
        ),
        "/v/clip.webm" => Reply::shared("video/webm", c2.clone()),
        _ => Reply::status(404),
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        let p = live(open(&s, srv.url("/watch")).expect("ticket from a page"));
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.to_end(90);
        assert!(pump.frames > 20);
        assert!(srv.hits_for("/watch") >= 1 && srv.hits_for("/v/clip.webm") >= 1);
        // Watching is not saving.
        assert_eq!(s.downloaded_files(), 0);
        drop(p);
    }
    base.assert_settled();
}

#[test]
fn s17_page_hls_is_saved_imported_and_plays_from_the_file() {
    let _g = serial();
    if !ready(TS) {
        return;
    }
    let segs = fixtures::ts_segments(320, 240, 3).expect("encode TS segments");
    let mut routes: Vec<(String, &str, Arc<Vec<u8>>)> = vec![
        (
            "/watch".into(),
            "text/html",
            Arc::new(
                br#"<meta property="og:title" content="Stream Page">
                    <meta property="og:video" content="/v/index.m3u8">"#
                    .to_vec(),
            ),
        ),
        (
            "/v/index.m3u8".into(),
            M3U8,
            Arc::new(fixtures::playlist(segs.len(), 1.0, "seg", "ts", "").into_bytes()),
        ),
    ];
    for (i, sg) in segs.iter().enumerate() {
        routes.push((format!("/v/seg{i}.ts"), "video/mp2t", Arc::new(sg.clone())));
    }
    let srv = Server::start(Arc::new(move |r: &Req| {
        match routes.iter().find(|(p, _, _)| *p == r.path) {
            Some((_, t, b)) => Reply::shared(t, b.clone()),
            None => Reply::status(404),
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        s.svc
            .download_start(DownloadRequest::url(srv.url("/watch")))
            .unwrap();
        let l = settle(&s, 60);
        assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
        let files = saved(&s);
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].file_name().unwrap(), "Stream Page.ts");
        let want: Vec<u8> = segs.iter().flatten().copied().collect();
        assert_eq!(std::fs::read(&files[0]).unwrap(), want);
        let fin = s.svc.download_take_finished();
        assert_eq!((fin.done, fin.failed), (1, 0));
        // Into the library, counts only, then played from the file.
        let mut db = jizzcraft_core::Db::open_in_memory().unwrap();
        let rep = jizzcraft_fetch::commands::import_saved(&mut db, s.dl.path()).unwrap();
        assert_eq!(rep.added, 1);
        let p = Player::open(&files[0], 0.0, wake());
        let mut pump = Pump::new(&p);
        first_frame(&mut pump);
        pump.to_end(60);
        assert!(pump.frames > 10);
        drop(p);
    }
    srv.quiesce();
    base.assert_settled();
}

#[test]
fn s18_save_resumes_after_a_cut_and_bytes_match() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    let clip = big_clip();
    let cuts = Arc::new(AtomicUsize::new(0));
    let (c2, k2) = (clip.clone(), cuts.clone());
    let srv = Server::start(Arc::new(move |r: &Req| {
        let rep = Reply::shared("video/webm", c2.clone());
        // The first big body loses its connection halfway.
        let big = r
            .range
            .is_none_or(|(a, b)| b.is_none_or(|b| b - a > 64 * 1024));
        if big && k2.fetch_add(1, Ordering::SeqCst) == 0 {
            rep.cut()
        } else {
            rep
        }
    }));
    let base = Baseline::take();
    {
        let s = Svc::plain();
        s.svc
            .download_start(DownloadRequest::url(srv.url("/v/clip.webm")))
            .unwrap();
        let l = settle(&s, 60);
        assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
        assert!(cuts.load(Ordering::SeqCst) >= 1, "nothing was cut");
        let files = saved(&s);
        assert_eq!(files.len(), 1, "no .part or stray file left: {files:?}");
        assert!(
            std::fs::read(&files[0]).unwrap() == *clip,
            "the saved bytes differ"
        );
    }
    srv.quiesce();
    base.assert_settled();
}

#[test]
fn s19_low_disk_pauses_the_save_with_a_sentence_and_it_resumes() {
    let _g = serial();
    if !ready(WEBM) {
        return;
    }
    // The volume is simulated by the free-space probe the queue asks every
    // second; the real ENOSPC path is s15's. Nothing is deleted on a pause.
    let clip = big_clip();
    let srv = {
        let c2 = clip.clone();
        Server::start(Arc::new(move |_: &Req| {
            let mut r = Reply::shared("video/webm", c2.clone()).bps(500_000);
            r.ranges = false;
            r
        }))
    };
    let low = Arc::new(AtomicBool::new(false));
    let base = Baseline::take();
    {
        let l2 = low.clone();
        let mut s = Svc::plain();
        s.svc = s.svc.with_free_space(Arc::new(move |_| {
            Some(if l2.load(Ordering::SeqCst) {
                1024 * 1024
            } else {
                u64::MAX / 4
            })
        }));
        let id = s
            .svc
            .download_start(DownloadRequest::url(srv.url("/v/clip.webm")))
            .unwrap()[0];
        let job = |s: &Svc| {
            s.svc
                .download_list()
                .into_iter()
                .find(|j| j.id == id)
                .unwrap()
        };
        wait_for(20, "bytes moving", || job(&s).done_bytes > 20_000);
        low.store(true, Ordering::SeqCst);
        wait_for(20, "paused for lack of room", || {
            job(&s).state == JobState::Paused
        });
        let j = job(&s);
        assert_eq!(j.error, Some(errors::DISK_FULL));
        assert_clean(j.error.unwrap());
        // What was written is kept, as one .part file.
        let files = saved(&s);
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].to_string_lossy().ends_with(".part"));
        let at = job(&s).done_bytes;
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(job(&s).done_bytes, at, "a paused save kept writing");
        // Room is back.
        low.store(false, Ordering::SeqCst);
        assert!(s.svc.download_resume(id));
        let l = settle(&s, 60);
        assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
        let files = saved(&s);
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(std::fs::read(&files[0]).unwrap() == *clip, "bytes differ");
    }
    srv.quiesce();
    base.assert_settled();
}
