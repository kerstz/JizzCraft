//! Segmented playback (HLS/DASH): tiny local servers, synthetic bytes.
#![cfg(feature = "network")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use jizzcraft_core::timed::{Chunk, TimedSource};
use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::errors;
use jizzcraft_fetch::pick::{Prefer, Quality};
use jizzcraft_fetch::segmented::{SegmentedReader, open_dash, open_hls};
use jizzcraft_fetch::service::FetchService;

/// The proxy policy is process-global, so these tests run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Publish a default policy (kill switch off) for the process.
fn reset_policy() {
    let d = tempfile::tempdir().unwrap();
    let _ = FetchService::new(
        FetchConfig {
            dht: false,
            ..Default::default()
        },
        d.path().into(),
    )
    .unwrap();
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

fn ok(body: impl Into<Vec<u8>>) -> Resp {
    Resp {
        status: 200,
        body: body.into(),
    }
}

fn status(code: u16) -> Resp {
    Resp {
        status: code,
        body: Vec::new(),
    }
}

type Handler = Arc<dyn Fn(&str) -> Resp + Send + Sync>;

/// Tiny HTTP/1.1 server. `Range` is honoured on 200 answers.
fn serve(h: Handler, hits: Arc<AtomicUsize>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let (h, hits) = (h.clone(), hits.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                hits.fetch_add(1, Ordering::SeqCst);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let range = req.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                });
                let r = h(&path);
                if r.status == 200
                    && let Some(rg) = range
                {
                    let (a, b) = rg.split_once('-').unwrap();
                    let a: usize = a.trim().parse().unwrap();
                    let b: usize = b.trim().parse::<usize>().unwrap().min(r.body.len() - 1);
                    let part = &r.body[a..=b];
                    let _ = write!(
                        s,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        r.body.len(),
                        part.len()
                    );
                    let _ = s.write_all(part);
                } else {
                    let _ = write!(
                        s,
                        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        r.status,
                        r.body.len()
                    );
                    let _ = s.write_all(&r.body);
                }
            });
        }
    });
    port
}

fn hits() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn open(rt: &tokio::runtime::Runtime, url: String, q: Quality) -> Result<SegmentedReader, String> {
    rt.block_on(open_hls(
        rt.handle().clone(),
        reqwest::Client::new(),
        url,
        q,
        Vec::new(),
    ))
}

/// Pull until the end or a final error.
fn drain(r: &mut impl TimedSource) -> (Vec<Chunk>, Option<String>) {
    let mut out = Vec::new();
    let t0 = Instant::now();
    loop {
        match r.pull() {
            Ok(Some(c)) => out.push(c),
            Ok(None) => return (out, None),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return (out, Some(e.to_string())),
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "stalled");
    }
}

fn pull_one(r: &mut impl TimedSource) -> Chunk {
    let t0 = Instant::now();
    loop {
        match r.pull() {
            Ok(Some(c)) => return c,
            Ok(None) => panic!("ended early"),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "stalled");
    }
}

fn seg_bytes(tag: u8, i: usize) -> Vec<u8> {
    (0..1000 + i * 7)
        .map(|k| tag.wrapping_add((k % 200) as u8))
        .collect()
}

fn playlist(n: usize, dur: &str) -> String {
    let mut p = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n");
    for i in 0..n {
        p.push_str(&format!("#EXTINF:{dur},\ns{i}.ts\n"));
    }
    p.push_str("#EXT-X-ENDLIST\n");
    p
}

fn by_path(pl: String, n: usize, tag: u8) -> Handler {
    Arc::new(move |p: &str| {
        if p.ends_with(".m3u8") {
            return ok(pl.clone());
        }
        let i: usize = p
            .trim_start_matches("/s")
            .trim_end_matches(".ts")
            .parse()
            .unwrap_or(99);
        if i < n {
            ok(seg_bytes(tag, i))
        } else {
            status(404)
        }
    })
}

