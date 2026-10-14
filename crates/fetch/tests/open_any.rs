//! `open_any`: one ranged GET decides between manifest, file and page.
#![cfg(feature = "network")]

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jizzcraft_core::timed::TimedSource;
use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::errors;
use jizzcraft_fetch::resolver::ResolverDef;
use jizzcraft_fetch::service::{FetchService, StreamOpts, StreamTicket};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

type Handler = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

/// Tiny HTTP/1.1 server; 404 for an empty body; honours `Range`.
fn serve(h: Handler, hits: Arc<AtomicUsize>, delay: Duration) -> u16 {
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
                std::thread::sleep(delay);
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
                let body = h(&path);
                if body.is_empty() {
                    let _ = write!(
                        s,
                        "HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                } else if let Some(rg) = range {
                    let (a, b) = rg.split_once('-').unwrap();
                    let a: usize = a.trim().parse().unwrap();
                    let b: usize = b.trim().parse::<usize>().unwrap().min(body.len() - 1);
                    let part = &body[a..=b];
                    let _ = write!(
                        s,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        part.len()
                    );
                    let _ = s.write_all(part);
                } else {
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(&body);
                }
            });
        }
    });
    port
}

fn hits() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

fn svc(cfg: FetchConfig) -> (FetchService, tempfile::TempDir, tempfile::TempDir) {
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let s = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            ..cfg
        },
        st.path().into(),
    )
    .unwrap();
    (s, dl, st)
}

fn plain() -> (FetchService, tempfile::TempDir, tempfile::TempDir) {
    svc(FetchConfig::default())
}

fn drain(src: &mut dyn TimedSource) -> Vec<u8> {
    let mut out = Vec::new();
    let t0 = Instant::now();
    loop {
        match src.pull() {
            Ok(Some(c)) => out.extend_from_slice(&c.bytes),
            Ok(None) => return out,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "stalled");
    }
}

fn seg(i: usize) -> Vec<u8> {
    (0..500 + i * 3)
        .map(|k| (k as u8).wrapping_add(i as u8 + 1))
        .collect()
}

fn open(s: &FetchService, url: String) -> Result<StreamTicket, String> {
    s.open_any(url, StreamOpts::default())
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
}

#[test]
fn hls_vod_is_sniffed_and_concatenates() {
    let _g = serial();
    let h: Handler = Arc::new(|p| match p {
        "/watch" => {
            let mut s = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
            for i in 0..3 {
                s.push_str(&format!("#EXTINF:2,\nseg{i}.ts\n"));
            }
            s.push_str("#EXT-X-ENDLIST\n");
            s.into_bytes()
        }
        p => p
            .trim_start_matches("/seg")
            .trim_end_matches(".ts")
            .parse::<usize>()
            .map(seg)
            .unwrap_or_default(),
    });
    let port = serve(h, hits(), Duration::ZERO);
    let (s, _a, _b) = plain();
    let StreamTicket::Timed { mut source, .. } =
        open(&s, format!("http://127.0.0.1:{port}/watch")).expect("ticket")
    else {
        panic!("expected a timed stream");
    };
    assert_eq!(source.duration(), Some(Duration::from_secs(6)));
    let want: Vec<u8> = (0..3).flat_map(seg).collect();
    assert_eq!(drain(&mut *source), want);
}

#[test]
fn dash_segment_template_is_sniffed() {
    let _g = serial();
    let mpd = r#"<?xml version="1.0"?>
<MPD type="static" mediaPresentationDuration="PT4S"><Period>
<AdaptationSet mimeType="video/mp4"><Representation id="r1" bandwidth="500000" height="360">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="init.mp4" media="c$Number$.m4s"/>
</Representation></AdaptationSet></Period></MPD>"#;
    let h: Handler = Arc::new(move |p| match p {
        "/v/manifest" => mpd.as_bytes().to_vec(),
        "/v/init.mp4" => vec![9; 20],
        "/v/c0.m4s" => vec![1; 30],
        "/v/c1.m4s" => vec![2; 40],
        _ => Vec::new(),
    });
    let port = serve(h, hits(), Duration::ZERO);
    let (s, _a, _b) = plain();
    let StreamTicket::Timed { mut source, .. } =
        open(&s, format!("http://127.0.0.1:{port}/v/manifest")).expect("ticket")
    else {
        panic!("expected a timed stream");
    };
    let mut want = vec![9u8; 20];
    want.extend(vec![1u8; 30]);
    want.extend(vec![2u8; 40]);
    assert_eq!(drain(&mut *source), want);
}

