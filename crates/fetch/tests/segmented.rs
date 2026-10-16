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
use jizzcraft_fetch::segmented::{
    AbrConfig, AudioPref, LiveTuning, SegmentedReader, open_dash, open_dash_audio, open_hls,
    open_hls_audio, open_hls_with,
};
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

#[test]
fn a_dynamic_dash_manifest_still_says_live() {
    let _g = serial();
    reset_policy();
    let rt = rt();
    let h: Handler = Arc::new(|_| ok("<MPD type=\"dynamic\"><Period/></MPD>"));
    let port = serve(h, hits());
    let err = rt
        .block_on(open_dash(
            rt.handle().clone(),
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/l.mpd"),
            Quality::default(),
            Vec::new(),
        ))
        .err()
        .expect("live DASH is refused");
    assert!(err.contains("live"), "{err}");
}

// ---- live HLS ---------------------------------------------------------------

fn fast_live() -> LiveTuning {
    LiveTuning {
        floor: Duration::from_millis(20),
        stale: Duration::from_secs(60),
        max_listen: Duration::from_secs(60),
    }
}

/// A sliding window over a synthetic live origin. `head` is the number of
/// segments published so far; `window` how many are listed.
struct Origin {
    head: AtomicUsize,
    window: usize,
    ended: std::sync::atomic::AtomicBool,
    /// Extra tag lines inserted before the segment with this sequence.
    disc_at: Option<usize>,
    /// Segment indexes answered with 404.
    missing: Vec<usize>,
    log: std::sync::Mutex<Vec<String>>,
}

impl Origin {
    fn new(head: usize, window: usize) -> Arc<Origin> {
        Arc::new(Origin {
            head: AtomicUsize::new(head),
            window,
            ended: std::sync::atomic::AtomicBool::new(false),
            disc_at: None,
            missing: Vec::new(),
            log: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn publish(&self, head: usize) {
        self.head.store(head, Ordering::SeqCst);
    }

    fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
    }

    fn playlist(&self) -> String {
        let head = self.head.load(Ordering::SeqCst);
        let first = head.saturating_sub(self.window);
        let mut p = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:0.1\n#EXT-X-MEDIA-SEQUENCE:{first}\n"
        );
        for i in first..head {
            if self.disc_at == Some(i) {
                p.push_str("#EXT-X-DISCONTINUITY\n");
            }
            p.push_str(&format!("#EXTINF:0.1,\ns{i}.ts\n"));
        }
        if self.ended.load(Ordering::SeqCst) {
            p.push_str("#EXT-X-ENDLIST\n");
        }
        p
    }

    fn handler(self: &Arc<Self>) -> Handler {
        let o = Arc::clone(self);
        Arc::new(move |p: &str| {
            o.log.lock().unwrap().push(p.to_owned());
            if p.ends_with(".m3u8") {
                return ok(o.playlist());
            }
            let i: usize = p
                .trim_start_matches("/s")
                .trim_end_matches(".ts")
                .parse()
                .unwrap_or(usize::MAX);
            let head = o.head.load(Ordering::SeqCst);
            let first = head.saturating_sub(o.window);
            if i < head && i >= first && !o.missing.contains(&i) {
                ok(seg_bytes(3, i))
            } else {
                status(404)
            }
        })
    }

    fn requested_segments(&self) -> Vec<usize> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|p| p.strip_prefix("/s")?.strip_suffix(".ts")?.parse().ok())
            .collect()
    }

    fn playlist_hits(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.ends_with(".m3u8"))
            .count()
    }
}

fn open_live(
    rt: &tokio::runtime::Runtime,
    port: u16,
    path: &str,
    t: LiveTuning,
) -> Result<SegmentedReader, String> {
    rt.block_on(open_hls_with(
        rt.handle().clone(),
        reqwest::Client::new(),
        format!("http://127.0.0.1:{port}{path}"),
        Quality::default(),
        Vec::new(),
        t,
    ))
}

/// Which segment a chunk holds (its length encodes the index).
fn seq_of(c: &Chunk) -> usize {
    (c.bytes.len() - 1000) / 7
}

