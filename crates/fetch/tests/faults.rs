//! Network faults against the Range reader and the plain-HTTP fallback, on a
//! loopback server that misbehaves on purpose. Synthetic bytes only.
#![cfg(feature = "network")]

#[path = "support/fault_server.rs"]
mod fault_server;

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use fault_server::{Cfg, mp4_moov_last, pattern, start};
use jizzcraft_fetch::errors::{self, classify};
use jizzcraft_fetch::service::{FetchService, StreamTicket};
use jizzcraft_fetch::stream::{self, ReopenFn};
use jizzcraft_fetch::{FetchConfig, NeedsFullDownloadProgress};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

struct Rig {
    rt: tokio::runtime::Runtime,
}

impl Rig {
    fn new() -> Self {
        Self {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        }
    }

    fn open(&self, url: String, reopen: Option<ReopenFn>) -> Result<stream::Opened, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(stream::CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap();
        let h = self.rt.handle().clone();
        self.rt.block_on(stream::open_with(h, client, url, reopen))
    }
}

/// Read `n` bytes, waiting through `WouldBlock`; any other error is returned.
fn read_n(r: &mut impl Read, n: usize, patience: Duration) -> Result<Vec<u8>, std::io::Error> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    while out.len() < n {
        let want = (n - out.len()).min(buf.len());
        match r.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(k) => out.extend_from_slice(&buf[..k]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        assert!(t0.elapsed() < patience, "stalled");
    }
    Ok(out)
}

fn svc(dl: &std::path::Path, st: &std::path::Path) -> FetchService {
    FetchService::new(
        FetchConfig {
            download_dir: Some(dl.into()),
            dht: false,
            ..Default::default()
        },
        st.into(),
    )
    .unwrap()
}

