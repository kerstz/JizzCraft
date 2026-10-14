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
    "openssl-sys",
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
