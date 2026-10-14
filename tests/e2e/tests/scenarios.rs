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
use jizzcraft_e2e::{FMP4, Svc, TS, WEBM, assert_clean, assert_sentence, fixtures, ready, serial};
use jizzcraft_fetch::config::ResumeOnStart;
use jizzcraft_fetch::errors;
use jizzcraft_fetch::resolver::ResolverDef;
use jizzcraft_fetch::segmented::{AbrConfig, open_hls};
use jizzcraft_fetch::service::{FetchService, Source, StreamOpts, StreamTicket};
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