#[test]
fn mp4_magic_goes_through_the_range_reader() {
    let _g = serial();
    let mut body = vec![0, 0, 0, 0x18];
    body.extend_from_slice(b"ftypisom");
    body.extend((0..1_500_000u32).map(|i| (i % 251) as u8));
    let want = body.clone();
    let count = hits();
    let port = serve(Arc::new(move |_| body.clone()), count, Duration::ZERO);
    let (s, dl, _st) = plain();
    // No extension in the address: the magic decides, not the name.
    let StreamTicket::Live {
        mut reader, len, ..
    } = open(&s, format!("http://127.0.0.1:{port}/get?id=7")).expect("ticket")
    else {
        panic!("expected a live stream");
    };
    assert_eq!(len, want.len() as u64);
    reader.seek(SeekFrom::Start(1_200_000)).unwrap();
    let mut got = vec![0u8; 1000];
    let t0 = Instant::now();
    let mut n = 0;
    while n < got.len() {
        match reader.read(&mut got[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(20));
    }
    assert_eq!(got, want[1_200_000..1_201_000]);
    assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn page_goes_to_resolvers_in_order_first_usable_wins() {
    let _g = serial();
    let h: Handler = Arc::new(|p| match p {
        "/page" => b"<html><body>hello</body></html>".to_vec(),
        "/media.m3u8" => {
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n".to_vec()
        }
        "/s0.ts" => seg(0),
        _ => Vec::new(),
    });
    let port = serve(h, hits(), Duration::ZERO);
    let def = |id: u32, script: String| ResolverDef {
        id,
        name: format!("r{id}"),
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
    let dead = def(
        1,
        format!(r#"printf '{{"formats":[{{"url":"http://127.0.0.1:{port}/gone.mp4"}}]}}'"#),
    );
    let good = def(
        2,
        format!(
            r#"printf '{{"formats":[{{"url":"http://127.0.0.1:{port}/video.mp4","vcodec":"avc1","acodec":"none","height":1080}},{{"url":"http://127.0.0.1:{port}/media.m3u8","protocol":"m3u8_native","height":480}}]}}'"#
        ),
    );
    let (s, _a, _b) = svc(FetchConfig {
        resolvers: vec![dead, good],
        ..Default::default()
    });
    let StreamTicket::Timed { mut source, .. } =
        open(&s, format!("http://127.0.0.1:{port}/page")).expect("ticket")
    else {
        panic!("expected a timed stream");
    };
    assert_eq!(drain(&mut *source), seg(0));

    // No resolver: a page is just a page.
    let (s2, _c, _d) = plain();
    let e = open(&s2, format!("http://127.0.0.1:{port}/page"))
        .err()
        .unwrap();
    assert_eq!(e, "no media");
    assert_eq!(errors::classify(&e), errors::NO_VIDEO_HERE);
}

#[test]
fn cancel_mid_open_returns_promptly() {
    let _g = serial();
    let port = serve(
        Arc::new(|_| b"#EXTM3U\n".to_vec()),
        hits(),
        Duration::from_secs(8),
    );
    let (s, _a, _b) = plain();
    let rx = s.open_any(
        format!("http://127.0.0.1:{port}/slow"),
        StreamOpts::default(),
    );
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    s.cancel_streams();
    let r = rx.recv_timeout(Duration::from_secs(3)).expect("answer");
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(errors::classify(&r.err().unwrap()), errors::CANCELLED);
    // The next open is not poisoned by the old flag.
    let port2 = serve(
        Arc::new(|_| {
            let mut b = vec![0, 0, 0, 8];
            b.extend_from_slice(b"ftypmp42");
            b.extend(vec![1u8; 100]);
            b
        }),
        hits(),
        Duration::ZERO,
    );
    assert!(open(&s, format!("http://127.0.0.1:{port2}/a")).is_ok());
}

#[test]
fn require_proxy_opens_no_socket_for_media_or_resolvers() {
    let _g = serial();
    let trap = hits();
    let port = serve(
        Arc::new(|_| b"#EXTM3U\n".to_vec()),
        trap.clone(),
        Duration::ZERO,
    );
    let resolver = ResolverDef {
        id: 1,
        name: "r".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "exit 1".into(),
            "sh".into(),
            "{url}".into(),
        ],
        timeout_secs: 5,
        enabled: true,
        honours_proxy: false,
    };
    let (s, _a, _b) = svc(FetchConfig {
        require_proxy: true,
        resolvers: vec![resolver],
        ..Default::default()
    });
    let e = open(&s, format!("http://127.0.0.1:{port}/m.m3u8"))
        .err()
        .unwrap();
    assert_eq!(errors::classify(&e), errors::PROXY_REQUIRED);
    let e = s
        .stream_via_resolver(1, format!("http://127.0.0.1:{port}/p"))
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(errors::classify(&e), errors::PROXY_REQUIRED);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(trap.load(Ordering::SeqCst), 0, "a socket was opened");
}

#[test]
fn source_headers_reach_the_media_fetch_and_the_secret_does_not() {
    let _g = serial();
    let seen: Arc<Mutex<String>> = Arc::default();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body: Vec<u8> = if req.starts_with("GET /s?") {
                format!(r#"{{"data":[{{"name":"Clip","play":"http://127.0.0.1:{port}/m"}}]}}"#)
                    .into_bytes()
            } else {
                *seen2.lock().unwrap() = req;
                let mut b = vec![0, 0, 0, 8];
                b.extend_from_slice(b"ftypmp42");
                b.extend(vec![1u8; 64]);
                b
            };
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    let mut def = jizzcraft_fetch::sources::parse_import(
        format!(
            r#"{{"name":"mine","search_url":"http://127.0.0.1:{port}/s?q={{query}}",
                "items":"/data","fields":{{"title":"/name","media":"/play"}},
                "auth_header":"Authorization","auth_prefix":"Bearer ",
                "referer":"https://ref.invalid/","user_agent":"UA-test"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    def.id = 1;
    def.secret = jizzcraft_fetch::Secret::new("TOKEN123");
    let (s, _a, _b) = svc(FetchConfig {
        sources: vec![def],
        ..Default::default()
    });
    let out = s
        .search(None, "x", &[], 10, Arc::default())
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    let r = &out.results[0];
    let mut opts = StreamOpts::for_result(r, Default::default());
    // Whatever else is smuggled in is dropped.
    opts.headers
        .push(("Authorization".into(), "Bearer TOKEN123".into()));
    let t = s
        .open_any(r.link.clone().unwrap(), opts)
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert!(t.is_ok());
    let req = seen.lock().unwrap().to_ascii_lowercase();
    assert!(req.contains("referer: https://ref.invalid/"), "{req}");
    assert!(req.contains("user-agent: ua-test"), "{req}");
    assert!(
        !req.contains("token123") && !req.contains("authorization"),
        "{req}"
    );
}
