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
#[test]
fn network_build_links_librqbit() {
    let names = tree(&["-p", "jizzcraft", "--features", "network"]);
    assert!(names.iter().any(|n| n == "librqbit"));
}
