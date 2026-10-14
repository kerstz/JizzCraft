//! The offline promise, as a test: in the default build no HTTP, TLS, async
//! runtime or BitTorrent crate may appear in the resolved dependency tree.
//!
//! The lock file lists optional dependencies even when their feature is off,
//! so this asks `cargo tree`, which resolves features like a real build.

use std::process::Command;

const BANNED: &[&str] = &[
    "reqwest",
    "hyper",
    "ureq",
    "curl",
    "curl-sys",
    "isahc",
    "attohttpc",
    "surf",
    "native-tls",
    "rustls",
    "openssl",
    // `openssl-sys` is NOT listed: SQLCipher (rusqlite feature
    // `bundled-sqlcipher-vendored-openssl`) statically links a vendored
    // OpenSSL libcrypto for its cipher. `only_sqlcipher_pulls_openssl_sys`
    // pins who may depend on it; `openssl` and `native-tls` stay banned.
    "h2",
    "tungstenite",
    "webbrowser",
    "librqbit",
    "tokio",
    "quinn",
    "axum",
    // Browser engines live in the separate helper process only.
    "webkit6",
    "webkit2gtk",
    "wry",
    "gtk4",
    "gtk",
];

fn tree(args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--offline",
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .args(args)
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .collect()
}

#[test]
fn default_build_has_no_network_crates() {
    let names = tree(&["--workspace"]);
    let found: Vec<&&str> = BANNED
        .iter()
        .filter(|b| names.iter().any(|n| n == **b))
        .collect();
    assert!(
        found.is_empty(),
        "network crates in the default build: {found:?}"
    );
}

#[test]
fn default_apps_do_not_link_fetch() {
    for pkg in ["jizzcraft", "jizzcraft-cli"] {
        let names = tree(&["-p", pkg]);
        assert!(
            !names.iter().any(|n| n == "jizzcraft-fetch"),
            "{pkg} links jizzcraft-fetch in the default build"
        );
    }
}

/// Positive control: proves the check above can actually see network crates.
/// A smoke detector you never tested is a ceiling decoration.
#[test]
fn network_build_links_librqbit() {
    let names = tree(&["-p", "jizzcraft", "--features", "network"]);
    assert!(names.iter().any(|n| n == "librqbit"));
}

/// The browser helper may use gtk4/webkit6 behind its `engine` feature, but no
/// app or library crate may depend on it or on those crates, in any feature set.
#[test]
fn browser_engine_stays_in_the_helper() {
    let banned = [
        "jizzcraft-browser",
        "gtk4",
        "webkit6",
        "webkit2gtk",
        "wry",
        "gtk",
    ];
    for (pkg, extra) in [
        ("jizzcraft", vec![]),
        ("jizzcraft", vec!["--features", "network"]),
        ("jizzcraft-cli", vec![]),
        ("jizzcraft-ui", vec![]),
        ("jizzcraft-fetch", vec![]),
        ("jizzcraft-media", vec![]),
        ("jizzcraft-core", vec![]),
    ] {
        let mut args = vec!["-p", pkg];
        args.extend(extra);
        let names = tree(&args);
        for b in banned {
            assert!(!names.iter().any(|n| n == b), "{pkg} depends on {b}");
        }
    }
}

/// `openssl-sys` is allowed for one reason only: SQLCipher's libcrypto. The
/// only direct dependent may be `libsqlite3-sys`; a TLS stack would show up
/// as `openssl`, `native-tls` or another dependent and fail here (and in
/// `BANNED`).
#[test]
fn only_sqlcipher_pulls_openssl_sys() {
    for extra in [vec![], vec!["--features", "network"]] {
        let mut args = vec!["--workspace", "-i", "openssl-sys", "--depth", "1"];
        args.extend(extra);
        let names = tree(&args);
        assert_eq!(names.first().map(String::as_str), Some("openssl-sys"));
        let dependents: Vec<&String> = names.iter().skip(1).collect();
        assert_eq!(
            dependents,
            ["libsqlite3-sys"],
            "unexpected dependents of openssl-sys"
        );
    }
}

/// Positive control for the SQLCipher build: a plain SQLite would return no
/// row for `PRAGMA cipher_version`.
#[test]
fn sqlcipher_is_linked() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let v: String = conn
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .expect("PRAGMA cipher_version must return a row");
    assert!(!v.trim().is_empty(), "empty cipher_version");
    let fts: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_compile_options WHERE compile_options = 'ENABLE_FTS5'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts, 1, "FTS5 missing from the SQLCipher build");
}
