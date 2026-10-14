//! A tiny SOCKS5 server (no-auth and user/pass) that records what it is asked
//! to CONNECT to. Proves that with a proxy set every request path goes through
//! it with names unresolved (socks5h), that a dead or refusing proxy fails
//! closed (a trap listener sees nothing), and that `require_proxy` without a
//! proxy connects to nothing. Loopback only, synthetic data only.
#![cfg(feature = "network")]

use jizzcraft_fetch::config::{FetchConfig, Indexer};
use jizzcraft_fetch::resolver::ResolverDef;
use jizzcraft_fetch::secret::Secret;
use jizzcraft_fetch::service::{FetchService, Source, StreamOpts, StreamTicket};
use jizzcraft_fetch::sources;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Accept, then answer HTTP as the destination.
    Serve,
    /// Reply "connection refused" to CONNECT.
    Refuse,
    /// Close the socket as soon as it is accepted.
    Drop,
}

#[derive(Clone, Debug)]
struct Seen {
    host: String,
    domain: bool,
    port: u16,
    path: String,
}

type Handler = Arc<dyn Fn(&str, &str) -> Vec<u8> + Send + Sync>;

struct Mock {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    creds: Arc<Mutex<Vec<(String, String)>>>,
}

impl Mock {
    fn hosts(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.host.clone())
            .collect()
    }
    fn url(&self) -> String {
        format!("socks5://127.0.0.1:{}", self.port)
    }
}

fn read_n(s: &mut TcpStream, n: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).ok()?;
    Some(b)
}

type Handshake = (String, bool, u16, Vec<(String, String)>);

fn handshake(s: &mut TcpStream, auth: Option<(&str, &str)>) -> Option<Handshake> {
    let g = read_n(s, 2)?;
    if g[0] != 5 {
        return None;
    }
    let methods = read_n(s, g[1] as usize)?;
    let mut creds = Vec::new();
    match auth {
        Some((u, p)) => {
            if !methods.contains(&2) {
                let _ = s.write_all(&[5, 0xff]);
                return None;
            }
            s.write_all(&[5, 2]).ok()?;
            let v = read_n(s, 2)?;
            let user = read_n(s, v[1] as usize)?;
            let pl = read_n(s, 1)?;
            let pass = read_n(s, pl[0] as usize)?;
            let (user, pass) = (
                String::from_utf8_lossy(&user).into_owned(),
                String::from_utf8_lossy(&pass).into_owned(),
            );
            let ok = user == u && pass == p;
            creds.push((user, pass));
            s.write_all(&[1, if ok { 0 } else { 1 }]).ok()?;
            if !ok {
                return None;
            }
        }
        None => {
            s.write_all(&[5, 0]).ok()?;
        }
    }
    let r = read_n(s, 4)?;
    let (host, domain) = match r[3] {
        1 => {
            let a = read_n(s, 4)?;
            (format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]), false)
        }
        3 => {
            let l = read_n(s, 1)?;
            let d = read_n(s, l[0] as usize)?;
            (String::from_utf8_lossy(&d).into_owned(), true)
        }
        4 => {
            let a = read_n(s, 16)?;
            (format!("{a:?}"), false)
        }
        _ => return None,
    };
    let p = read_n(s, 2)?;
    Some((host, domain, u16::from_be_bytes([p[0], p[1]]), creds))
}

