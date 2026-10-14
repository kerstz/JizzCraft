//! Security properties of the fetch crate that do not need a swarm:
//! hostile input to sources and resolvers, URL spellings that aim at the
//! local network, every network entry point behind the kill switch, and
//! secrets that must never reach an error, a toast or a listing.
//! Synthetic data only; everything stays on loopback.

use jizzcraft_fetch::sources::{self, SourceDef};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Hand-written hostile strings: line breaks in every spelling, NUL, escapes,
/// separators, structure characters, absurd lengths.
fn payloads() -> Vec<String> {
    let mut v: Vec<String> = [
        "",
        " ",
        "\t",
        "\r",
        "\n",
        "\r\n",
        "\n\r",
        "\0",
        "a\0b",
        "\x7f",
        "\x1b[31m",
        "\x08",
        "%0d%0a",
        "%0D%0AX-Evil: 1",
        "%00",
        "%",
        "%zz",
        "%e2%80%a8",
        "a\r\nX-Evil: y",
        "a\nX-Evil: y",
        "a\rX-Evil: y",
        "\u{0085}",
        "\u{2028}",
        "\u{2029}",
        "\u{feff}",
        "\u{202e}gnp.exe",
        "\u{200b}",
        "\u{3000}",
        "é",
        "日本語",
        "{",
        "}",
        "{query",
        "query}",
        "{query}}",
        "{{query}}",
        "{secret}",
        "{url}",
        "{page",
        "../..",
        "..\\..",
        "/~",
        "/~0",
        "/~1",
        "/~2",
        "/~9",
        "//",
        "/",
        "/a/b/../c",
        "http://",
        "https://@",
        "https://@/",
        "@evil.invalid",
        "evil.invalid/",
        "#frag",
        "?x=1",
        "&x=1",
        "\\",
        "\"",
        "'",
        "<script>",
        "Bearer \r\nX: y",
        "'; DROP TABLE media;--",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    v.push("/".repeat(100_000));
    v.push("A".repeat(70_000));
    v.push("é".repeat(5_000));
    v.push("\0".repeat(1_000));
    v.push("%0d%0a".repeat(20_000));
    v.push(format!("/{}", "a/".repeat(50_000)));
    v
}

fn base() -> Value {
    json!({
        "name": "mine",
        "search_url": "https://api.example.invalid/v1/s?q={query}&p={page}&n={limit}",
        "items": "/data",
        "fields": {"title": "/name", "media": "/play", "page_url": "/url", "size": "/bytes"},
        "auth_header": "X-Api-Key",
        "auth_prefix": "Token ",
        "user_agent": "Agent/1.0",
        "referer": "https://r.example.invalid/"
    })
}

/// Everything that is accepted must be safe to put on the wire.
fn assert_accepted_is_safe(d: &SourceDef) {
    let hdr = |s: &str| !s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
    for v in [
        d.user_agent.as_deref(),
        d.referer.as_deref(),
        Some(d.auth_prefix.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        assert!(hdr(v), "accepted header value with a line break");
        assert!(
            v.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b)),
            "accepted a non-printable header value"
        );
    }
    if let Some(h) = &d.auth_header {
        assert!(
            !h.is_empty()
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }
    assert!(
        !d.search_url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace()),
        "accepted a template with whitespace or control characters"
    );
}

fn set(v: &mut Value, path: &[&str], value: Value) {
    let mut cur = v;
    for k in &path[..path.len() - 1] {
        cur = &mut cur[*k];
    }
    cur[path[path.len() - 1]] = value;
}

