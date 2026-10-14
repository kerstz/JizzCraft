//! `jizzcraft-browser`: optional helper process, started only by JizzCraft.
//! It speaks the bridge protocol on stdin/stdout. Started from a terminal, or
//! without a Hello inside two seconds, it refuses, so the 18+ gate in the app
//! stays in front of it.

#[cfg(feature = "engine")]
mod engine;
mod policy;
#[cfg(feature = "engine")]
mod selftest;

use std::io::{BufReader, IsTerminal, Write};
use std::sync::mpsc;
use std::time::Duration;

use jizzcraft_bridge::{FromHelper, ReadError, ToHelper, read_message, write_message};
use policy::*;

const HELLO_TIMEOUT: Duration = Duration::from_millis(2000);

fn refuse() -> ! {
    eprintln!("{NOT_FROM_APP_TEXT}");
    std::process::exit(EXIT_NOT_FROM_APP);
}

fn send(msg: &FromHelper) {
    let mut out = std::io::stdout().lock();
    let _ = write_message(&mut out, msg);
    let _ = out.flush();
}

fn fail(code: &str, exit: i32) -> ! {
    send(&FromHelper::Error {
        code: code.to_owned(),
    });
    std::process::exit(exit);
}

fn main() {
    #[cfg(feature = "engine")]
    if let Some(code) = self_test_mode() {
        std::process::exit(code);
    }
    let timeout = std::env::var("JIZZCRAFT_BROWSER_HELLO_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .filter(|d| *d <= HELLO_TIMEOUT)
        .unwrap_or(HELLO_TIMEOUT);
    if std::io::stdin().is_terminal() || std::io::stdout().is_terminal() {
        refuse();
    }
    // The reader thread owns stdin for the whole session.
    let (tx, rx) = mpsc::channel::<Result<ToHelper, ReadError>>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(std::io::stdin().lock());
        loop {
            let m = read_message::<_, ToHelper>(&mut r);
            let stop = matches!(m, Err(ReadError::Eof) | Err(ReadError::Io(_)));
            if tx.send(m).is_err() || stop {
                break;
            }
        }
    });
    let first = match rx.recv_timeout(timeout) {
        Ok(Ok(m)) => m,
        _ => refuse(),
    };
    let session = match validate_hello(first) {
        Ok(s) => s,
        Err(e) => fail(e.code(), e.exit_code()),
    };
    run(session, rx)
}

#[cfg(not(feature = "engine"))]
fn run(_s: Session, _rx: mpsc::Receiver<Result<ToHelper, ReadError>>) -> ! {
    fail("engine_missing", EXIT_ENGINE_MISSING)
}

#[cfg(feature = "engine")]
fn run(s: Session, rx: mpsc::Receiver<Result<ToHelper, ReadError>>) -> ! {
    let code = engine::run(s, rx, send, None);
    std::process::exit(code)
}

/// Hidden `--self-test`: only with JIZZCRAFT_BROWSER_SELFTEST=1. No Hello and
/// no stdin. `JIZZCRAFT_BROWSER_SELFTEST_URL` loads one URL instead of running
/// the suite; `JIZZCRAFT_BROWSER_SELFTEST_PROXY` sets the Hello proxy.
#[cfg(feature = "engine")]
fn self_test_mode() -> Option<i32> {
    if std::env::var("JIZZCRAFT_BROWSER_SELFTEST").as_deref() != Ok("1")
        || !std::env::args().any(|a| a == "--self-test")
    {
        return None;
    }
    let url = std::env::var("JIZZCRAFT_BROWSER_SELFTEST_URL").ok();
    let hello = ToHelper::Hello {
        v: HELLO_VERSION,
        proxy: std::env::var("JIZZCRAFT_BROWSER_SELFTEST_PROXY").ok(),
        require_proxy: false,
        js: true,
        media: true,
        remember: false,
        discreet: false,
        disguised: true,
        rules_path: None,
        search_template: None,
        software: false,
    };
    let session = match validate_hello(hello) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("self-test: {}", e.code());
            return Some(e.exit_code());
        }
    };
    let (_tx, rx) = mpsc::channel();
    let st = match url {
        Some(u) => engine::SelfTest::Load(u),
        None => engine::SelfTest::Suite,
    };
    Some(engine::run(session, rx, send, Some(st)))
}
