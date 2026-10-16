//! The built-in page reader against a loopback "site": what it plays, what it
//! refuses to look at, what it never sends. Synthetic bytes, loopback only.
#![cfg(feature = "network")]
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]

#[path = "support/fault_server.rs"]
mod fault_server;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fault_server::{mp4_moov_last, pattern};
use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::config::{ExtractorMode, SaveSettings};
use jizzcraft_fetch::errors::{self, classify};
use jizzcraft_fetch::service::{
    ExtractMode, Extraction, FetchService, StreamOpts, StreamTicket, Via, set_page_deadline,
};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone)]
enum Body {
    Bytes(Vec<u8>),
    /// A prefix at once, then one blank byte every 50 ms, `n` times.
    Trickle(Vec<u8>, usize),
}

#[derive(Clone)]
struct Route {
    path: String,
    ctype: String,
    body: Body,
}

fn route(path: &str, ctype: &str, body: impl Into<Vec<u8>>) -> Route {
    Route {
        path: path.into(),
        ctype: ctype.into(),
        body: Body::Bytes(body.into()),
    }
}

fn page(path: &str, html: &str) -> Route {
    route(path, "text/html", html.as_bytes())
}

struct Site {
    port: u16,
    log: Arc<Mutex<Vec<Req>>>,
    written: Arc<AtomicUsize>,
}

impl Site {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
    fn count(&self, path: &str) -> usize {
        self.requests().iter().filter(|r| r.path == path).count()
    }
}