fn mock(mode: Mode, auth: Option<(&'static str, &'static str)>, h: Handler) -> Mock {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let creds = Arc::new(Mutex::new(Vec::new()));
    let (seen2, creds2) = (seen.clone(), creds.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            if mode == Mode::Drop {
                continue; // dropped at once
            }
            let (h, seen, creds) = (h.clone(), seen2.clone(), creds2.clone());
            std::thread::spawn(move || {
                let Some((host, domain, port, c)) = handshake(&mut s, auth) else {
                    return;
                };
                creds.lock().unwrap().extend(c);
                if mode == Mode::Refuse {
                    let _ = s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
                    seen.lock().unwrap().push(Seen {
                        host,
                        domain,
                        port,
                        path: String::new(),
                    });
                    return;
                }
                if s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).is_err() {
                    return;
                }
                // Act as the destination.
                let mut head = Vec::new();
                let mut b = [0u8; 2048];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&b[..n]),
                    }
                    if head.len() > 16384 {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&head).into_owned();
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                seen.lock().unwrap().push(Seen {
                    host: host.clone(),
                    domain,
                    port,
                    path: path.clone(),
                });
                let range = req.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                });
                let body = h(&host, &path);
                if body.is_empty() {
                    let _ = write!(
                        s,
                        "HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                } else if let Some(rg) = range {
                    let (a, e) = rg.split_once('-').unwrap();
                    let a: usize = a.trim().parse().unwrap();
                    let e: usize = e.trim().parse::<usize>().unwrap().min(body.len() - 1);
                    let _ = write!(
                        s,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{e}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        e - a + 1
                    );
                    let _ = s.write_all(&body[a..=e]);
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
    Mock { port, seen, creds }
}

/// A listener on 127.0.0.2 (not covered by the loopback bypass of the proxy
/// setting, so a request to it must go through the proxy). It counts every
/// connection: a "direct" connection that escaped the proxy shows up here.
fn trap() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.2:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            h2.fetch_add(1, Ordering::SeqCst);
            let mut b = [0u8; 512];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }
    });
    (port, hits)
}

const XML: &str = r#"<rss xmlns:torznab="x"><channel>
<item><title>Sample Clip 01</title><link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link>
<torznab:attr name="seeders" value="3"/></item></channel></rss>"#;

fn mp4() -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0x18];
    b.extend_from_slice(b"ftypisom");
    b.extend((0..20_000u32).map(|i| (i % 251) as u8));
    b
}

fn site() -> Handler {
    Arc::new(|host, path| {
        match (host, path.split('?').next().unwrap_or("")) {
        ("indexer.invalid", _) => XML.as_bytes().to_vec(),
        ("src.invalid", _) => br#"{"data":[{"name":"A","play":"http://media.invalid:8081/v.mp4"}]}"#.to_vec(),
        ("media.invalid", "/v.mp4") => mp4(),
        ("cdn.invalid", "/list.m3u8") => b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"http://keys.invalid/k\"\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n".to_vec(),
        ("cdn.invalid", "/s0.ts") => vec![7; 32],
        ("keys.invalid", "/k") => vec![1; 16],
        _ => Vec::new(),
    }
    })
}

fn source_json(host: &str) -> sources::SourceDef {
    let mut d = sources::parse_import(
        format!(
            r#"{{"name":"s","search_url":"http://{host}/s?q={{query}}","items":"/data",
                "fields":{{"title":"/name","media":"/play"}}}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    d.id = 1;
    d.per_minute = 0;
    d
}

struct Env {
    svc: FetchService,
    _dl: tempfile::TempDir,
    _st: tempfile::TempDir,
}

fn env(cfg: FetchConfig) -> Env {
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            ..cfg
        },
        st.path().into(),
    )
    .unwrap();
    Env {
        svc,
        _dl: dl,
        _st: st,
    }
}

fn indexer(url: &str) -> Indexer {
    Indexer {
        id: 1,
        name: "mock".into(),
        url: url.into(),
        api_key: Secret::new("k3y"),
        enabled: true,
    }
}

fn wait<T>(rx: std::sync::mpsc::Receiver<T>) -> T {
    rx.recv_timeout(Duration::from_secs(40)).expect("an answer")
}

fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

impl Env {
    fn svc_indexer(&self) -> Indexer {
        indexer("http://indexer.invalid:9117/api")
    }
}

fn proxied_cfg(proxy: &str) -> FetchConfig {
    FetchConfig {
        indexers: vec![indexer("http://indexer.invalid:9117/api")],
        sources: vec![source_json("src.invalid:8080")],
        proxy: Some(Secret::new(proxy)),
        ..Default::default()
    }
}