#[test]
fn live_starts_three_from_the_edge_and_plays_each_segment_once() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(6, 6);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    // No total length, no seeking, no quality chip.
    assert_eq!(r.duration(), None);
    assert!(r.variants().is_empty());
    let mut seen = Vec::new();
    for i in 0..3 {
        let c = pull_one(&mut r);
        if i == 0 {
            assert_eq!(c.pts, Some(Duration::ZERO));
            assert!(!c.discont);
        }
        seen.push(seq_of(&c));
    }
    assert_eq!(seen, [3, 4, 5]);
    // The origin keeps publishing; the window slides (6 listed of 14).
    for head in 7..=14 {
        o.publish(head);
        std::thread::sleep(Duration::from_millis(130));
    }
    o.end();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    seen.extend(chunks.iter().map(seq_of));
    // Every segment exactly once, in order, nothing re-fetched or replayed.
    assert_eq!(seen, (3..14).collect::<Vec<_>>());
    assert!(chunks.iter().all(|c| !c.discont));
    let mut asked = o.requested_segments();
    asked.sort_unstable();
    asked.dedup();
    assert_eq!(asked, (3..14).collect::<Vec<_>>());
    // The reader went on asking for the playlist while it played.
    assert!(o.playlist_hits() >= 4);
    reset_policy();
}

#[test]
fn a_hole_in_the_sequence_is_a_discontinuity_and_playback_resumes() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(5, 5);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    let first: Vec<usize> = (0..3).map(|_| seq_of(&pull_one(&mut r))).collect();
    assert_eq!(first, [2, 3, 4]);
    // The consumer was away: the window jumps far ahead of what was seen.
    o.publish(20);
    let c = pull_one(&mut r);
    assert_eq!(seq_of(&c), 15);
    assert!(c.discont, "the hole must break the timeline");
    o.end();
    let (rest, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        rest.iter().map(seq_of).collect::<Vec<_>>(),
        [16, 17, 18, 19]
    );
    // Segments 5..15 are gone: never asked for.
    assert!(
        o.requested_segments()
            .iter()
            .all(|&i| !(5..15).contains(&i))
    );
    reset_policy();
}

#[test]
fn ext_x_discontinuity_marks_the_chunk() {
    let _g = serial();
    reset_policy();
    let mut o = Origin::new(8, 8);
    Arc::get_mut(&mut o).unwrap().disc_at = Some(6);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    o.end();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    // Starts at 5; the tag sits before 6.
    let flags: Vec<(usize, bool)> = chunks.iter().map(|c| (seq_of(c), c.discont)).collect();
    assert_eq!(flags, [(5, false), (6, true), (7, false)]);
    reset_policy();
}

#[test]
fn a_missing_live_segment_is_skipped_with_a_discontinuity() {
    let _g = serial();
    reset_policy();
    let mut o = Origin::new(9, 9);
    Arc::get_mut(&mut o).unwrap().missing = vec![7];
    let port = serve(o.handler(), hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    o.end();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    let flags: Vec<(usize, bool)> = chunks.iter().map(|c| (seq_of(c), c.discont)).collect();
    assert_eq!(flags, [(6, false), (8, true)]);
    reset_policy();
}

#[test]
fn a_numbering_restart_is_followed_and_flagged() {
    let _g = serial();
    reset_policy();
    // Two origins behind one port: the second starts again from 0.
    let first = Origin::new(6, 6);
    let second = Origin::new(4, 4);
    let flip = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (a, b, f) = (first.handler(), second.handler(), flip.clone());
    let h: Handler = Arc::new(
        move |p: &str| {
            if f.load(Ordering::SeqCst) { b(p) } else { a(p) }
        },
    );
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    let seen: Vec<usize> = (0..3).map(|_| seq_of(&pull_one(&mut r))).collect();
    assert_eq!(seen, [3, 4, 5]);
    flip.store(true, Ordering::SeqCst);
    let c = pull_one(&mut r);
    assert_eq!(seq_of(&c), 0);
    assert!(c.discont);
    second.end();
    let (rest, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(rest.iter().map(seq_of).collect::<Vec<_>>(), [1, 2, 3]);
    reset_policy();
}

#[test]
fn dropping_a_live_reader_stops_the_reloads() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(6, 6);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    let _ = pull_one(&mut r);
    std::thread::sleep(Duration::from_millis(250));
    assert!(o.playlist_hits() >= 3, "it was reloading");
    let t0 = Instant::now();
    drop(r);
    assert!(t0.elapsed() < Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(150));
    let after = (o.playlist_hits(), o.requested_segments().len());
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!((o.playlist_hits(), o.requested_segments().len()), after);
    reset_policy();
}

#[test]
fn a_playlist_that_stops_changing_ends_with_live_ended() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(4, 4);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let t = LiveTuning {
        stale: Duration::from_millis(500),
        ..fast_live()
    };
    let mut r = open_live(&rt, port, "/live.m3u8", t).unwrap();
    let (chunks, err) = drain(&mut r);
    // What was listed plays out first.
    assert_eq!(chunks.iter().map(seq_of).collect::<Vec<_>>(), [1, 2, 3]);
    let err = err.expect("ends with an error");
    assert_eq!(errors::classify(&err), errors::LIVE_ENDED);
    reset_policy();
}