/// Every response carries a cookie, so a client that kept it would show.
fn start(routes: Vec<Route>) -> Site {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let written = Arc::new(AtomicUsize::new(0));
    let (log2, written2) = (log.clone(), written.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let (routes, log, written) = (routes.clone(), log2.clone(), written2.clone());
            std::thread::spawn(move || {
                let mut req = Vec::new();
                let mut buf = [0u8; 2048];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&req).to_string();
                let mut lines = text.lines();
                let first = lines.next().unwrap_or("");
                let mut parts = first.split_whitespace();
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("/").to_owned();
                let headers: Vec<(String, String)> = lines
                    .take_while(|l| !l.is_empty())
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let range = headers
                    .iter()
                    .find(|(k, _)| k == "range")
                    .and_then(|(_, v)| v.strip_prefix("bytes=").map(str::to_owned));
                log.lock().unwrap().push(Req {
                    method,
                    path: path.clone(),
                    headers,
                });
                let bare = path.split('?').next().unwrap_or("");
                let Some(r) = routes.iter().find(|r| r.path == bare) else {
                    let _ = write!(
                        s,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    return;
                };
                let write_all = |s: &mut std::net::TcpStream, b: &[u8]| {
                    let mut done = 0;
                    for c in b.chunks(16 * 1024) {
                        if s.write_all(c).is_err() {
                            break;
                        }
                        done += c.len();
                        written.fetch_add(c.len(), Ordering::SeqCst);
                    }
                    done
                };
                match &r.body {
                    Body::Bytes(body) => {
                        let total = body.len();
                        match range {
                            Some(rg) => {
                                let (a, b) = rg.split_once('-').unwrap_or(("0", "0"));
                                let a: usize = a.trim().parse().unwrap_or(0);
                                let b: usize = b
                                    .trim()
                                    .parse::<usize>()
                                    .unwrap_or(total - 1)
                                    .min(total - 1);
                                let _ = write!(
                                    s,
                                    "HTTP/1.1 206 Partial Content\r\nContent-Type: {}\r\nSet-Cookie: sid=1\r\nContent-Range: bytes {a}-{b}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    r.ctype,
                                    b - a + 1
                                );
                                write_all(&mut s, &body[a..=b]);
                            }
                            None => {
                                let _ = write!(
                                    s,
                                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nSet-Cookie: sid=1\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
                                    r.ctype
                                );
                                write_all(&mut s, body);
                            }
                        }
                    }
                    Body::Trickle(prefix, n) => {
                        let _ = write!(
                            s,
                            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            r.ctype,
                            prefix.len() + n
                        );
                        let _ = s.write_all(prefix);
                        let _ = s.flush();
                        for _ in 0..*n {
                            std::thread::sleep(Duration::from_millis(50));
                            if s.write_all(b" ").and_then(|()| s.flush()).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    Site { port, log, written }
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

fn open(s: &FetchService, url: String) -> Result<StreamTicket, String> {
    s.open_any(url, StreamOpts::default())
        .recv_timeout(Duration::from_secs(30))
        .expect("an answer")
}

fn extract(s: &FetchService, url: String) -> Result<Extraction, String> {
    s.extract(url, ExtractMode::Auto)
        .recv_timeout(Duration::from_secs(30))
        .expect("an answer")
}

fn media() -> Vec<u8> {
    mp4_moov_last(30_000)
}

fn seg(i: usize) -> Vec<u8> {
    pattern(300 + i)
}

fn playlist() -> Vec<u8> {
    let mut s = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
    for i in 0..2 {
        s.push_str(&format!("#EXTINF:2,\nseg{i}.ts\n"));
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s.into_bytes()
}

#[test]
fn page_with_og_video_streams_through_open_any() {
    let _g = serial();
    let site = start(vec![
        page(
            "/page",
            r#"<meta property="og:title" content="Sample Clip">
               <meta property="og:video" content="/v.mp4">"#,
        ),
        route("/v.mp4", "video/mp4", media()),
    ]);
    let (s, dl, _st) = plain();
    let StreamTicket::Live { name, len, .. } = open(&s, site.url("/page")).expect("ticket") else {
        panic!("expected a live stream");
    };
    assert_eq!(name, "Sample Clip");
    assert_eq!(len, media().len() as u64);
    // A page's video is streamed, never written.
    assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
}

#[test]
fn page_with_hls_master_link_streams_timed() {
    let _g = serial();
    let site = start(vec![
        page(
            "/page",
            r#"<link rel="alternate" type="application/x-mpegURL" href="/list.m3u8">"#,
        ),
        route("/list.m3u8", "application/vnd.apple.mpegurl", playlist()),
        route("/seg0.ts", "video/mp2t", seg(0)),
        route("/seg1.ts", "video/mp2t", seg(1)),
    ]);
    let (s, _a, _b) = plain();
    let StreamTicket::Timed { mut source, .. } = open(&s, site.url("/page")).expect("ticket")
    else {
        panic!("expected a timed stream");
    };
    let mut out = Vec::new();
    let t0 = Instant::now();
    loop {
        match source.pull() {
            Ok(Some(c)) => out.extend_from_slice(&c.bytes),
            Ok(None) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(20));
    }
    let mut want = seg(0);
    want.extend(seg(1));
    assert_eq!(out, want);
}

#[test]
fn embed_followed_once_with_referer() {
    let _g = serial();
    let site = start(vec![
        page("/page", r#"<h1>Hi</h1><iframe src="/embed"></iframe>"#),
        page(
            "/embed",
            r#"<video src="/v.mp4"></video><iframe src="/deeper"></iframe>"#,
        ),
        page("/deeper", r#"<video src="/other.mp4"></video>"#),
        route("/v.mp4", "video/mp4", media()),
        route("/other.mp4", "video/mp4", media()),
    ]);
    let (s, _a, _b) = plain();
    let ex = extract(&s, site.url("/page")).expect("an extraction");
    assert_eq!(ex.via, Via::Builtin { embed: true });
    assert_eq!(ex.entries.len(), 1);
    assert_eq!(ex.entries[0].formats[0].url, site.url("/v.mp4"));
    assert!(matches!(
        open(&s, site.url("/page")),
        Ok(StreamTicket::Live { .. })
    ));
    // The second level is never read.
    assert_eq!(site.count("/deeper"), 0);
    assert_eq!(site.count("/other.mp4"), 0);
    let reqs = site.requests();
    let embed = reqs.iter().find(|r| r.path == "/embed").unwrap();
    assert_eq!(embed.header("referer"), Some(site.url("/page").as_str()));
    let v = reqs.iter().find(|r| r.path == "/v.mp4").unwrap();
    assert_eq!(v.header("referer"), Some(site.url("/embed").as_str()));
    // The page itself carried no referer.
    assert_eq!(
        reqs.iter()
            .find(|r| r.path == "/page")
            .unwrap()
            .header("referer"),
        None
    );

    // Switched off: the player is not looked into.
    let (s2, _c, _d) = svc(FetchConfig {
        save: SaveSettings {
            follow_embeds: false,
            ..Default::default()
        },
        ..Default::default()
    });
    let before = site.count("/embed");
    let e = extract(&s2, site.url("/page")).unwrap_err();
    assert_eq!(classify(&e), errors::NO_VIDEO_HERE);
    assert_eq!(site.count("/embed"), before);
}

#[test]
fn decline_sentences() {
    let _g = serial();
    let spa = r#"<div id="root"></div><script src="a"></script><script src="b"></script><script src="c"></script>"#;
    let site = start(vec![
        page(
            "/age",
            "<h1>Are you 18 or older?</h1><form><button>Enter</button></form>",
        ),
        page("/login", r#"<form><input type="password" name="p"></form>"#),
        page("/spa", spa),
        page("/drm", "<p>This title is protected by Widevine.</p>"),
        page("/plain", "<p>nothing here</p>"),
        route("/v.mp4", "video/mp4", media()),
    ]);
    let (s, _a, _b) = plain();
    for (path, want) in [
        ("/age", errors::AGE_WALL),
        ("/login", errors::LOGIN_REQUIRED),
        ("/spa", errors::NEEDS_JS),
        ("/drm", errors::PROTECTED),
        ("/plain", errors::NO_VIDEO_HERE),
        ("/missing", errors::NOT_FOUND),
    ] {
        let e = open(&s, site.url(path)).err().unwrap_or_default();
        assert_eq!(classify(&e), want, "{path}: {e}");
        assert!(errors::ALL.contains(&classify(&e)));
        assert!(!e.contains("127.0.0.1"), "{e}");
    }
    assert_eq!(site.count("/v.mp4"), 0);
    // Only the pages themselves were asked for (open_any sniffs, then reads).
    assert!(site.requests().iter().all(|r| r.method == "GET"));
}

#[test]
fn huge_page_is_cut_at_cap() {
    let _g = serial();
    let site = start(vec![route("/big", "text/html", vec![b'a'; 50 << 20])]);
    let (s, _a, _b) = plain();
    let t0 = Instant::now();
    let e = extract(&s, site.url("/big")).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    assert_eq!(classify(&e), errors::NO_VIDEO_HERE);
    std::thread::sleep(Duration::from_millis(500));
    let sent = site.written.load(Ordering::SeqCst);
    assert!(sent < 40 << 20, "the connection was kept: {sent} bytes");
}

#[test]
fn trickling_page_hits_total_deadline() {
    let _g = serial();
    set_page_deadline(Some(Duration::from_secs(1)));
    let site = start(vec![
        Route {
            path: "/slow".into(),
            ctype: "text/html".into(),
            body: Body::Trickle(
                br#"<meta property="og:video" content="/v.mp4">"#.to_vec(),
                400,
            ),
        },
        route("/v.mp4", "video/mp4", media()),
    ]);
    let (s, _a, _b) = plain();
    let t0 = Instant::now();
    let r = extract(&s, site.url("/slow"));
    set_page_deadline(None);
    assert!(t0.elapsed() < Duration::from_secs(6), "{:?}", t0.elapsed());
    // What arrived before the deadline is what was read.
    let ex = r.expect("the early part of the page was scanned");
    assert_eq!(ex.entries[0].formats[0].url, site.url("/v.mp4"));
}

#[test]
fn no_cookie_no_post_only_allowed_headers() {
    let _g = serial();
    let site = start(vec![
        page(
            "/page",
            r#"<form method="post" action="/login"><input name="u"></form>
               <meta http-equiv="refresh" content="0; url=/elsewhere">
               <meta property="og:video" content="/v.mp4">"#,
        ),
        page("/login", "posted"),
        page("/elsewhere", "refreshed"),
        route("/v.mp4", "video/mp4", media()),
    ]);
    let (s, _a, _b) = plain();
    assert!(matches!(
        open(&s, site.url("/page")),
        Ok(StreamTicket::Live { .. })
    ));
    let reqs = site.requests();
    assert!(reqs.len() >= 3);
    let allowed = [
        "host",
        "user-agent",
        "accept",
        "referer",
        "range",
        "accept-encoding",
    ];
    for r in &reqs {
        assert_eq!(r.method, "GET", "{r:?}");
        for (k, _) in &r.headers {
            assert!(
                allowed.contains(&k.as_str()),
                "unexpected header {k} on {}",
                r.path
            );
        }
        assert!(r.header("cookie").is_none() && r.header("authorization").is_none());
        // The page reader's own requests carry the generic User-Agent (the
        // first ranged sniff of the address is the long-standing one).
        if r.header("range").is_none() {
            assert!(
                r.header("user-agent")
                    .is_some_and(|u| u.starts_with("Mozilla/5.0")),
                "{r:?}"
            );
        }
    }
    // A form is never submitted and a refresh never followed.
    assert_eq!(site.count("/login"), 0);
    assert_eq!(site.count("/elsewhere"), 0);
}

#[cfg(unix)]
mod modes {
    use super::*;
    use jizzcraft_fetch::resolver::ResolverDef;

    fn tool(id: u32, script: &str) -> ResolverDef {
        ResolverDef {
            id,
            name: format!("r{id}"),
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                script.into(),
                "sh".into(),
                "{url}".into(),
            ],
            timeout_secs: 10,
            enabled: true,
            honours_proxy: false,
        }
    }

    fn site() -> Site {
        start(vec![
            page(
                "/page",
                r#"<meta property="og:title" content="From page">
                   <meta property="og:video" content="/v.mp4">"#,
            ),
            route("/v.mp4", "video/mp4", media()),
            route("/r.mp4", "video/mp4", media()),
        ])
    }

    fn name_of(r: Result<StreamTicket, String>) -> Result<String, String> {
        match r? {
            StreamTicket::Live { name, .. } => Ok(name),
            _ => Err("not live".into()),
        }
    }

    #[test]
    fn mode_order_first_fallback_off() {
        let _g = serial();
        let site = site();
        let marker = tempfile::tempdir().unwrap();
        let ran = marker.path().join("ran");
        let good = tool(
            1,
            &format!(
                r#"touch '{}'; printf '{{"title":"From tool","formats":[{{"url":"http://127.0.0.1:{}/r.mp4","height":720}}]}}'"#,
                ran.display(),
                site.port
            ),
        );
        let bad = tool(1, &format!("touch '{}'; exit 1", ran.display()));
        let with = |m: ExtractorMode, r: &ResolverDef| {
            svc(FetchConfig {
                resolvers: vec![r.clone()],
                save: SaveSettings {
                    extractor: m,
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        // First: the built-in reader answers, the tool is not even started.
        let (s, _a, _b) = with(ExtractorMode::First, &good);
        assert_eq!(name_of(open(&s, site.url("/page"))).unwrap(), "From page");
        assert!(!ran.exists());
        // Fallback (the default): the tool first.
        let (s, _a, _b) = with(ExtractorMode::Fallback, &good);
        assert_eq!(name_of(open(&s, site.url("/page"))).unwrap(), "From tool");
        assert!(ran.exists());
        std::fs::remove_file(&ran).unwrap();
        // Fallback with a failing tool: the built-in reader takes over.
        let (s, _a, _b) = with(ExtractorMode::Fallback, &bad);
        assert_eq!(name_of(open(&s, site.url("/page"))).unwrap(), "From page");
        assert!(ran.exists());
        std::fs::remove_file(&ran).unwrap();
        // Off: tools only; a failing tool means failure, whatever the page holds.
        let (s, _a, _b) = with(ExtractorMode::Off, &bad);
        let before = site.count("/page");
        assert!(open(&s, site.url("/page")).is_err());
        assert!(ran.exists());
        // (the address was sniffed once, never read as a page)
        assert_eq!(site.count("/page"), before + 1);
        let (s, _a, _b) = with(ExtractorMode::Off, &good);
        assert_eq!(name_of(open(&s, site.url("/page"))).unwrap(), "From tool");
    }
}

#[test]
fn rule_candidates_rank_first() {
    use jizzcraft_fetch::extract::rules;
    let _g = serial();
    let site = start(vec![
        page(
            "/page",
            r#"<meta property="og:video" content="/og.mp4">
               <p data-file="/rule.mp4">x</p><h2>Rule title</h2>"#,
        ),
        route("/og.mp4", "video/mp4", media()),
        route("/rule.mp4", "video/mp4", media()),
    ]);
    let rule = rules::parse_import(
        br#"{"name":"t","hosts":["127.0.0.1"],
            "title":"<h2>(?P<title>[^<]+)</h2>",
            "media":[{"regex":"data-file=\"(?P<url>[^\"]+)\""}]}"#,
    )
    .unwrap();
    let (s, _a, _b) = svc(FetchConfig {
        extract_rules: rule,
        ..Default::default()
    });
    let ex = extract(&s, site.url("/page")).expect("an extraction");
    let urls: Vec<_> = ex.entries[0]
        .formats
        .iter()
        .map(|f| f.url.clone())
        .collect();
    assert_eq!(urls, [site.url("/rule.mp4"), site.url("/og.mp4")]);
    assert_eq!(ex.entries[0].meta.title.as_deref(), Some("Rule title"));
    // Rules only ever read; they cause no request of their own.
    assert_eq!(site.count("/rule.mp4"), 0);
    // A rule for another host does nothing here.
    let other = rules::parse_import(
        br#"{"name":"t","hosts":["other.example.invalid"],
            "media":[{"regex":"data-file=\"(?P<url>[^\"]+)\""}]}"#,
    )
    .unwrap();
    let (s2, _c, _d) = svc(FetchConfig {
        extract_rules: other,
        ..Default::default()
    });
    let ex = extract(&s2, site.url("/page")).unwrap();
    assert_eq!(ex.entries[0].formats.len(), 1);
}