fn run_through_proxy(auth: Option<(&'static str, &'static str)>) {
    let m = mock(Mode::Serve, auth, site());
    let proxy = match auth {
        Some((u, p)) => format!("socks5://{u}:{p}@127.0.0.1:{}", m.port),
        None => m.url(),
    };
    let e = env(proxied_cfg(&proxy));
    // Search: indexer + source.
    let out = wait(e.svc.search(None, "clip", &[], 5, cancel()));
    assert_eq!((out.queried, out.failed), (2, 0), "{out:?}");
    assert_eq!(out.results.len(), 2);
    // Indexer test.
    wait(e.svc.test_indexer(&e.svc_indexer())).ok();
    // Ranged stream open.
    let t = wait(e.svc.stream_url("http://media.invalid:8081/v.mp4".into()));
    assert!(matches!(t, Ok(StreamTicket::Live { .. })));
    // HLS with an encrypted segment: the key is fetched too.
    let StreamTicket::Timed { mut source, .. } = wait(
        e.svc
            .open_any("http://cdn.invalid/list.m3u8".into(), StreamOpts::default()),
    )
    .expect("hls ticket") else {
        panic!("timed stream expected");
    };
    let t0 = Instant::now();
    while !m.hosts().iter().any(|h| h == "keys.invalid") && t0.elapsed() < Duration::from_secs(20) {
        let _ = source.pull();
        std::thread::sleep(Duration::from_millis(20));
    }
    let seen = m.seen.lock().unwrap().clone();
    for want in [
        "indexer.invalid",
        "src.invalid",
        "media.invalid",
        "cdn.invalid",
        "keys.invalid",
    ] {
        assert!(
            seen.iter().any(|s| s.host == want),
            "{want} not proxied: {seen:?}"
        );
    }
    // Names travel as names: the proxy resolves them (socks5h).
    assert!(
        seen.iter().all(|s| s.domain),
        "an address was resolved locally: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|s| s.host == "indexer.invalid" && s.port == 9117)
    );
    assert!(
        seen.iter()
            .any(|s| s.host == "keys.invalid" && s.path == "/k")
    );
    if let Some((u, p)) = auth {
        let c = m.creds.lock().unwrap();
        assert!(!c.is_empty() && c.iter().all(|(a, b)| a == u && b == p));
    }
}

#[test]
fn every_request_goes_through_the_proxy_without_auth() {
    let _g = serial();
    run_through_proxy(None);
}

#[test]
fn every_request_goes_through_the_proxy_with_user_password() {
    let _g = serial();
    run_through_proxy(Some(("alice", "p@ss word")));
}

fn fail_closed(mode: Mode, dead_port: bool) {
    let (tport, hits) = trap();
    let port = if dead_port {
        // Nobody listens there any more.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    } else {
        mock(mode, None, site()).port
    };
    let target = format!("127.0.0.2:{tport}");
    let e = env(FetchConfig {
        indexers: vec![indexer(&format!("http://{target}/api"))],
        sources: vec![source_json(&target)],
        proxy: Some(Secret::new(format!("socks5://127.0.0.1:{port}"))),
        ..Default::default()
    });
    let out = wait(e.svc.search(None, "clip", &[], 5, cancel()));
    assert_eq!((out.failed, out.results.len()), (2, 0));
    assert!(
        wait(
            e.svc
                .test_indexer(&indexer(&format!("http://{target}/api")))
        )
        .is_err()
    );
    assert!(wait(e.svc.stream_url(format!("http://{target}/v.mp4"))).is_err());
    assert!(
        wait(
            e.svc
                .open_any(format!("http://{target}/v.mp4"), StreamOpts::default())
        )
        .is_err()
    );
    assert!(wait(e.svc.play_http(format!("http://{target}/v.mp4"))).is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "a request escaped the proxy"
    );
}

#[test]
fn a_dropping_proxy_fails_closed() {
    let _g = serial();
    fail_closed(Mode::Drop, false);
}

#[test]
fn a_refusing_proxy_fails_closed() {
    let _g = serial();
    fail_closed(Mode::Refuse, false);
}

