//! Runs the helper's hidden `--self-test` against real WebKitGTK. Needs a
//! display, so it is ignored unless `JIZZCRAFT_BROWSER_SELFTEST=1`.

#![cfg(feature = "engine")]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs a display; set JIZZCRAFT_BROWSER_SELFTEST=1 and run with --ignored"]
fn engine_policies_hold() {
    if std::env::var("JIZZCRAFT_BROWSER_SELFTEST").as_deref() != Ok("1") {
        eprintln!("JIZZCRAFT_BROWSER_SELFTEST!=1: skipped");
        return;
    }
    let mut c = Command::new(env!("CARGO_BIN_EXE_jizzcraft-browser"))
        .arg("--self-test")
        .env("JIZZCRAFT_BROWSER_SELFTEST", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn helper");
    let start = Instant::now();
    let status = loop {
        if let Some(s) = c.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = c.kill();
            let _ = c.wait();
            panic!("self-test timed out after 30 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    println!("{out}");
    let lines: Vec<&str> = out.lines().filter(|l| l.contains("\"check\"")).collect();
    assert!(lines.len() >= 10, "too few checks: {out}");
    for l in &lines {
        assert!(l.contains("\"blocked\":true"), "policy failed: {l}");
    }
    assert!(status.success(), "exit {status:?}");
}
