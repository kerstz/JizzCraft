//! `cargo xtask e2e [--allow-skips] [TEST_FILTER...]`: the end-to-end
//! scenarios of `tests/e2e` (loopback servers and swarms, the real player).
//! See `docs/e2e.md`.

use std::process::Command;

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut allow_skips = false;
    let mut filters = Vec::new();
    for a in args {
        match a.as_str() {
            "--allow-skips" => allow_skips = true,
            f if f.starts_with('-') => return Err(format!("unknown option {f}")),
            f => filters.push(f.to_owned()),
        }
    }
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args([
        "test",
        "-p",
        "jizzcraft-e2e",
        "--features",
        "network",
        "--",
        // The proxy policy and the GStreamer registry are process-global.
        "--test-threads=1",
    ]);
    cmd.args(&filters);
    if allow_skips {
        cmd.env_remove("JIZZCRAFT_REQUIRE_PLUGINS");
    } else {
        // A scenario that skips for a missing plugin proves nothing.
        cmd.env("JIZZCRAFT_REQUIRE_PLUGINS", "1");
    }
    let st = cmd.status().map_err(|e| format!("cargo: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err("e2e scenarios failed".into())
    }
}
