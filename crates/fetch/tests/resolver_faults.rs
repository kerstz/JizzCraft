//! A page, a stub resolver and a loopback "site": what the user is told when
//! the resolver cannot help, what is never written to disk, and what the
//! detection of installed tools accepts. Synthetic bytes, `.invalid`/loopback
//! only, no real site.
#![cfg(all(feature = "network", unix))]

#[path = "support/fault_server.rs"]
mod fault_server;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use fault_server::{Cfg, Route, Server, mp4_moov_last, pattern, start};
use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::detect_tools::{age_days_at, civil_days, find_resolver_tools, vetted_argv};
use jizzcraft_fetch::errors::{self, classify};
use jizzcraft_fetch::resolver::{self, ResolverDef};
use jizzcraft_fetch::service::{FetchService, StreamOpts, StreamTicket};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const SECRET_TITLE: &str = "secret-title-XYZ";

fn site() -> Server {
    let mut c = Cfg::new(mp4_moov_last(20_000));
    c.routes = vec![
        Route::new(
            "/page",
            "text/html",
            b"<html><body>hello</body></html>".to_vec(),
            true,
        ),
        Route::new("/v.mp4", "video/mp4", mp4_moov_last(30_000), true),
        Route::new("/norange.mp4", "video/mp4", mp4_moov_last(30_000), false),
        Route::new(
            "/m.m3u8",
            "application/vnd.apple.mpegurl",
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\ns0.ts\n#EXT-X-ENDLIST\n".to_vec(),
            true,
        ),
        Route::new("/s0.ts", "video/mp2t", pattern(600), true),
    ];
    start(c)
}

fn def(id: u32, script: &str, honours: bool) -> ResolverDef {
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
        honours_proxy: honours,
    }
}

/// A resolver that says `phrase` (with the site's address and a title in it)
/// on stderr and exits with `code`.
fn failing(id: u32, port: u16, phrase: &str, code: i32) -> ResolverDef {
    def(
        id,
        &format!(
            "echo 'ERROR: {phrase}: http://127.0.0.1:{port}/page {SECRET_TITLE}' >&2; exit {code}"
        ),
        false,
    )
}