#[test]
fn losing_the_playlist_ends_with_live_ended() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(4, 4);
    let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (inner, g) = (o.handler(), gone.clone());
    let h: Handler = Arc::new(move |p: &str| {
        if p.ends_with(".m3u8") && g.load(Ordering::SeqCst) {
            status(404)
        } else {
            inner(p)
        }
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    let _ = pull_one(&mut r);
    gone.store(true, Ordering::SeqCst);
    let (_, err) = drain(&mut r);
    let err = err.expect("ends with an error");
    assert_eq!(errors::classify(&err), errors::LIVE_ENDED);
    reset_policy();
}

#[test]
fn the_listening_session_is_bounded() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(4, 4);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let t = LiveTuning {
        max_listen: Duration::from_millis(400),
        ..fast_live()
    };
    let mut r = open_live(&rt, port, "/live.m3u8", t).unwrap();
    let (_, err) = drain(&mut r);
    assert!(err.is_none(), "a limit is a clean end: {err:?}");
    reset_policy();
}

#[test]
fn live_drm_is_refused_at_open_and_when_it_appears_later() {
    let _g = serial();
    reset_policy();
    let rt = rt();
    let sample = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:2,\ns0.ts\n";
    let h: Handler = Arc::new(move |_| ok(sample));
    let port = serve(h, hits());
    let err = open_live(&rt, port, "/l.m3u8", fast_live())
        .err()
        .expect("refused");
    assert_eq!(errors::classify(&err), errors::PROTECTED);
    // Widevine key format.
    let wv = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"\n#EXTINF:2,\ns0.ts\n";
    let h: Handler = Arc::new(move |_| ok(wv));
    let port = serve(h, hits());
    let err = open_live(&rt, port, "/l.m3u8", fast_live())
        .err()
        .expect("refused");
    assert_eq!(errors::classify(&err), errors::PROTECTED);

    // Clear at first, protected on a later reload: what is buffered plays,
    // then the stream ends as protected. No key is ever requested.
    let o = Origin::new(4, 4);
    let drm = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (inner, d) = (o.handler(), drm.clone());
    let h: Handler = Arc::new(move |p: &str| {
        if p.ends_with(".m3u8") && d.load(Ordering::SeqCst) {
            ok(
                "#EXTM3U\n#EXT-X-TARGETDURATION:0.1\n#EXT-X-MEDIA-SEQUENCE:5\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:0.1,\ns5.ts\n",
            )
        } else {
            inner(p)
        }
    });
    let port = serve(h, hits());
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    let _ = pull_one(&mut r);
    drm.store(true, Ordering::SeqCst);
    let (_, err) = drain(&mut r);
    assert_eq!(errors::classify(&err.expect("refused")), errors::PROTECTED);
    reset_policy();
}

#[test]
fn live_from_a_master_plays_the_chosen_variant_without_a_quality_chip() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(5, 5);
    let inner = o.handler();
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=160x120\nlow/live.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=320x240\nhigh/live.m3u8\n";
    let h: Handler = Arc::new(move |p: &str| {
        if p == "/master.m3u8" {
            ok(master)
        } else if let Some(rest) = p.strip_prefix("/high/") {
            inner(&format!("/{rest}"))
        } else {
            inner(p)
        }
    });
    let port = serve(h, hits());
    let rt = rt();
    // Segments resolve against the variant playlist: /high/s4.ts.
    let mut r = open_live(&rt, port, "/master.m3u8", fast_live()).unwrap();
    assert_eq!(r.duration(), None);
    assert!(r.variants().is_empty());
    let c = pull_one(&mut r);
    assert_eq!(seq_of(&c), 2);
    o.end();
    let (rest, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(rest.len(), 2);
    reset_policy();
}

#[test]
fn live_seek_is_a_no_op_and_memory_stays_bounded_on_a_long_run() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(4, 4);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let mut r = open_live(&rt, port, "/live.m3u8", fast_live()).unwrap();
    let c = pull_one(&mut r);
    // Seeking does not move anything.
    let _ = r.seek(Duration::from_secs(1000));
    let mut last = seq_of(&c);
    for head in 5..=200 {
        o.publish(head);
        // Pull whatever is there; the origin only lists 4.
        loop {
            match r.pull() {
                Ok(Some(c)) => last = seq_of(&c),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                other => panic!("{other:?}"),
            }
        }
    }
    o.end();
    let (_, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert!(last >= 150, "kept up with the edge: {last}");
    reset_policy();
}

