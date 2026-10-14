//! The offline promise, as a test: no HTTP, TLS or browser-launching crate may
//! appear in the dependency lock. `tokio` is allowed: on Linux it is only
//! pulled in for the wasm target.

#[test]
fn lockfile_has_no_network_crates() {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("Cargo.lock");
    let banned = [
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
    ];
    let names: Vec<&str> = lock
        .lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .filter_map(|l| l.strip_suffix('"'))
        .collect();
    let found: Vec<&&str> = banned.iter().filter(|b| names.contains(b)).collect();
    assert!(found.is_empty(), "network crates in Cargo.lock: {found:?}");
}