fn good(id: u32, port: u16, path: &str, title: Option<&str>) -> ResolverDef {
    let t = title.map_or(String::new(), |t| format!(r#","title":"{t}""#));
    def(
        id,
        &format!(
            r#"printf '%s' '{{"url":"http://127.0.0.1:{port}{path}","vcodec":"avc1","acodec":"mp4a","height":720{t}}}'"#
        ),
        false,
    )
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

fn open(s: &FetchService, url: String) -> Result<StreamTicket, String> {
    s.open_any(url, StreamOpts::default())
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
}

fn via(s: &FetchService, id: u32, url: String) -> Result<StreamTicket, String> {
    s.stream_via_resolver(id, url)
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
}

/// What may never be in what the user is told.
fn clean(raw: &str, port: u16) {
    let shown = classify(raw);
    assert!(errors::ALL.contains(&shown), "{shown}");
    for text in [raw, shown] {
        assert!(!text.contains("127.0.0.1"), "{text}");
        assert!(!text.contains(&port.to_string()), "{text}");
        assert!(!text.contains("/page"), "{text}");
        assert!(
            !text.contains("XYZ") && !text.contains(SECRET_TITLE),
            "{text}"
        );
    }
}

#[test]
fn each_fault_has_its_exact_sentence_and_leaks_nothing() {
    let _g = serial();
    let srv = site();
    let p = srv.port;
    let cases: [(&str, i32, &str, &str); 9] = [
        (
            "Unsupported URL",
            1,
            "resolver unsupported",
            errors::RESOLVER_UNSUPPORTED,
        ),
        (
            "Sign in to confirm your age",
            1,
            "resolver age wall",
            errors::AGE_WALL,
        ),
        (
            "Please log in to watch",
            1,
            "resolver login",
            errors::LOGIN_REQUIRED,
        ),
        (
            "Not available in your country",
            1,
            "resolver geo",
            errors::GEO_BLOCKED,
        ),
        (
            "This video is DRM protected",
            1,
            "resolver drm",
            errors::PROTECTED,
        ),
        (
            "HTTP Error 429 Too Many Requests",
            1,
            "resolver rate limit",
            errors::RATE_LIMITED,
        ),
        ("Video unavailable", 1, "resolver gone", errors::NOT_FOUND),
        (
            "anything",
            127,
            "resolver missing",
            errors::RESOLVER_MISSING,
        ),
        (
            "something broke",
            1,
            "resolver failed",
            errors::RESOLVER_FAILED,
        ),
    ];
    for (phrase, code, keyword, sentence) in cases {
        let (s, dl, _st) = svc(FetchConfig {
            resolvers: vec![failing(1, p, phrase, code)],
            ..Default::default()
        });
        let e = via(&s, 1, srv.url("/page")).err().expect("an error");
        assert_eq!(e, keyword, "{phrase}");
        assert_eq!(classify(&e), sentence, "{phrase}");
        clean(&e, p);
        // Same through open_any (a page), except "failed" which is the blanket.
        let e = open(&s, srv.url("/page")).err().expect("an error");
        clean(&e, p);
        if keyword == "resolver failed" {
            assert_eq!(classify(&e), errors::NO_VIDEO_HERE);
        } else {
            assert_eq!(classify(&e), sentence, "{phrase}");
        }
        assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
    }
}

#[test]
fn a_program_that_is_not_there_is_reported_as_missing() {
    let _g = serial();
    let srv = site();
    let gone = ResolverDef {
        id: 1,
        name: "r".into(),
        argv: vec!["/nonexistent-dir/tool".into(), "{url}".into()],
        timeout_secs: 5,
        enabled: true,
        honours_proxy: false,
    };
    let (s, _a, _b) = svc(FetchConfig {
        resolvers: vec![gone],
        ..Default::default()
    });
    let e = via(&s, 1, srv.url("/page")).err().unwrap();
    assert_eq!(classify(&e), errors::RESOLVER_MISSING);
}

#[test]
fn the_most_specific_fault_wins_across_resolvers() {
    let _g = serial();
    let srv = site();
    let p = srv.port;
    let run = |list: Vec<ResolverDef>| {
        let (s, _a, _b) = svc(FetchConfig {
            resolvers: list,
            ..Default::default()
        });
        let e = open(&s, srv.url("/page")).err().unwrap();
        clean(&e, p);
        classify(&e)
    };
    let f = |id, phrase| failing(id, p, phrase, 1);
    assert_eq!(
        run(vec![
            f(1, "Video unavailable"),
            f(2, "DRM protected"),
            f(3, "please log in")
        ]),
        errors::PROTECTED
    );
    assert_eq!(
        run(vec![
            f(1, "not available in your country"),
            f(2, "please log in")
        ]),
        errors::LOGIN_REQUIRED
    );
    assert_eq!(
        run(vec![f(1, "please log in"), f(2, "confirm your age")]),
        errors::AGE_WALL
    );
    assert_eq!(
        run(vec![f(1, "Video unavailable"), f(2, "boom")]),
        errors::NOT_FOUND
    );
    assert_eq!(
        run(vec![f(1, "too many requests"), f(2, "Unsupported URL")]),
        errors::RATE_LIMITED
    );
    // Nothing specific: the blanket sentence, as before.
    assert_eq!(run(vec![f(1, "boom")]), errors::NO_VIDEO_HERE);
}

#[test]
fn a_resolver_ticket_is_named_by_the_title_never_the_cdn_file() {
    let _g = serial();
    let srv = site();
    let p = srv.port;
    let (s, dl, _st) = svc(FetchConfig {
        resolvers: vec![
            good(1, p, "/v.mp4", Some("My Clip")),
            good(2, p, "/m.m3u8", None),
        ],
        ..Default::default()
    });
    match via(&s, 1, srv.url("/page")).expect("a ticket") {
        StreamTicket::Live { name, len, .. } => {
            assert_eq!(name, "My Clip");
            assert_eq!(len, mp4_moov_last(30_000).len() as u64);
        }
        _ => panic!("a seekable reader was expected"),
    }
    match via(&s, 2, srv.url("/page")).expect("a ticket") {
        StreamTicket::Timed { name, .. } => assert_eq!(name, "Video"),
        _ => panic!("a timed stream was expected"),
    }
    assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
}

#[test]
fn a_source_that_cannot_seek_is_never_saved_for_a_page() {
    let _g = serial();
    let srv = site();
    let p = srv.port;
    let (s, dl, _st) = svc(FetchConfig {
        resolvers: vec![good(1, p, "/norange.mp4", Some("Clip"))],
        ..Default::default()
    });
    for r in [via(&s, 1, srv.url("/page")), open(&s, srv.url("/page"))] {
        let e = r.err().expect("refused");
        assert_eq!(classify(&e), errors::NEEDS_SAVING);
        clean(&e, p);
    }
    std::thread::sleep(Duration::from_millis(300));
    let left: Vec<_> = std::fs::read_dir(dl.path()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(srv.active.load(Ordering::SeqCst), 0);
}

#[test]
fn require_proxy_without_proxy_opens_no_socket_and_starts_nothing() {
    let _g = serial();
    let srv = site();
    let marker = tempfile::tempdir().unwrap();
    std::fs::set_permissions(marker.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let ran = marker.path().join("ran");
    let script = format!("touch '{}'; exit 1", ran.display());
    let mut aware = def(2, &script, false);
    // Counts as proxy-aware through the placeholder pair alone.
    aware.argv.insert(4, "--proxy".into());
    aware.argv.insert(5, "{proxy}".into());
    let (s, _a, _b) = svc(FetchConfig {
        require_proxy: true,
        resolvers: vec![def(1, &script, false), aware],
        ..Default::default()
    });
    for r in [
        open(&s, srv.url("/page")),
        open(&s, srv.url("/m.m3u8")),
        via(&s, 1, srv.url("/page")),
        via(&s, 2, srv.url("/page")),
    ] {
        assert_eq!(classify(&r.err().unwrap()), errors::PROXY_REQUIRED);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(srv.hits.load(Ordering::SeqCst), 0, "a socket was opened");
    assert!(!ran.exists(), "a resolver started without a proxy");
}

#[test]
fn the_proxy_pair_is_dropped_when_there_is_no_proxy() {
    let _g = serial();
    // A service without a proxy publishes "no proxy" to the helpers.
    let (_s, _a, _b) = svc(FetchConfig::default());
    let mut d = def(
        1,
        r#"printf '{"url":"https://a.invalid/x.mp4","title":"n=%s"}' "$#""#,
        false,
    );
    // sh -c script sh [--proxy {proxy}] {url}
    d.argv.insert(4, "--proxy".into());
    d.argv.insert(5, "{proxy}".into());
    assert!(resolver::validate(&d).is_ok());
    let (_f, meta) = resolver::run_meta(&d, "https://page.invalid/").unwrap();
    assert_eq!(meta.title.as_deref(), Some("n=1"));
    // The same program without the pair sees the same count.
    d.argv.drain(4..6);
    let (_f, meta) = resolver::run_meta(&d, "https://page.invalid/").unwrap();
    assert_eq!(meta.title.as_deref(), Some("n=1"));
}

// ----- detection of installed tools -----

fn stub(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p
}

fn private_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

#[test]
fn installed_tools_are_found_with_version_age_and_a_vetted_argv() {
    let _g = serial();
    let d = private_dir();
    let tool = stub(d.path(), "yt-dlp", "echo 2026.01.01", 0o700);
    // Not a tool name: never run, never listed.
    let marker = d.path().join("ran-other");
    stub(
        d.path(),
        "other-tool",
        &format!("touch '{}'; echo 2026.01.01", marker.display()),
        0o700,
    );
    let found = find_resolver_tools(&[d.path().to_path_buf()]);
    let f = found
        .iter()
        .find(|f| f.path.starts_with(d.path()))
        .expect("the stub is found");
    assert_eq!(f.path, tool);
    assert_eq!(f.version, "2026.01.01");
    let want = age_days_at("2026.01.01", civil_days(2026, 10, 14)).unwrap();
    assert_eq!(want, 286);
    assert!(f.age_days.is_some());
    assert!(!marker.exists());
    assert_eq!(
        vetted_argv(&f.path),
        vec![
            tool.to_str().unwrap(),
            "-J",
            "--no-playlist",
            "--ignore-config",
            "--proxy",
            "{proxy}",
            "--",
            "{url}"
        ]
    );
    // The vetted vector is a valid resolver as it is.
    let ok = ResolverDef {
        id: 1,
        name: "t".into(),
        argv: vetted_argv(&f.path),
        timeout_secs: 10,
        enabled: true,
        honours_proxy: false,
    };
    assert!(resolver::validate(&ok).is_ok());
}

#[test]
fn tools_on_path_are_found_and_the_environment_is_cleared() {
    let _g = serial();
    let d = private_dir();
    unsafe { std::env::set_var("JIZZ_DETECT_SECRET", "leak") };
    stub(
        d.path(),
        "youtube-dl",
        r#"echo "2025.12.${JIZZ_DETECT_SECRET:-31}""#,
        0o700,
    );
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![d.path().to_path_buf()];
    paths.extend(std::env::split_paths(&old));
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    let found = find_resolver_tools(&[]);
    unsafe { std::env::set_var("PATH", old) };
    let f = found
        .iter()
        .find(|f| f.path.starts_with(d.path()))
        .expect("found through PATH");
    assert_eq!(f.version, "2025.12.31");
    assert!(f.age_days.is_some());
}

#[test]
fn world_writable_tools_and_folders_are_refused() {
    let _g = serial();
    let d = private_dir();
    stub(d.path(), "yt-dlp", "echo 2026.01.01", 0o757);
    let found = find_resolver_tools(&[d.path().to_path_buf()]);
    assert!(
        !found.iter().any(|f| f.path.starts_with(d.path())),
        "a world-writable program was listed"
    );
    // A tool that does not answer a version is not listed either.
    let d2 = private_dir();
    stub(d2.path(), "yt-dlp", "exit 3", 0o700);
    let found = find_resolver_tools(&[d2.path().to_path_buf()]);
    assert!(!found.iter().any(|f| f.path.starts_with(d2.path())));
}