const AUDIO_MEDIA: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nv0.ts\n#EXT-X-ENDLIST\n";

#[test]
fn separate_audio_only_says_separate_tracks_and_muxed_wins_when_offered() {
    let _g = serial();
    reset_policy();
    let split = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",DEFAULT=YES,URI=\"a.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720,AUDIO=\"a\"\nv.m3u8\n";
    let both = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",DEFAULT=YES,URI=\"a.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,AUDIO=\"a\"\nv.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\nmuxed.m3u8\n";
    let h: Handler = Arc::new(move |p: &str| match p {
        "/split.m3u8" => ok(split),
        "/both.m3u8" => ok(both),
        p if p.ends_with(".m3u8") => ok(AUDIO_MEDIA),
        _ => ok(vec![7u8; 100]),
    });
    let port = serve(h, hits());
    let rt = rt();
    let err = open(
        &rt,
        format!("http://127.0.0.1:{port}/split.m3u8"),
        Quality::default(),
    )
    .err()
    .expect("refused");
    assert!(err.contains("separate tracks"), "{err}");
    let r = open(
        &rt,
        format!("http://127.0.0.1:{port}/both.m3u8"),
        Quality::default(),
    )
    .unwrap();
    // Only the muxed variant is offered, and it is the one playing.
    assert_eq!(r.variants().len(), 1);
    assert_eq!(r.variants()[0].height, Some(360));
    assert_eq!(r.current_variant(), Some(0));
}

#[test]
fn one_failed_short_segment_is_skipped_with_a_discontinuity() {
    let _g = serial();
    reset_policy();
    let n = 50;
    let inner = by_path(playlist(n, "2"), n, 6);
    let h: Handler = Arc::new(
        move |p: &str| {
            if p == "/s7.ts" { status(404) } else { inner(p) }
        },
    );
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(chunks.len(), n - 1);
    let flagged: Vec<usize> = chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.discont)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(flagged, [7]);
    assert_eq!(chunks[7].pts, Some(Duration::from_secs(16)));
}

#[test]
fn quality_goes_down_when_the_server_is_slow_and_back_up_when_it_recovers() {
    let _g = serial();
    reset_policy();
    let n = 60;
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=400000,RESOLUTION=640x360,CODECS=\"avc1.42e01e,mp4a.40.2\"\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\nhigh.m3u8\n";
    let media = |tag: &str| {
        let mut p = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
        for i in 0..n {
            p.push_str(&format!("#EXTINF:2,\n{tag}{i}.ts\n"));
        }
        p.push_str("#EXT-X-ENDLIST\n");
        p
    };
    let (low, high) = (media("low"), media("high"));
    let born = Instant::now();
    let h: Handler = Arc::new(move |p: &str| match p {
        "/master.m3u8" => ok(master),
        "/low.m3u8" => ok(low.clone()),
        "/high.m3u8" => ok(high.clone()),
        p if p.starts_with("/low") => ok(vec![1u8; 20_000]),
        _ => {
            // The first seconds the network is poor for the big files.
            if born.elapsed() < Duration::from_millis(2500) {
                std::thread::sleep(Duration::from_millis(1000));
            }
            ok(vec![2u8; 40_000])
        }
    });
    let port = serve(h, hits());
    let rt = rt();
    let cfg = AbrConfig {
        down_gap: Duration::from_secs(1),
        up_buffer: Duration::from_secs(6),
        up_hold: Duration::from_secs(1),
        up_gap: Duration::from_secs(2),
        ..AbrConfig::default()
    };
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/master.m3u8"),
        Quality::default(),
    )
    .unwrap()
    .with_abr(cfg);
    assert_eq!(r.current_variant(), Some(1));
    let mut seen = vec![1usize];
    let mut count = 0;
    let t0 = Instant::now();
    loop {
        match r.pull() {
            Ok(Some(_)) => {
                count += 1;
                std::thread::sleep(Duration::from_millis(60));
            }
            Ok(None) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        let v = r.current_variant().unwrap();
        if seen.last() != Some(&v) {
            seen.push(v);
        }
        assert!(t0.elapsed() < Duration::from_secs(40), "stalled {seen:?}");
    }
    assert_eq!(count, n);
    assert!(
        seen.starts_with(&[1, 0, 1]),
        "expected down then up, got {seen:?}"
    );
    // The pipeline never had to be rebuilt: codecs agree.
    assert!(!r.wants_downgrade());
}

// ---- separate audio and video (fragmented MP4) -------------------------------

fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let n: usize = parts.iter().map(|p| p.len()).sum::<usize>() + 8;
    let mut out = (n as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

fn fbx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    bx(kind, &[&[0, 0, 0, 0], body])
}

/// Synthetic init segment of one track (id 1), `format` is the sample entry.
fn init_mp4(handler: &[u8; 4], format: &[u8; 4]) -> Vec<u8> {
    let mut mvhd = vec![0u8; 96];
    mvhd.extend_from_slice(&2u32.to_be_bytes());
    let mut tk = vec![0u8; 8];
    tk.extend_from_slice(&1u32.to_be_bytes());
    tk.extend_from_slice(&[0u8; 68]);
    let entry = bx(format, &[&[0u8; 8]]);
    let mut stsd = 1u32.to_be_bytes().to_vec();
    stsd.extend_from_slice(&entry);
    let stbl = bx(b"stbl", &[&fbx(b"stsd", &stsd)]);
    let minf = bx(b"minf", &[&stbl]);
    let mut hd = vec![0u8; 4];
    hd.extend_from_slice(handler);
    hd.extend_from_slice(&[0u8; 12]);
    let mdia = bx(b"mdia", &[&fbx(b"hdlr", &hd), &minf]);
    let trak = bx(b"trak", &[&fbx(b"tkhd", &tk), &mdia]);
    let mut tx = 1u32.to_be_bytes().to_vec();
    tx.extend_from_slice(&[0u8; 16]);
    let mvex = bx(b"mvex", &[&fbx(b"trex", &tx)]);
    let moov = bx(b"moov", &[&fbx(b"mvhd", &mvhd), &trak, &mvex]);
    let mut out = bx(b"ftyp", &[b"isom", &[0, 0, 0, 0], b"isomiso6"]);
    out.extend_from_slice(&moov);
    out
}

fn frag_mp4(seq: u32, payload: &[u8]) -> Vec<u8> {
    let traf = bx(b"traf", &[&fbx(b"tfhd", &1u32.to_be_bytes())]);
    let moof = bx(b"moof", &[&fbx(b"mfhd", &seq.to_be_bytes()), &traf]);
    let mut out = moof;
    out.extend_from_slice(&bx(b"mdat", &[payload]));
    out
}

/// Track id written in the first `tfhd` of a combined fragment.
fn tfhd_id(frag: &[u8]) -> u32 {
    let at = frag.windows(4).position(|w| w == b"tfhd").unwrap() + 4 + 4;
    u32::from_be_bytes(frag[at..at + 4].try_into().unwrap())
}

/// Payload tag of a fragment built by `frag_mp4` (the `mdat` content).
fn tag_of(frag: &[u8]) -> Vec<u8> {
    let at = frag.windows(4).position(|w| w == b"mdat").unwrap() + 4;
    frag[at..].to_vec()
}