#[test]
fn a_dead_proxy_fails_closed() {
    let _g = serial();
    fail_closed(Mode::Serve, true);
}

#[test]
fn require_proxy_without_proxy_connects_to_nothing() {
    let _g = serial();
    let (tport, hits) = trap();
    let target = format!("127.0.0.2:{tport}");
    let marker = tempfile::tempdir().unwrap();
    let ran = marker.path().join("ran");
    let script = marker.path().join("resolver.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ntouch '{}'\necho '{{\"formats\":[]}}'\n",
            ran.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(marker.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let resolver = |id, honours| ResolverDef {
        id,
        name: "r".into(),
        argv: vec![script.to_str().unwrap().into(), "{url}".into()],
        timeout_secs: 5,
        enabled: true,
        honours_proxy: honours,
    };
    let e = env(FetchConfig {
        indexers: vec![indexer(&format!("http://{target}/api"))],
        sources: vec![source_json(&target)],
        resolvers: vec![resolver(1, false), resolver(2, true)],
        require_proxy: true,
        ..Default::default()
    });
    let out = wait(e.svc.search(None, "clip", &[], 5, cancel()));
    assert_eq!((out.failed, out.results.len()), (2, 0));
    assert!(
        wait(
            e.svc
                .test_indexer(&indexer(&format!("http://{target}/api")))
        )
        .is_err()
    );
    assert!(wait(e.svc.stream_url(format!("http://{target}/v.mp4"))).is_err());
    assert!(wait(e.svc.play_http(format!("http://{target}/v.mp4"))).is_err());
    assert!(
        wait(
            e.svc
                .open_any(format!("http://{target}/page"), StreamOpts::default())
        )
        .is_err()
    );
    assert!(wait(e.svc.stream_via_resolver(1, format!("http://{target}/p"))).is_err());
    assert!(wait(e.svc.stream_via_resolver(2, format!("http://{target}/p"))).is_err());
    // Torrent session: refused before any socket is opened.
    assert!(
        wait(e.svc.add(Source::Magnet(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into()
        )))
        .is_err()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(hits.load(Ordering::SeqCst), 0, "something connected");
    assert!(!ran.exists(), "a resolver started without a proxy");
}

#[cfg(unix)]
#[test]
fn the_proxy_pair_is_filled_in_and_the_media_goes_through_the_proxy() {
    use std::os::unix::fs::PermissionsExt;
    let _g = serial();
    let m = mock(Mode::Serve, None, site());
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let args = dir.path().join("args");
    // The stub records its arguments, then answers with a media address that
    // only the proxy's site knows.
    let script = format!(
        r#"printf '%s\n' "$@" > '{}'; printf '%s' '{{"url":"http://media.invalid:8081/v.mp4","vcodec":"avc1","acodec":"mp4a","title":"T"}}'"#,
        args.display()
    );
    let d = ResolverDef {
        id: 1,
        name: "r".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            script,
            "sh".into(),
            "--proxy".into(),
            "{proxy}".into(),
            "{url}".into(),
        ],
        timeout_secs: 10,
        enabled: true,
        // Not declared: the pair alone makes it proxy-aware, so the kill
        // switch lets it run.
        honours_proxy: false,
    };
    let e = env(FetchConfig {
        resolvers: vec![d],
        proxy: Some(Secret::new(m.url())),
        require_proxy: true,
        ..Default::default()
    });
    let t = wait(
        e.svc
            .stream_via_resolver(1, "http://page.invalid/watch".into()),
    );
    assert!(matches!(t, Ok(StreamTicket::Live { .. })), "{:?}", t.err());
    let got = std::fs::read_to_string(&args).unwrap();
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(
        lines,
        [
            "--proxy",
            &format!("socks5h://127.0.0.1:{}", m.port),
            "http://page.invalid/watch"
        ]
    );
    let hosts = m.hosts();
    assert!(hosts.iter().any(|h| h == "media.invalid"), "{hosts:?}");
}