#[test]
fn concatenation_pts_and_duration() {
    let _g = serial();
    reset_policy();
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2.0,\ns0.ts\n#EXTINF:2.0,\ns1.ts\n#EXTINF:3.0,\ns2.ts\n#EXT-X-ENDLIST\n";
    let port = serve(by_path(pl.into(), 3, 10), hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    assert_eq!(r.duration(), Some(Duration::from_secs(7)));
    assert!(r.variants().is_empty() && r.current_variant().is_none());
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    let want: Vec<u8> = (0..3).flat_map(|i| seg_bytes(10, i)).collect();
    let got: Vec<u8> = chunks.iter().flat_map(|c| c.bytes.clone()).collect();
    assert_eq!(got, want);
    let pts: Vec<_> = chunks.iter().map(|c| c.pts).collect();
    assert_eq!(
        pts,
        [
            Some(Duration::ZERO),
            Some(Duration::from_secs(2)),
            Some(Duration::from_secs(4))
        ]
    );
    assert!(chunks.iter().all(|c| !c.discont));
    assert_eq!(r.buffered_until(), None);
}

#[test]
fn byterange_slices_one_file() {
    let _g = serial();
    reset_policy();
    let data: Vec<u8> = (0..3000u32).map(|i| (i % 253) as u8).collect();
    let pl = "#EXTM3U\n#EXTINF:2,\n#EXT-X-BYTERANGE:1000@0\nall.ts\n#EXTINF:2,\n#EXT-X-BYTERANGE:1000\nall.ts\n#EXTINF:2,\n#EXT-X-BYTERANGE:500@2000\nall.ts\n#EXT-X-ENDLIST\n";
    let d = data.clone();
    let h: Handler = Arc::new(move |p: &str| {
        if p.ends_with(".m3u8") {
            ok(pl)
        } else {
            ok(d.clone())
        }
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    let got: Vec<u8> = chunks.iter().flat_map(|c| c.bytes.clone()).collect();
    let mut want = data[..2000].to_vec();
    want.extend_from_slice(&data[2000..2500]);
    assert_eq!(got, want);
    assert_eq!(chunks.len(), 3);
}

#[test]
fn init_segment_is_emitted_again_after_seek_and_on_map_change() {
    let _g = serial();
    reset_policy();
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n\
#EXTINF:2,\ns0.ts\n#EXTINF:2,\ns1.ts\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"init2.mp4\"\n\
#EXTINF:2,\ns2.ts\n#EXT-X-ENDLIST\n";
    let h: Handler = Arc::new(move |p: &str| match p {
        "/m.m3u8" => ok(pl),
        "/init.mp4" => ok(vec![0, 0, 0, 8, 1, 1, 1, 1]),
        "/init2.mp4" => ok(vec![0, 0, 0, 8, 2, 2, 2, 2]),
        _ => ok(seg_bytes(50, p.len())),
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let c0 = pull_one(&mut r);
    assert_eq!((c0.pts, c0.bytes[4], c0.discont), (None, 1, false));
    let c1 = pull_one(&mut r);
    assert_eq!(c1.pts, Some(Duration::ZERO));
    let c2 = pull_one(&mut r);
    assert_eq!(c2.pts, Some(Duration::from_secs(2)));
    // The map changes at the discontinuity: new init first, flagged.
    let c3 = pull_one(&mut r);
    assert_eq!((c3.pts, c3.bytes[4], c3.discont), (None, 2, true));
    let c4 = pull_one(&mut r);
    assert_eq!((c4.pts, c4.discont), (Some(Duration::from_secs(4)), false));
    // Seek back: the first init again, flagged, then the segment containing 2.5 s.
    let at = r.seek(Duration::from_millis(2500));
    assert_eq!(at, Duration::from_secs(2));
    let s0 = pull_one(&mut r);
    assert_eq!((s0.pts, s0.bytes[4], s0.discont), (None, 1, true));
    let s1 = pull_one(&mut r);
    assert_eq!((s1.pts, s1.discont), (Some(Duration::from_secs(2)), false));
}

fn encrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    cbc::Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
}

#[test]
fn aes128_round_trip_with_explicit_and_sequence_iv() {
    let _g = serial();
    reset_policy();
    let key = [7u8; 16];
    let iv0: [u8; 16] = std::array::from_fn(|i| i as u8);
    let iv1 = 6u128.to_be_bytes(); // media sequence 5 + index 1
    let plain0 = seg_bytes(20, 0);
    let plain1 = seg_bytes(20, 1);
    let (e0, e1) = (encrypt(&key, &iv0, &plain0), encrypt(&key, &iv1, &plain1));
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:5\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:2,\ns0.ts\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:2,\ns1.ts\n#EXT-X-ENDLIST\n";
    let h: Handler = Arc::new(move |p: &str| match p {
        "/m.m3u8" => ok(pl),
        "/key.bin" => ok(key.to_vec()),
        "/s0.ts" => ok(e0.clone()),
        _ => ok(e1.clone()),
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    assert_eq!(chunks[0].bytes, plain0);
    assert_eq!(chunks[1].bytes, plain1);
}

#[test]
fn key_of_wrong_size_is_refused() {
    let _g = serial();
    reset_policy();
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n";
    let h: Handler = Arc::new(move |p: &str| match p {
        "/m.m3u8" => ok(pl),
        "/key.bin" => ok(vec![1u8; 15]),
        _ => ok(vec![9u8; 32]),
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(chunks.is_empty());
    assert_eq!(errors::classify(&err.unwrap()), errors::UNSUPPORTED_STREAM);
}

#[test]
fn local_key_from_a_public_origin_is_refused_without_a_request() {
    let _g = serial();
    reset_policy();
    let trap_hits = hits();
    let trap = serve(Arc::new(|_| ok(vec![1u8; 16])), trap_hits.clone());
    let pl = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"http://127.0.0.1:{trap}/key\"\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n"
    );
    let port = serve(by_path(pl, 1, 1), hits());
    // The name is public as far as the app can tell; the client maps it home.
    let client = reqwest::Client::builder()
        .resolve(
            "cdn.example.invalid",
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        )
        .build()
        .unwrap();
    let rt = rt();
    let res = rt.block_on(open_hls(
        rt.handle().clone(),
        client,
        format!("http://cdn.example.invalid:{port}/m.m3u8"),
        Quality::default(),
        Vec::new(),
    ));
    let err = res.err().expect("must be refused");
    assert_eq!(errors::classify(&err), errors::UNSUPPORTED_STREAM);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(trap_hits.load(Ordering::SeqCst), 0);
}

#[test]
fn a_404_on_a_vod_segment_ends_the_stream_with_http_404() {
    let _g = serial();
    reset_policy();
    let pl = playlist(3, "2");
    let port = serve(by_path(pl, 2, 3), hits()); // s2.ts is missing
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert_eq!(chunks.len(), 2);
    let err = err.unwrap();
    assert_eq!(err, "http 404");
    assert_eq!(errors::classify(&err), errors::NOT_FOUND);
}

#[test]
fn require_proxy_fails_before_any_socket_opens() {
    let _g = serial();
    let trap_hits = hits();
    let port = serve(by_path(playlist(1, "2"), 1, 1), trap_hits.clone());
    let d = tempfile::tempdir().unwrap();
    let _svc = FetchService::new(
        FetchConfig {
            dht: false,
            require_proxy: true,
            ..Default::default()
        },
        d.path().into(),
    )
    .unwrap();
    let rt = rt();
    let err = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .err()
    .expect("blocked");
    assert_eq!(errors::classify(&err), errors::PROXY_REQUIRED);
    let err = rt
        .block_on(open_dash(
            rt.handle().clone(),
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/m.mpd"),
            Quality::default(),
            Vec::new(),
        ))
        .err()
        .expect("blocked");
    assert_eq!(errors::classify(&err), errors::PROXY_REQUIRED);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(trap_hits.load(Ordering::SeqCst), 0);
    reset_policy();
}

#[test]
fn dropping_the_reader_stops_requests() {
    let _g = serial();
    reset_policy();
    let n = 12;
    let pl = playlist(n, "5");
    let inner = by_path(pl, n, 4);
    let h: Handler = Arc::new(move |p: &str| {
        if p.ends_with(".ts") {
            std::thread::sleep(Duration::from_millis(150));
        }
        inner(p)
    });
    let count = hits();
    let port = serve(h, count.clone());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    for _ in 0..3 {
        pull_one(&mut r);
    }
    drop(r);
    std::thread::sleep(Duration::from_millis(150));
    let snap = count.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(count.load(Ordering::SeqCst), snap);
}

#[test]
fn at_most_four_requests_in_flight() {
    let _g = serial();
    reset_policy();
    let n = 10;
    let pl = playlist(n, "2");
    let inner = by_path(pl, n, 5);
    let (cur, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c2, m2) = (cur.clone(), max.clone());
    let h: Handler = Arc::new(move |p: &str| {
        if !p.ends_with(".ts") {
            return inner(p);
        }
        let now = c2.fetch_add(1, Ordering::SeqCst) + 1;
        m2.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        c2.fetch_sub(1, Ordering::SeqCst);
        inner(p)
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    assert_eq!(chunks.len(), n);
    let m = max.load(Ordering::SeqCst);
    assert!((2..=4).contains(&m), "max in flight {m}");
}

#[test]
fn master_picks_by_quality_and_switches_on_request() {
    let _g = serial();
    reset_policy();
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720\nhigh.m3u8\n";
    let media = |tag: &str| {
        format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\n{tag}0.ts\n#EXTINF:2,\n{tag}1.ts\n#EXT-X-ENDLIST\n"
        )
    };
    let (low, high) = (media("low"), media("high"));
    let h: Handler = Arc::new(move |p: &str| match p {
        "/master.m3u8" => ok(master),
        "/low.m3u8" => ok(low.clone()),
        "/high.m3u8" => ok(high.clone()),
        p if p.starts_with("/low") => ok(vec![1u8; 300]),
        _ => ok(vec![2u8; 300]),
    });
    let port = serve(h, hits());
    let rt = rt();
    let url = format!("http://127.0.0.1:{port}/master.m3u8");
    let r = open(&rt, url.clone(), Quality::default()).unwrap();
    assert_eq!(r.current_variant(), Some(1));
    assert_eq!(r.variants().len(), 2);
    assert_eq!(r.variants()[0].height, Some(360));
    drop(r);
    let r = open(
        &rt,
        url.clone(),
        Quality {
            max_height: Some(480),
            prefer: Prefer::Best,
        },
    )
    .unwrap();
    assert_eq!(r.current_variant(), Some(0));
    drop(r);
    let mut r = open(&rt, url, Quality::default()).unwrap();
    r.select_variant(Some(0));
    std::thread::sleep(Duration::from_millis(400));
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    assert_eq!(r.current_variant(), Some(0));
    assert!(
        chunks.iter().all(|c| c.bytes[0] == 1),
        "all from the low variant"
    );
    assert_eq!(chunks.len(), 2);
}

#[test]
fn a_playlist_where_media_should_be_is_refused() {
    let _g = serial();
    reset_policy();
    let pl = playlist(1, "2");
    let h: Handler = Arc::new(move |p: &str| {
        if p.ends_with(".m3u8") {
            ok(pl.clone())
        } else {
            ok("#EXTM3U\n#EXTINF:1,\nhttp://elsewhere.invalid/x.ts\n")
        }
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(chunks.is_empty());
    assert_eq!(errors::classify(&err.unwrap()), errors::NO_VIDEO_HERE);
}

#[test]
fn dash_list_and_sidx_play_back() {
    let _g = serial();
    reset_policy();
    // Template/list manifest.
    let mpd = r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period>
<AdaptationSet mimeType="video/mp4"><Representation id="r1" bandwidth="500000" height="360">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="init.mp4" media="c$Number$.m4s"/>
</Representation></AdaptationSet></Period></MPD>"#;
    // Single file with a hand-built sidx for the Base case.
    let file: Vec<u8> = (0..400u32).map(|i| (i % 249) as u8 + 1).collect();
    let mut sidx = Vec::new();
    sidx.extend_from_slice(&[0, 0, 0, 0]);
    sidx.extend_from_slice(b"sidx");
    sidx.extend_from_slice(&[0, 0, 0, 0]);
    sidx.extend_from_slice(&1u32.to_be_bytes());
    sidx.extend_from_slice(&1u32.to_be_bytes());
    sidx.extend_from_slice(&0u32.to_be_bytes());
    sidx.extend_from_slice(&0u32.to_be_bytes());
    sidx.extend_from_slice(&[0, 0]);
    sidx.extend_from_slice(&2u16.to_be_bytes());
    for size in [100u32, 100] {
        sidx.extend_from_slice(&size.to_be_bytes());
        sidx.extend_from_slice(&2u32.to_be_bytes());
        sidx.extend_from_slice(&0x9000_0000u32.to_be_bytes());
    }
    let n = sidx.len() as u32;
    sidx[..4].copy_from_slice(&n.to_be_bytes());
    // Layout: [init 0..100) [sidx 100..100+n) [seg 100+n.. +100) [seg ..+100)
    let mut full = file[..100].to_vec();
    full.extend_from_slice(&sidx);
    full.extend_from_slice(&file[100..300]);
    let idx_end = 100 + sidx.len() as u64 - 1;
    let mpd_base = format!(
        r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period>
<AdaptationSet mimeType="video/mp4"><Representation id="b" bandwidth="1" height="240">
<BaseURL>one.mp4</BaseURL><SegmentBase indexRange="100-{idx_end}"><Initialization range="0-99"/></SegmentBase>
</Representation></AdaptationSet></Period></MPD>"#
    );
    let h: Handler = Arc::new(move |p: &str| match p {
        "/t.mpd" => ok(mpd),
        "/b.mpd" => ok(mpd_base.clone()),
        "/init.mp4" => ok(vec![0, 0, 0, 8, 9, 9, 9, 9]),
        "/one.mp4" => ok(full.clone()),
        p => ok(vec![p.len() as u8; 64]),
    });
    let port = serve(h, hits());
    let rt = rt();
    let od = |name: &str| {
        rt.block_on(open_dash(
            rt.handle().clone(),
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/{name}"),
            Quality::default(),
            Vec::new(),
        ))
        .unwrap()
    };
    let mut r = od("t.mpd");
    assert_eq!(r.duration(), Some(Duration::from_secs(4)));
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    // init once, then two segments.
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].pts, None);
    assert_eq!(chunks[2].pts, Some(Duration::from_secs(2)));
    let mut r = od("b.mpd");
    assert_eq!(r.duration(), Some(Duration::from_secs(4)));
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].bytes, file[..100]);
    assert_eq!(chunks[1].bytes, file[100..200]);
    assert_eq!(chunks[2].bytes, file[200..300]);
}