fn count_of(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

const SPLIT_MPD: &str = r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period>
<AdaptationSet mimeType="video/mp4"><Representation id="v1" bandwidth="500000" height="360">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="vinit.mp4" media="v$Number$.m4s"/>
</Representation><Representation id="v2" bandwidth="900000" height="720">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="vinit.mp4" media="w$Number$.m4s"/>
</Representation></AdaptationSet>
<AdaptationSet mimeType="audio/mp4" lang="en"><Representation id="a1" bandwidth="64000">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="ainit.mp4" media="aen$Number$.m4s"/>
</Representation></AdaptationSet>
<AdaptationSet mimeType="audio/mp4" lang="fr"><Representation id="a2" bandwidth="64000">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="ainit.mp4" media="afr$Number$.m4s"/>
</Representation></AdaptationSet>
</Period></MPD>"#;

fn split_handler(audio_init: Vec<u8>) -> Handler {
    let vinit = init_mp4(b"vide", b"avc1");
    Arc::new(move |p: &str| {
        let seq = |name: &str| -> u32 {
            name.trim_end_matches(".m4s")
                .trim_start_matches(|c: char| c.is_ascii_alphabetic())
                .parse()
                .unwrap_or(0)
        };
        let n = seq(p.trim_start_matches('/'));
        match p {
            "/s.mpd" => ok(SPLIT_MPD),
            "/vinit.mp4" => ok(vinit.clone()),
            "/ainit.mp4" => ok(audio_init.clone()),
            p if p.starts_with("/v") => ok(frag_mp4(n, format!("V{n}").as_bytes())),
            p if p.starts_with("/w") => ok(frag_mp4(n, format!("W{n}").as_bytes())),
            p if p.starts_with("/aen") => ok(frag_mp4(n, format!("E{n}").as_bytes())),
            p if p.starts_with("/afr") => ok(frag_mp4(n, format!("F{n}").as_bytes())),
            _ => status(404),
        }
    })
}

fn open_split(
    rt: &tokio::runtime::Runtime,
    port: u16,
    lang: Option<&str>,
) -> Result<SegmentedReader, String> {
    rt.block_on(open_dash_audio(
        rt.handle().clone(),
        reqwest::Client::new(),
        format!("http://127.0.0.1:{port}/s.mpd"),
        Quality {
            max_height: Some(360),
            prefer: Prefer::Best,
        },
        Vec::new(),
        lang.map(str::to_owned),
    ))
}

#[test]
fn dash_video_and_audio_tracks_are_combined_in_time_order() {
    let _g = serial();
    reset_policy();
    let port = serve(split_handler(init_mp4(b"soun", b"mp4a")), hits());
    let rt = rt();
    let mut r = open_split(&rt, port, None).unwrap();
    assert_eq!(r.duration(), Some(Duration::from_secs(4)));
    // Quality concerns the video only: two variants, audio is not one.
    assert_eq!(r.variants().len(), 2);
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    // One combined init, then v0 a0 v1 a1.
    assert_eq!(chunks.len(), 5);
    assert_eq!(chunks[0].pts, None);
    assert_eq!(count_of(&chunks[0].bytes, b"trak"), 2);
    assert_eq!(count_of(&chunks[0].bytes, b"moov"), 1);
    let media: Vec<(u32, Vec<u8>, Option<Duration>)> = chunks[1..]
        .iter()
        .map(|c| (tfhd_id(&c.bytes), tag_of(&c.bytes), c.pts))
        .collect();
    let s = Duration::from_secs;
    assert_eq!(
        media,
        vec![
            (1, b"V0".to_vec(), Some(s(0))),
            (2, b"E0".to_vec(), Some(s(0))),
            (1, b"V1".to_vec(), Some(s(2))),
            (2, b"E1".to_vec(), Some(s(2))),
        ]
    );
    // A language preference picks the French set; sample bytes are intact.
    let mut r = open_split(&rt, port, Some("fr-CA")).unwrap();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    assert_eq!(tag_of(&chunks[2].bytes), b"F0");
}

#[test]
fn separate_tracks_are_listed_and_can_be_picked_by_index() {
    let _g = serial();
    reset_policy();
    let port = serve(split_handler(init_mp4(b"soun", b"mp4a")), hits());
    let rt = rt();
    let r = open_split(&rt, port, None).unwrap();
    let tracks = r.audio_tracks();
    let langs: Vec<Option<&str>> = tracks.iter().map(|t| t.lang.as_deref()).collect();
    assert_eq!(langs, vec![Some("en"), Some("fr")]);
    assert_eq!(r.current_audio(), Some(0));
    drop(r);
    let mut r = rt
        .block_on(open_dash_audio(
            rt.handle().clone(),
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/s.mpd"),
            Quality::default(),
            Vec::new(),
            AudioPref::Index(1),
        ))
        .unwrap();
    assert_eq!(r.current_audio(), Some(1));
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none());
    assert_eq!(tag_of(&chunks[2].bytes), b"F0");
    // Out of range falls back to the default track.
    let r = rt
        .block_on(open_dash_audio(
            rt.handle().clone(),
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/s.mpd"),
            Quality::default(),
            Vec::new(),
            AudioPref::Index(9),
        ))
        .unwrap();
    assert_eq!(r.current_audio(), Some(0));
}

#[test]
fn separate_tracks_seek_restarts_both_with_a_new_init() {
    let _g = serial();
    reset_policy();
    let port = serve(split_handler(init_mp4(b"soun", b"mp4a")), hits());
    let rt = rt();
    let mut r = open_split(&rt, port, None).unwrap();
    for _ in 0..3 {
        pull_one(&mut r);
    }
    assert_eq!(r.seek(Duration::from_secs(2)), Duration::from_secs(2));
    let init = pull_one(&mut r);
    assert_eq!(init.pts, None);
    assert!(init.discont);
    assert_eq!(count_of(&init.bytes, b"trak"), 2);
    let (rest, err) = drain(&mut r);
    assert!(err.is_none());
    let tags: Vec<Vec<u8>> = rest.iter().map(|c| tag_of(&c.bytes)).collect();
    assert_eq!(tags, vec![b"V1".to_vec(), b"E1".to_vec()]);
    // Past the end: nothing more.
    r.seek(Duration::from_secs(99));
    assert!(drain(&mut r).0.is_empty());
}

