//! Runs the compiled helper. Skipped when built with the real engine, where
//! the reply to a valid Hello is Ready, not engine_missing.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn spawn() -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_jizzcraft-browser"))
        .env("JIZZCRAFT_BROWSER_HELLO_MS", "400")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait(mut c: std::process::Child) -> (Option<i32>, Duration) {
    let t = Instant::now();
    loop {
        if let Some(s) = c.try_wait().unwrap() {
            return (s.code(), t.elapsed());
        }
        assert!(t.elapsed() < Duration::from_secs(5), "helper hung");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(feature = "engine"))]
#[test]
fn valid_hello_gets_engine_missing() {
    let mut c = spawn();
    c.stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"t\":\"hello\",\"v\":1}\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(c.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        line.contains("\"error\"") && line.contains("engine_missing"),
        "{line}"
    );
    assert_eq!(wait(c).0, Some(3));
}

#[test]
fn no_hello_exits_2() {
    let mut c = spawn();
    let _keep_open = c.stdin.take(); // pipe stays open: only the timeout can end it
    let (code, took) = wait(c);
    assert_eq!(code, Some(2));
    assert!(took < Duration::from_secs(3));
}

#[test]
fn wrong_first_message_exits_2_or_5() {
    let mut c = spawn();
    c.stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"t\":\"close\"}\n")
        .unwrap();
    let (code, _) = wait(c);
    assert_eq!(code, Some(5));
}

#[test]
fn proxy_required_without_proxy() {
    let mut c = spawn();
    c.stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"t\":\"hello\",\"v\":1,\"require_proxy\":true}\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(c.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("proxy_required"), "{line}");
    assert_eq!(wait(c).0, Some(4));
}