#[test]
fn slow_but_moving_body_does_not_time_out() {
    let _g = serial();
    // 64 kbit/s: 280 kB takes about 35 s, longer than the old 30 s total.
    let data = pattern(280_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.throttle_bps = Some(8000);
    let srv = start(cfg);
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/v/slow.mp4"), None).expect("open");
    let got = read_n(&mut o.reader, data.len(), Duration::from_secs(90)).expect("read");
    assert_eq!(got, data);
}

#[test]
fn mid_body_drop_is_retried_not_sticky() {
    let _g = serial();
    let data = pattern(1_500_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.drop_idx = Some((1, 3));
    let srv = start(cfg);
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/v/drop.mp4"), None).expect("open");
    let got = read_n(&mut o.reader, data.len(), Duration::from_secs(40)).expect("read");
    assert_eq!(got, data);
    assert!(srv.hits.load(Ordering::SeqCst) >= 4);
}

#[test]
fn busy_server_with_retry_after_is_waited_for() {
    let _g = serial();
    let data = pattern(600_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.busy_idx = Some((1, 2));
    let srv = start(cfg);
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/v/busy.mp4"), None).expect("open");
    let t0 = Instant::now();
    let got = read_n(&mut o.reader, data.len(), Duration::from_secs(30)).expect("read");
    assert_eq!(got, data);
    assert!(
        t0.elapsed() >= Duration::from_millis(900),
        "Retry-After ignored"
    );
}

#[test]
fn etag_flip_is_a_source_change() {
    let _g = serial();
    let data = pattern(900_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.flip_etag_from = Some(1);
    let srv = start(cfg);
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/v/flip.mp4"), None).expect("open");
    o.reader.seek(SeekFrom::Start(700_000)).unwrap();
    let e = read_n(&mut o.reader, 1000, Duration::from_secs(20)).expect_err("must fail");
    assert_eq!(classify(&e.to_string()), errors::SOURCE_CHANGED);
}

#[test]
fn forbidden_mid_stream_without_reopen_is_expired() {
    let _g = serial();
    let mut cfg = Cfg::new(pattern(900_000));
    cfg.forbid = Some(("/old".into(), 1));
    let srv = start(cfg);
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/old/a.mp4"), None).expect("open");
    o.reader.seek(SeekFrom::Start(700_000)).unwrap();
    let e = read_n(&mut o.reader, 1000, Duration::from_secs(20)).expect_err("must fail");
    assert_eq!(classify(&e.to_string()), errors::EXPIRED);
}

#[test]
fn expired_url_is_renewed_once_by_the_hook() {
    let _g = serial();
    let data = pattern(900_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.forbid = Some(("/old".into(), 1));
    let srv = start(cfg);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (c2, new_url) = (calls.clone(), srv.url("/new/a.mp4"));
    let hook: ReopenFn = Arc::new(move || {
        c2.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(Ok(new_url.clone()));
        rx
    });
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/old/a.mp4"), Some(hook)).expect("open");
    o.reader.seek(SeekFrom::Start(300_000)).unwrap();
    let got = read_n(&mut o.reader, 500_000, Duration::from_secs(20)).expect("read");
    assert_eq!(got, data[300_000..800_000]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cross_host_redirect_is_refused() {
    let _g = serial();
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = svc(dl.path(), st.path());
    let mut cfg = Cfg::new(pattern(1000));
    // Same machine, other host name: another origin.
    cfg.redirect_to = Some("http://localhost:9/elsewhere.mp4".into());
    let srv = start(cfg);
    let e = svc
        .stream_url(srv.url("/v/redir.mp4"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .err()
        .expect("refused");
    assert_eq!(classify(&e), errors::UNREACHABLE, "{e}");
    assert_eq!(srv.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn no_range_server_with_index_at_end_says_needs_full_download() {
    let _g = serial();
    let data = mp4_moov_last(400_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.ranges = false;
    cfg.throttle_bps = Some(100_000);
    let srv = start(cfg);
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = svc(dl.path(), st.path());
    let t = svc
        .stream_url(srv.url("/v/tail-index.mp4"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .expect("ticket");
    let StreamTicket::Live { mut reader, .. } = t else {
        panic!("live");
    };
    reader.seek(SeekFrom::End(-64)).unwrap();
    let e = read_n(&mut reader, 10, Duration::from_secs(10)).expect_err("typed error");
    let p = NeedsFullDownloadProgress::from_io(&e).expect("progress");
    assert!(p.written < p.total);
    assert_eq!(classify(&e.to_string()), errors::NEEDS_FULL_DOWNLOAD);
    drop(reader);
}

#[test]
fn failed_fallback_download_leaves_no_part_file() {
    let _g = serial();
    let data = pattern(600_000);
    let mut cfg = Cfg::new(data);
    cfg.ranges = false;
    cfg.drop_idx = Some((1, 2));
    let srv = start(cfg);
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = svc(dl.path(), st.path());
    let t = svc
        .stream_url(srv.url("/v/cut.mp4"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .expect("ticket");
    let StreamTicket::Live { mut reader, .. } = t else {
        panic!("live");
    };
    let r = read_n(&mut reader, 600_000, Duration::from_secs(20));
    assert!(r.is_err(), "a cut body must end in an error");
    drop(reader);
    let t0 = Instant::now();
    while std::fs::read_dir(dl.path()).unwrap().count() != 0 {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "part file left behind"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn seeking_away_abandons_requests_nobody_needs() {
    let _g = serial();
    let data = pattern(8_000_000);
    let mut cfg = Cfg::new(data.clone());
    cfg.throttle_bps = Some(400_000);
    let srv = start(cfg);
    let rig = Rig::new();
    let mut o = rig.open(srv.url("/v/seek.mp4"), None).expect("open");
    // Reading the start queues read-ahead requests.
    read_n(&mut o.reader, 1000, Duration::from_secs(20)).unwrap();
    let t0 = Instant::now();
    while srv.active.load(Ordering::SeqCst) < 3 && t0.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(srv.active.load(Ordering::SeqCst) >= 3, "no read-ahead");
    o.reader.seek(SeekFrom::End(-5000)).unwrap();
    let got = read_n(&mut o.reader, 5000, Duration::from_secs(30)).unwrap();
    assert_eq!(got, data[data.len() - 5000..]);
    let t0 = Instant::now();
    while srv.active.load(Ordering::SeqCst) > 0 {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "abandoned requests still running"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