#[test]
fn audio_that_cannot_be_combined_is_refused_and_protected_audio_is() {
    let _g = serial();
    reset_policy();
    let rt = rt();
    // Audio that is not fragmented MP4 (here: transport stream bytes).
    let port = serve(split_handler(vec![0x47; 188]), hits());
    let err = open_split(&rt, port, None).err().expect("refused");
    assert_eq!(errors::classify(&err), errors::SEPARATE_TRACKS);
    // Audio whose sample entry says encrypted.
    let port = serve(split_handler(init_mp4(b"soun", b"enca")), hits());
    let err = open_split(&rt, port, None).err().expect("refused");
    assert_eq!(errors::classify(&err), errors::PROTECTED);
}

#[test]
fn dropping_a_two_track_reader_stops_both_downloads() {
    let _g = serial();
    reset_policy();
    let inner = split_handler(init_mp4(b"soun", b"mp4a"));
    let h: Handler = Arc::new(move |p: &str| {
        if p.ends_with(".m4s") {
            std::thread::sleep(Duration::from_millis(100));
        }
        inner(p)
    });
    let count = hits();
    let port = serve(h, count.clone());
    let rt = rt();
    let mut r = open_split(&rt, port, None).unwrap();
    pull_one(&mut r);
    drop(r);
    std::thread::sleep(Duration::from_millis(300));
    let snap = count.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(count.load(Ordering::SeqCst), snap);
}

#[test]
fn hls_audio_group_is_combined_and_language_is_chosen() {
    let _g = serial();
    reset_policy();
    let master = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"aen.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Portugues\",LANGUAGE=\"pt-BR\",URI=\"apt.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720,AUDIO=\"a\"\nv.m3u8\n";
    let media = |prefix: &str| {
        format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"{prefix}init.mp4\"\n\
#EXTINF:2,\n{prefix}0.m4s\n#EXTINF:2,\n{prefix}1.m4s\n#EXT-X-ENDLIST\n"
        )
    };
    let (v, en, pt) = (media("v"), media("en"), media("pt"));
    let h: Handler = Arc::new(move |p: &str| match p {
        "/m.m3u8" => ok(master),
        "/v.m3u8" => ok(v.clone()),
        "/aen.m3u8" => ok(en.clone()),
        "/apt.m3u8" => ok(pt.clone()),
        "/vinit.mp4" => ok(init_mp4(b"vide", b"avc1")),
        "/eninit.mp4" | "/ptinit.mp4" => ok(init_mp4(b"soun", b"mp4a")),
        "/v0.m4s" => ok(frag_mp4(0, b"V0")),
        "/v1.m4s" => ok(frag_mp4(1, b"V1")),
        "/en0.m4s" => ok(frag_mp4(0, b"E0")),
        "/en1.m4s" => ok(frag_mp4(1, b"E1")),
        "/pt0.m4s" => ok(frag_mp4(0, b"P0")),
        "/pt1.m4s" => ok(frag_mp4(1, b"P1")),
        _ => status(404),
    });
    let port = serve(h, hits());
    let rt = rt();
    let open_lang = |lang: Option<&str>| {
        rt.block_on(open_hls_audio(
            rt.handle().clone(),
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/m.m3u8"),
            Quality::default(),
            Vec::new(),
            LiveTuning::default(),
            lang.map(str::to_owned),
        ))
        .unwrap()
    };
    let tags = |mut r: SegmentedReader| {
        let (c, e) = drain(&mut r);
        assert!(e.is_none(), "{e:?}");
        assert_eq!(count_of(&c[0].bytes, b"trak"), 2);
        c[1..].iter().map(|c| tag_of(&c.bytes)).collect::<Vec<_>>()
    };
    let want =
        |a: &[u8; 2], b: &[u8; 2]| vec![b"V0".to_vec(), a.to_vec(), b"V1".to_vec(), b.to_vec()];
    // Default rendition without a preference, Portuguese on request.
    assert_eq!(tags(open_lang(None)), want(b"E0", b"E1"));
    assert_eq!(tags(open_lang(Some("pt"))), want(b"P0", b"P1"));
    assert_eq!(tags(open_lang(Some("xx"))), want(b"E0", b"E1"));
}

// ---- strict mode for saving ---------------------------------------------------

#[test]
fn download_mode_fails_on_a_lost_segment_where_playback_skips() {
    let _g = serial();
    reset_policy();
    let n = 50;
    let inner = by_path(playlist(n, "2"), n, 6);
    let h: Handler = Arc::new(
        move |p: &str| {
            if p == "/s7.ts" { status(404) } else { inner(p) }
        },
    );
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap()
    .for_download();
    let (chunks, err) = drain(&mut r);
    assert_eq!(chunks.len(), 7);
    assert_eq!(err.as_deref(), Some("http 404"));
    assert!(chunks.iter().all(|c| !c.discont));
}