#[test]
fn hostile_definitions_never_panic_and_accepted_ones_are_safe() {
    let slots: &[&[&str]] = &[
        &["name"],
        &["search_url"],
        &["items"],
        &["fields", "title"],
        &["fields", "media"],
        &["fields", "page_url"],
        &["fields", "size"],
        &["auth_header"],
        &["auth_prefix"],
        &["user_agent"],
        &["referer"],
    ];
    let mut accepted = 0;
    for p in payloads() {
        for slot in slots {
            let mut doc = base();
            set(&mut doc, slot, json!(p));
            let raw = serde_json::to_vec(&doc).unwrap();
            if let Ok(d) = sources::parse_import(&raw) {
                accepted += 1;
                assert_accepted_is_safe(&d);
            }
            // The same payload as a prefix of a plausible template.
            let mut doc = base();
            set(
                &mut doc,
                &["search_url"],
                json!(format!("https://api.example.invalid/{p}?q={{query}}")),
            );
            if let Ok(d) = sources::parse_import(&serde_json::to_vec(&doc).unwrap()) {
                assert_accepted_is_safe(&d);
            }
        }
    }
    // Sanity: the harness does accept ordinary values.
    assert!(accepted > 0);
    // Not JSON at all, truncated, deeply nested.
    for raw in [
        &b""[..],
        b"\0",
        b"{",
        b"[[[[[[[[[[",
        b"{\"name\":",
        b"\xff\xfe",
        "{\"name\":\"\u{2028}\"}".as_bytes(),
    ] {
        assert!(sources::parse_import(raw).is_err());
    }
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(sources::parse_import(deep.as_bytes()).is_err());
    // Unknown and wrong-typed fields.
    assert!(sources::parse_import(br#"{"name":5}"#).is_err());
    assert!(sources::parse_import(br#"[1,2]"#).is_err());
}

#[test]
fn search_terms_cannot_leave_their_slot() {
    let d = sources::parse_import(&serde_json::to_vec(&base()).unwrap()).unwrap();
    for q in payloads() {
        for (page, limit) in [(0, 0), (1, 50), (u32::MAX, u32::MAX)] {
            let u = sources::build_url(&d, &q, page, limit);
            assert!(
                u.starts_with("https://api.example.invalid/v1/s?q="),
                "{u:.80}"
            );
            assert!(
                u.bytes().all(|b| b.is_ascii_graphic()),
                "raw whitespace, control or non-ASCII in a built address"
            );
            // Exactly the three parameters the template had.
            assert_eq!(u.matches('?').count(), 1);
            assert_eq!(u.matches('&').count(), 2);
            assert!(!u.contains(['{', '}']));
        }
    }
}

fn public_def() -> SourceDef {
    sources::parse_import(&serde_json::to_vec(&base()).unwrap()).unwrap()
}

#[test]
fn hostile_answers_are_cleaned_and_bounded() {
    let d = public_def();
    let mut items = Vec::new();
    for p in payloads() {
        items.push(
            json!({"name": p, "play": format!("https://cdn.example.invalid/{p}"),
                          "url": p, "bytes": p}),
        );
    }
    items.push(json!({"name": 1, "play": 2, "url": null, "bytes": [1]}));
    items.push(json!({"name": {"a": 1}, "play": []}));
    let body = serde_json::to_vec(&json!({"data": items})).unwrap();
    let got = sources::parse_items(&d, &body, 10_000).unwrap();
    for it in &got {
        assert!(!it.title.chars().any(|c| c.is_control()));
        assert!(it.title.chars().count() <= 4096);
        for u in [&it.media, &it.page_url].into_iter().flatten() {
            assert!(u.starts_with("http://") || u.starts_with("https://"));
            assert!(!u.chars().any(|c| c.is_control()));
        }
    }
    // The cap on the number of items holds.
    assert!(sources::parse_items(&d, &body, 3).unwrap().len() <= 3);
    // Hostile pointers set directly (bypassing validation) still do not panic.
    for p in payloads() {
        let mut v = base();
        v["items"] = json!(p.clone());
        v["fields"]["title"] = json!(p.clone());
        v["fields"]["media"] = json!(p.clone());
        v["fields"]["size"] = json!(p);
        let Ok(raw) = serde_json::to_vec(&v) else {
            continue;
        };
        let def: SourceDef = serde_json::from_slice(&raw).unwrap();
        let _ = sources::parse_items(&def, &body, 50);
    }
}

#[test]
fn error_sentences_are_fixed() {
    // `&'static str` already says it; this guards against a refactor to String.
    let mut d = base();
    d["search_url"] = json!("https://k3y-in-the-url.invalid/{nope}");
    let e: &'static str = sources::parse_import(&serde_json::to_vec(&d).unwrap()).unwrap_err();
    assert!(!e.contains("k3y-in-the-url"));
    assert!(e.len() < 100);
}

/// Spellings of local hosts that URL parsers accept. A public source must not
/// be able to aim the app at any of them.
const LOCAL_SPELLINGS: &[&str] = &[
    "http://127.0.0.1/x.mp4",
    "http://127.0.0.2:8080/x.mp4",
    "http://2130706433/x.mp4",
    "http://0x7f000001/x.mp4",
    "http://0X7F.0.0.1/x.mp4",
    "http://0177.0.0.1/x.mp4",
    "http://127.1/x.mp4",
    "http://127.0.1/x.mp4",
    "http://0/x.mp4",
    "http://0.0.0.0/x.mp4",
    "http://[::1]/x.mp4",
    "http://[::]/x.mp4",
    "http://[::ffff:127.0.0.1]/x.mp4",
    "http://[::ffff:7f00:1]/x.mp4",
    "http://[::ffff:192.168.1.1]/x.mp4",
    "http://[fe80::1]/x.mp4",
    "http://[fd00::1]/x.mp4",
    "http://[64:ff9b::7f00:1]/x.mp4",
    "http://localhost/x.mp4",
    "http://localhost./x.mp4",
    "http://LOCALHOST:9/x.mp4",
    "http://a.localhost/x.mp4",
    "http://printer.local/x.mp4",
    "http://nas/x.mp4",
    "http://router.lan/x.mp4",
    "http://10.0.0.1/x.mp4",
    "http://172.16.5.4/x.mp4",
    "http://192.168.1.1:8080/x.mp4",
    "http://169.254.169.254/latest/meta-data",
    "http://100.64.0.1/x.mp4",
    "http://user:pw@127.0.0.1/x.mp4",
    "http://evil@127.0.0.1/x.mp4",
    "http://127.0.0.1\\@evil.invalid/x.mp4",
    "http://%31%32%37.0.0.1/x.mp4",
    "http://127\u{3002}0\u{3002}0\u{3002}1/x.mp4",
    "http://\u{ff11}\u{ff12}\u{ff17}.0.0.1/x.mp4",
];

const PUBLIC_SPELLINGS: &[&str] = &[
    "https://cdn.example.invalid/x.mp4",
    "http://93.184.216.34/x.mp4",
    "http://1.1.1.1:8443/x.mp4",
    "https://[2606:2800:220:1:248:1893:25c8:1946]/x.mp4",
    "https://evil.invalid\\@cdn.example.invalid/x.mp4",
];

fn media_allowed(d: &SourceDef, u: &str) -> bool {
    let body = serde_json::to_vec(&json!({"data":[{"name":"t","play":u}]})).unwrap();
    !sources::parse_items(d, &body, 10).unwrap().is_empty()
}

#[test]
fn a_public_source_cannot_aim_at_the_local_network() {
    let d = public_def();
    for u in LOCAL_SPELLINGS {
        assert!(
            !media_allowed(&d, u),
            "{u} was accepted from a public source"
        );
    }
    for u in PUBLIC_SPELLINGS {
        assert!(media_allowed(&d, u), "{u} was refused");
    }
}

#[test]
fn private_ranges_are_classified_as_such() {
    use std::net::IpAddr;
    let no = [
        "0.0.0.0",
        "0.1.2.3",
        "10.9.9.9",
        "100.127.255.255",
        "127.255.0.1",
        "169.254.0.1",
        "172.31.0.1",
        "192.0.0.1",
        "192.168.0.0",
        "198.18.0.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fc00::1",
        "fdff::1",
        "fe80::abcd",
        "fec0::1",
        "ff02::1",
        "::ffff:10.0.0.1",
        "::10.0.0.1",
        "2002:0a00:0001::",
        "2001:db8::1",
    ];
    for s in no {
        assert!(!sources::is_public_ip(s.parse::<IpAddr>().unwrap()), "{s}");
    }
    let yes = [
        "8.8.8.8",
        "1.1.1.1",
        "100.63.0.1",
        "172.32.0.1",
        "2606:4700::1111",
        "::ffff:8.8.8.8",
    ];
    for s in yes {
        assert!(sources::is_public_ip(s.parse::<IpAddr>().unwrap()), "{s}");
    }
}

// ---------------------------------------------------------------------------
// Static audit of the network entry points (no feature needed to read files).
// ---------------------------------------------------------------------------

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Production lines of a file (everything before the first `#[cfg(test)]`),
/// comments blanked, with 1-based line numbers.
fn prod_lines(p: &Path) -> Vec<(usize, String)> {
    let text = std::fs::read_to_string(p).unwrap();
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim() == "#[cfg(test)]" {
            break;
        }
        let code = l.split("//").next().unwrap_or("").to_owned();
        out.push((i + 1, code));
    }
    out
}

/// Every place that can open a socket must sit behind the kill switch:
/// `FetchService::builder()` fails when a proxy is required, a bare
/// `reqwest::Client::builder()` may exist only inside it, torrent sessions
/// check `require_proxy` first, and any file that sends requests either goes
/// through that builder or asks `proxy_blocked()`.
#[test]
fn every_network_entry_point_is_behind_the_kill_switch() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    let mut problems: Vec<String> = Vec::new();
    let (mut builders, mut sessions, mut senders) = (0, 0, 0);
    let rel = |p: &Path| p.strip_prefix(&src).unwrap().display().to_string();

    for f in &files {
        let name = rel(f);
        let lines = prod_lines(f);
        let whole: String = lines
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for (idx, (n, l)) in lines.iter().enumerate() {
            let at = format!("src/{name}:{n}");
            // Raw client construction.
            if l.contains("Client::builder(") || l.contains("ClientBuilder::new(") {
                builders += 1;
                let ok = name == "service/proxy.rs" && whole.contains("\"proxy required\"");
                if !ok {
                    problems.push(format!("{at}: raw client builder outside proxy.rs"));
                }
            }
            if l.contains("Client::new(")
                || l.contains("reqwest::get(")
                || l.contains("reqwest::blocking")
                || l.contains("UdpSocket::bind")
            {
                problems.push(format!("{at}: unguarded client or socket"));
            }
            if l.contains("TcpStream::connect") || l.contains("TcpListener::bind") {
                // Only the explicit loopback probe of a local Jackett/Prowlarr.
                // Or the user's click on "Test the proxy": a SOCKS5 greeting to
                // the proxy itself, nothing relayed (`probe_proxy`).
                let probe = name == "service/proxy.rs"
                    && l.contains("connect_timeout")
                    && whole.contains("pub fn probe_proxy");
                if !probe
                    && !(name == "detect.rs"
                        && (whole.contains("127.0.0.1") || whole.contains("[127, 0, 0, 1]")))
                {
                    problems.push(format!("{at}: raw TCP socket"));
                }
            }
            // The crate's builder: the `?` is what carries "proxy required".
            if l.contains("FetchService::builder()") || l.contains("Self::builder()") {
                builders += 1;
                if !l.contains("builder()?") {
                    problems.push(format!("{at}: builder() result not propagated with `?`"));
                }
            }
            // Torrent sessions.
            if l.contains("Session::new(") || l.contains("Session::new_with_opts(") {
                sessions += 1;
                let from = idx.saturating_sub(40);
                let window: String = lines[from..idx]
                    .iter()
                    .map(|(_, l)| l.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !(window.contains("require_proxy") && window.contains("proxy required")) {
                    problems.push(format!(
                        "{at}: torrent session without a require_proxy check"
                    ));
                }
            }
        }
        if whole.contains(".send()") {
            senders += 1;
            if !(whole.contains("proxy_blocked()") || whole.contains("builder()?")) {
                problems.push(format!(
                    "src/{name}:1: sends requests but neither builds through builder() nor asks proxy_blocked()"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "unguarded network entry points:\n{}",
        problems.join("\n")
    );
    // The audit must actually have found the entry points it protects.
    assert!(
        builders >= 4,
        "found only {builders} builders: patterns drifted"
    );
    assert!(sessions >= 1, "found no torrent session");
    assert!(senders >= 3, "found only {senders} request senders");
}

/// Long-lived loops (segment, block and key fetches) re-check the switch.
#[test]
fn streams_that_are_already_open_recheck_the_kill_switch() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for f in ["stream.rs", "segmented.rs"] {
        let text = std::fs::read_to_string(src.join(f)).unwrap();
        assert!(
            text.contains("proxy_blocked()"),
            "src/{f}:1: no proxy_blocked() in a module that fetches"
        );
    }
}

// ---------------------------------------------------------------------------
// Network feature: resolvers, secrets through real flows.
// ---------------------------------------------------------------------------

#[cfg(feature = "network")]
mod net {
    use super::*;
    use jizzcraft_core::Db;
    use jizzcraft_core::commands::{HasLibrary, Registry, register_core};
    use jizzcraft_fetch::commands::{FetchHost, HasFetch, register_fetch};
    use jizzcraft_fetch::config::{FetchConfig, Indexer};
    use jizzcraft_fetch::errors;
    use jizzcraft_fetch::resolver::{self, ResolverDef};
    use jizzcraft_fetch::secret::Secret;
    use jizzcraft_fetch::service::{FetchService, StreamOpts};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    static SERIAL: Mutex<()> = Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The expectations above are the table's claim; the URL parser the HTTP
    /// client really uses is the judge.
    #[test]
    fn the_spelling_table_matches_the_real_url_parser() {
        use reqwest::Url;
        for s in LOCAL_SPELLINGS {
            let Ok(u) = Url::parse(s) else { continue };
            let h = u.host_str().unwrap_or("").trim_matches(['[', ']', '.']);
            let local = match h.parse::<std::net::IpAddr>() {
                Ok(ip) => !sources::is_public_ip(ip),
                Err(_) => {
                    h.ends_with("localhost")
                        || h.ends_with(".local")
                        || !h.contains('.')
                        || h.ends_with(".lan")
                }
            };
            assert!(local, "{s} parses to a non-local host: {:?}", u.host_str());
        }
        for s in PUBLIC_SPELLINGS {
            let u = Url::parse(s).unwrap();
            if let Ok(ip) = u.host_str().unwrap_or("").parse::<std::net::Ipv4Addr>() {
                assert!(sources::is_public_ip(ip.into()), "{s}");
            }
        }
    }

    /// Differential: for thousands of IPv4 addresses in every legacy spelling,
    /// "is this address allowed from a public source" must agree with the
    /// address the real URL parser extracts.
    #[test]
    fn legacy_ipv4_spellings_agree_with_the_url_parser() {
        let d = public_def();
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut checked = 0;
        for i in 0..4000 {
            let v = next() as u32;
            // Bias towards the interesting ranges.
            let v = match i % 5 {
                0 => 0x7f00_0000 | (v & 0x00ff_ffff),
                1 => 0x0a00_0000 | (v & 0x00ff_ffff),
                2 => 0xc0a8_0000 | (v & 0xffff),
                3 => 0xa9fe_0000 | (v & 0xffff),
                _ => v,
            };
            let [a, b, c, e] = v.to_be_bytes();
            let spellings = [
                format!("{a}.{b}.{c}.{e}"),
                format!("{v}"),
                format!("0x{v:x}"),
                format!("0{:o}.{b}.{c}.{e}", a),
                format!("{a}.{}", v & 0x00ff_ffff),
                format!("{a}.{b}.{}", v & 0xffff),
                format!("0x{a:x}.0x{b:x}.0x{c:x}.0x{e:x}"),
            ];
            for s in spellings {
                let url = format!("http://{s}/x.mp4");
                let Ok(u) = reqwest::Url::parse(&url) else {
                    continue;
                };
                let Ok(ip) = u.host_str().unwrap_or("").parse::<std::net::Ipv4Addr>() else {
                    continue;
                };
                checked += 1;
                assert_eq!(
                    media_allowed(&d, &url),
                    sources::is_public_ip(ip.into()),
                    "{url} parses to {ip}"
                );
            }
        }
        assert!(checked > 10_000);
    }

    #[test]
    fn built_addresses_keep_their_host_for_every_search_term() {
        let d = public_def();
        for q in payloads() {
            let u = sources::build_url(&d, &q, 1, 10);
            let p = reqwest::Url::parse(&u).expect("a built address parses");
            assert_eq!(p.host_str(), Some("api.example.invalid"));
            assert_eq!(p.scheme(), "https");
            assert_eq!(p.path(), "/v1/s");
        }
    }

    #[test]
    fn resolver_output_is_filtered() {
        for p in payloads() {
            let doc = json!({"formats":[{
                "url": format!("https://a.invalid/{p}"),
                "protocol": p,
                "http_headers": {"User-Agent": p, "Referer": p, "Origin": p, "Cookie": p,
                                 "X-Evil": p, p.clone(): "x"},
                "vcodec": p, "acodec": "mp4a", "height": 720
            }, {"url": p}]});
            if let Ok(f) = resolver::parse_formats(&serde_json::to_vec(&doc).unwrap()) {
                for fmt in f {
                    assert!(fmt.url.starts_with("http"));
                    for (k, v) in fmt.headers {
                        assert!(
                            ["user-agent", "referer", "origin"]
                                .contains(&k.to_ascii_lowercase().as_str())
                        );
                        assert!(v.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b)));
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_resolver_programs_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let chmod = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        };
        chmod(d.path(), 0o700);
        let prog = d.path().join("tool");
        std::fs::write(&prog, "#!/bin/sh\necho '{}'\n").unwrap();
        chmod(&prog, 0o700);
        let def = |p: &Path| ResolverDef {
            id: 0,
            name: "t".into(),
            argv: vec![p.to_str().unwrap().into(), "{url}".into()],
            timeout_secs: 5,
            enabled: true,
            honours_proxy: false,
        };
        assert!(resolver::validate(&def(&prog)).is_ok());
        chmod(&prog, 0o702);
        assert!(resolver::validate(&def(&prog)).is_err());
        // `run` refuses too: a file swapped after it was registered.
        assert!(resolver::run(&def(&prog), "https://a.invalid/").is_err());
        chmod(&prog, 0o700);
        chmod(d.path(), 0o777);
        assert!(resolver::validate(&def(&prog)).is_err());
        chmod(d.path(), 0o700);
    }

    // ----- secrets -----

    const SRC: &str = "SEEDsrc-9f1e77a0";
    const IDX: &str = "SEEDidx-41ac02be";
    const PX: &str = "SEEDpx-c3d9b8f1";
    const ARG: &str = "SEEDarg-77d2e105";

    struct Ctx {
        db: Db,
        fetch: FetchHost,
    }
    impl HasLibrary for Ctx {
        fn db(&mut self) -> &mut Db {
            &mut self.db
        }
    }
    impl HasFetch for Ctx {
        fn fetch(&mut self) -> &mut FetchHost {
            &mut self.fetch
        }
    }

    /// Replies with the request it got, as an error page, whatever it was.
    fn echo_server() -> (u16, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut b = [0u8; 4096];
                let n = s.read(&mut b).unwrap_or(0);
                let req = String::from_utf8_lossy(&b[..n]).into_owned();
                s2.lock().unwrap().push(req.clone());
                let body = format!("denied for {req}");
                let st = if req.contains("/redir") {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid/?k={SRC}{IDX}\r\n"
                    )
                } else if req.contains("/auth") {
                    "HTTP/1.1 401 Unauthorized\r\n".to_owned()
                } else {
                    "HTTP/1.1 500 Oops\r\n".to_owned()
                };
                let _ = write!(
                    s,
                    "{st}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (port, seen)
    }

    fn closed_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    fn assert_clean(label: &str, s: &str) {
        for seed in [SRC, IDX, PX, ARG] {
            assert!(!s.contains(seed), "{label} leaks {seed}: {s}");
        }
    }

    fn drive(port: u16, proxy: Option<String>, host_name: &str, out: &mut Vec<(String, String)>) {
        let dl = tempfile::tempdir().unwrap();
        let st = tempfile::tempdir().unwrap();
        let mut push = |l: &str, s: String| out.push((l.to_owned(), s));
        let mk_ix = |path: &str| Indexer {
            id: 1,
            name: "ix".into(),
            url: format!("http://{host_name}:{port}/{path}"),
            api_key: Secret::new(IDX),
            enabled: true,
        };
        for path in ["auth", "redir", "err"] {
            // A key never travels in clear text: only the loopback echo is http.
            let scheme = if host_name == "127.0.0.1" {
                "http"
            } else {
                "https"
            };
            let mut d = sources::parse_import(
                format!(
                    r#"{{"name":"src","search_url":"{scheme}://{host_name}:{port}/{path}?q={{query}}",
                       "items":"/data","fields":{{"title":"/name","media":"/play"}},
                       "auth_header":"X-Key","auth_prefix":"Bearer "}}"#
                )
                .as_bytes(),
            )
            .unwrap();
            d.id = 1;
            d.per_minute = 0;
            d.secret = Secret::new(SRC);
            let cfg = FetchConfig {
                indexers: vec![mk_ix(path)],
                sources: vec![d],
                proxy: proxy.clone().map(Secret::new),
                download_dir: Some(dl.path().into()),
                dht: false,
                ..Default::default()
            };
            push("config debug", format!("{cfg:?}"));
            let svc = FetchService::new(cfg, st.path().into()).unwrap();
            let rx = svc.search(
                None,
                "query words",
                &[],
                5,
                Arc::new(AtomicBool::new(false)),
            );
            let o = rx.recv_timeout(Duration::from_secs(40)).unwrap();
            push("search outcome", format!("{o:?}"));
            let e = svc
                .test_indexer(&mk_ix(path))
                .recv_timeout(Duration::from_secs(40))
                .unwrap();
            if let Err(e) = e {
                push("test_indexer raw", e.clone());
                push("test_indexer classified", errors::classify(&e).to_owned());
            }
            let url = format!("http://{host_name}:{port}/{path}/v.mp4?token={SRC}");
            for (l, r) in [
                ("stream_url", svc.stream_url(url.clone())),
                ("play_http", svc.play_http(url.clone())),
                ("open_any", svc.open_any(url.clone(), StreamOpts::default())),
            ] {
                if let Err(e) = r.recv_timeout(Duration::from_secs(40)).unwrap() {
                    push(&format!("{l} raw"), e.clone());
                    push(&format!("{l} classified"), errors::classify(&e).to_owned());
                }
            }
        }
    }

    #[test]
    fn secrets_never_reach_errors_debug_or_listings() {
        let _g = serial();
        let (port, seen) = echo_server();
        let mut produced: Vec<(String, String)> = Vec::new();

        // Direct, against a server that echoes everything it receives.
        drive(port, None, "127.0.0.1", &mut produced);
        // Through a proxy that does not answer, with a password in its URL.
        let dead = closed_port();
        drive(
            port,
            Some(format!("socks5://user:{PX}@127.0.0.1:{dead}")),
            "target.invalid",
            &mut produced,
        );
        // Sanity: the secrets did travel where they should (the echo server saw
        // the source key and the indexer key), so the scan below is meaningful.
        {
            let s = seen.lock().unwrap().join("\n");
            assert!(s.contains(SRC), "the source key never reached its server");
            assert!(s.contains(IDX), "the indexer key never reached its server");
        }
        assert!(produced.len() > 20);

        // Error classification is total and fixed.
        for raw in [
            format!("error sending request for url (http://h.invalid/?apikey={IDX})"),
            format!("socks connect error: user:{PX}@127.0.0.1"),
            format!("HTTP 401 {SRC}"),
            format!("{ARG} resolver failed"),
            format!("No such file: /home/u/{SRC}.mp4"),
        ] {
            let c = errors::classify(&raw);
            assert!(errors::ALL.contains(&c));
            produced.push(("classify".into(), c.to_owned()));
            produced.push((
                "redact".into(),
                jizzcraft_fetch::redact::redact_url(&format!(
                    "https://u:{PX}@h.invalid/p?k={SRC}#{IDX}"
                )),
            ));
        }
        let r = ResolverDef {
            id: 1,
            name: "r".into(),
            argv: vec![
                "/usr/bin/tool".into(),
                format!("--key={ARG}"),
                "{url}".into(),
            ],
            timeout_secs: 5,
            enabled: true,
            honours_proxy: false,
        };
        produced.push(("resolver debug".into(), format!("{r:?} {:#?}", r.clone())));
        produced.push((
            "secret debug".into(),
            format!(
                "{:?} {} {:?}",
                Secret::new(SRC),
                Secret::new(SRC),
                Some(Secret::new(IDX))
            ),
        ));

        // The command layer: listings, settings and failing calls.
        let d = tempfile::tempdir().unwrap();
        let mut reg = Registry::<Ctx>::new();
        register_core(&mut reg);
        register_fetch(&mut reg);
        let mut c = Ctx {
            db: Db::open_in_memory().unwrap(),
            fetch: FetchHost::new(d.path().join("fetch.json")),
        };
        let dead2 = closed_port();
        let calls = [
            (
                "fetch.indexers.add",
                json!({"name":"n","url":format!("http://127.0.0.1:{dead2}/t?apikey={IDX}"),"api_key":IDX}),
            ),
            (
                "fetch.settings",
                json!({"proxy": format!("socks5://u:{PX}@127.0.0.1:{dead2}")}),
            ),
            (
                "fetch.settings",
                json!({"proxy": format!("http://u:{PX}@127.0.0.1")}),
            ),
            ("fetch.indexers.list", json!({})),
            ("fetch.indexers.test", json!({"id": 1})),
            ("fetch.search", json!({"query": format!("{SRC} {IDX}")})),
            (
                "fetch.add",
                json!({"magnet": format!("magnet:?xt=urn:btih:zz&dn={SRC}")}),
            ),
        ];
        for (name, args) in calls {
            match reg.run(&mut c, name, args) {
                Ok(v) => produced.push((name.to_owned(), v.to_string())),
                Err(e) => {
                    produced.push((format!("{name} err"), e.to_string()));
                    produced.push((format!("{name} err debug"), format!("{e:?}")));
                }
            }
        }
        let host = &mut c.fetch;
        let sid = host
            .import_source(
                format!(
                    r#"{{"name":"s","search_url":"https://api.example.invalid/s?q={{query}}",
                        "items":"/d","fields":{{"title":"/n","media":"/m"}},
                        "auth_header":"X-Key","secret":"{SRC}"}}"#
                )
                .as_bytes(),
            )
            .unwrap();
        host.set_source_secret(sid, SRC.to_owned()).unwrap();
        // What `sources.list` exposes: id, name, enabled, has_secret.
        let listing: Vec<Value> = host
            .config()
            .sources
            .iter()
            .map(|s| json!({"id": s.id, "name": s.name, "enabled": s.enabled, "has_secret": !s.secret.is_empty()}))
            .collect();
        assert_eq!(listing[0]["has_secret"], true);
        produced.push(("sources.list".into(), json!(listing).to_string()));
        produced.push(("host config debug".into(), format!("{:?}", host.config())));

        for (label, s) in &produced {
            assert_clean(label, s);
        }

        // The only place a secret may live: the 0600 config file.
        let path = d.path().join("fetch.json");
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(file.contains(SRC) && file.contains(IDX));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // An import never takes a secret from the file.
        let again = FetchHost::new(path);
        assert!(
            again
                .config()
                .sources
                .iter()
                .all(|s| s.secret.expose() == SRC)
        );
    }

    #[test]
    fn a_secret_with_a_line_break_is_not_sent() {
        let _g = serial();
        let (port, seen) = echo_server();
        let dl = tempfile::tempdir().unwrap();
        let st = tempfile::tempdir().unwrap();
        let mut d = sources::parse_import(
            format!(
                r#"{{"name":"src","search_url":"http://127.0.0.1:{port}/err?q={{query}}",
                   "items":"/data","fields":{{"title":"/name","media":"/play"}},
                   "auth_header":"X-Key"}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        d.id = 1;
        d.per_minute = 0;
        d.secret = Secret::new(format!("{SRC}\r\nX-Injected: 1"));
        let svc = FetchService::new(
            FetchConfig {
                sources: vec![d],
                download_dir: Some(dl.path().into()),
                dht: false,
                ..Default::default()
            },
            st.path().into(),
        )
        .unwrap();
        let o = svc
            .search(None, "q", &[], 5, Arc::new(AtomicBool::new(false)))
            .recv_timeout(Duration::from_secs(20))
            .unwrap();
        assert_eq!(o.failed, 1);
        assert!(!seen.lock().unwrap().join("").contains("X-Injected"));
        assert_clean("outcome", &format!("{o:?}"));
    }
}