#[test]
fn download_mode_keeps_its_variant_under_throttle() {
    let _g = serial();
    reset_policy();
    let n = 12;
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=400000,RESOLUTION=640x360,CODECS=\"avc1.42e01e,mp4a.40.2\"\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\nhigh.m3u8\n";
    let media = |tag: &str| {
        let mut p = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
        for i in 0..n {
            p.push_str(&format!("#EXTINF:2,\n{tag}{i}.ts\n"));
        }
        p.push_str("#EXT-X-ENDLIST\n");
        p
    };
    let (low, high) = (media("low"), media("high"));
    let h: Handler = Arc::new(move |p: &str| match p {
        "/master.m3u8" => ok(master),
        "/low.m3u8" => ok(low.clone()),
        "/high.m3u8" => ok(high.clone()),
        p if p.starts_with("/low") => ok(vec![1u8; 20_000]),
        _ => {
            std::thread::sleep(Duration::from_millis(150));
            ok(vec![2u8; 40_000])
        }
    });
    let port = serve(h, hits());
    let rt = rt();
    let cfg = AbrConfig {
        down_gap: Duration::from_millis(200),
        ..AbrConfig::default()
    };
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/master.m3u8"),
        Quality::default(),
    )
    .unwrap()
    .with_abr(cfg)
    .for_download();
    assert_eq!(r.current_variant(), Some(1));
    r.select_variant(Some(0)); // ignored while saving
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(chunks.len(), n);
    assert!(chunks.iter().all(|c| c.bytes[0] == 2));
    assert_eq!(r.current_variant(), Some(1));
    assert!(!r.wants_downgrade());
}

#[test]
fn download_mode_aes128_yields_plaintext() {
    let _g = serial();
    reset_policy();
    let key = [9u8; 16];
    let iv: [u8; 16] = std::array::from_fn(|i| i as u8);
    let plain = seg_bytes(33, 0);
    let enc = encrypt(&key, &iv, &plain);
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n";
    let h: Handler = Arc::new(move |p: &str| match p {
        "/m.m3u8" => ok(pl),
        "/key.bin" => ok(key.to_vec()),
        _ => ok(enc.clone()),
    });
    let port = serve(h, hits());
    let rt = rt();
    let mut r = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .unwrap()
    .for_download();
    let (chunks, err) = drain(&mut r);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].bytes, plain);
}

#[test]
fn live_has_no_duration_or_estimate() {
    let _g = serial();
    reset_policy();
    let o = Origin::new(6, 6);
    let port = serve(o.handler(), hits());
    let rt = rt();
    let r = open_live(&rt, port, "/live.m3u8", fast_live())
        .unwrap()
        .for_download();
    assert_eq!(r.duration(), None);
    assert_eq!(r.estimated_bytes(), None);
}

#[test]
fn estimated_bytes_from_bandwidth() {
    let _g = serial();
    reset_policy();
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nlow.m3u8\n";
    let media =
        "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\na.ts\n#EXTINF:10,\nb.ts\n#EXT-X-ENDLIST\n";
    let h: Handler = Arc::new(move |p: &str| match p {
        "/master.m3u8" => ok(master),
        "/plain.m3u8" => ok(media),
        "/low.m3u8" => ok(media),
        _ => ok(vec![0x47u8; 100]),
    });
    let port = serve(h, hits());
    let rt = rt();
    let r = open(
        &rt,
        format!("http://127.0.0.1:{port}/master.m3u8"),
        Quality::default(),
    )
    .unwrap();
    // 800 kbit/s for 20 s = 2 MB.
    assert_eq!(r.estimated_bytes(), Some(2_000_000));
    // A bare media playlist declares no bandwidth.
    let r = open(
        &rt,
        format!("http://127.0.0.1:{port}/plain.m3u8"),
        Quality::default(),
    )
    .unwrap();
    assert_eq!(r.estimated_bytes(), None);
}

#[test]
fn sample_aes_is_refused_in_download_mode() {
    let _g = serial();
    reset_policy();
    let sample = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n";
    let h: Handler = Arc::new(move |_| ok(sample));
    let port = serve(h, hits());
    let rt = rt();
    let err = open(
        &rt,
        format!("http://127.0.0.1:{port}/m.m3u8"),
        Quality::default(),
    )
    .err()
    .expect("refused");
    assert_eq!(errors::classify(&err), errors::PROTECTED);
}
