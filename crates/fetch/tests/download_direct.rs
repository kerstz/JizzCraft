//! The plain-HTTP saver against a loopback server that misbehaves on
//! purpose. Synthetic bytes only; nothing leaves the machine.
#![cfg(feature = "network")]

#[path = "support/fault_server.rs"]
mod fault_server;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fault_server::{Cfg, Route, mp4_moov_last, start};
use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::download::direct::{save_direct, set_retry_base};
use jizzcraft_fetch::download::{FreeSpace, JobCtl, RateLimiter, Renew, real_free_space};
use jizzcraft_fetch::errors::{self, classify};
use jizzcraft_fetch::service::FetchService;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    reset_policy();
    set_retry_base(Some(Duration::from_millis(15)));
    g
}

/// Publish a default policy (kill switch off) for the process.
fn reset_policy() {
    let d = tempfile::tempdir().unwrap();
    let _ = FetchService::new(
        FetchConfig {
            dht: false,
            ..Default::default()
        },
        d.path().into(),
    )
    .unwrap();
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

struct Run {
    ctl: Arc<JobCtl>,
    limiter: Arc<RateLimiter>,
    free: FreeSpace,
}

impl Run {
    fn new() -> Self {
        Self {
            ctl: JobCtl::new(),
            limiter: Arc::new(RateLimiter::new(None)),
            free: real_free_space(),
        }
    }

    fn go(&self, rt: &tokio::runtime::Runtime, url: &str, dir: &Path) -> Result<PathBuf, String> {
        rt.block_on(self.save(url.to_owned(), dir.to_owned(), None))
    }

    async fn save(
        &self,
        url: String,
        dir: PathBuf,
        renew: Option<Renew>,
    ) -> Result<PathBuf, String> {
        save_direct(
            &url,
            &[],
            &dir,
            "Title",
            &self.ctl,
            &self.limiter,
            &self.free,
            renew.as_ref(),
        )
        .await
    }

    /// Start in the background.
    fn spawn(
        self: &Arc<Self>,
        rt: &tokio::runtime::Runtime,
        url: String,
        dir: PathBuf,
    ) -> tokio::task::JoinHandle<Result<PathBuf, String>> {
        let me = self.clone();
        rt.spawn(async move { me.save(url, dir, None).await })
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ranged_file_resumes_after_drop_byte_identical() {
    let _g = serial();
    let body = mp4_moov_last(300_000);
    let mut cfg = Cfg::new(body.clone());
    cfg.drop_idx = Some((0, 1));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let out = Run::new().go(&rt, &srv.url("/v.mp4"), dir.path()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), body);
    assert_eq!(files(dir.path()), ["Title.mp4"]);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 2);
}

#[test]
fn pause_then_resume_continues_from_offset() {
    let _g = serial();
    let body = mp4_moov_last(400_000);
    let mut cfg = Cfg::new(body.clone());
    cfg.throttle_bps = Some(250_000);
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let run = Arc::new(Run::new());
    let h = run.spawn(&rt, srv.url("/v.mp4"), dir.path().to_owned());
    wait_for("some bytes", || {
        run.ctl.done.load(Ordering::SeqCst) > 20_000
    });
    run.ctl.pause.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    let at = run.ctl.done.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(run.ctl.done.load(Ordering::SeqCst), at, "paused is paused");
    assert!(at < body.len() as u64);
    assert!(files(dir.path()).iter().any(|f| f.ends_with(".part")));
    run.ctl.resume();
    let out = rt.block_on(h).unwrap().unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), body);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 2);
    assert_eq!(files(dir.path()), ["Title.mp4"]);
}

#[test]
fn unranged_server_restarts_from_zero_on_resume() {
    let _g = serial();
    let body = mp4_moov_last(200_000);
    let mut cfg = Cfg::new(body.clone());
    cfg.ranges = false;
    cfg.drop_idx = Some((0, 1));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let run = Run::new();
    let out = run.go(&rt, &srv.url("/v.mp4"), dir.path()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), body);
    assert!(!run.ctl.resumable.load(Ordering::SeqCst));
    assert_eq!(srv.hits.load(Ordering::SeqCst), 2);
    assert_eq!(files(dir.path()), ["Title.mp4"]);
}

#[test]
fn etag_flip_restarts_once_then_fails_source_changed() {
    let _g = serial();
    let mut cfg = Cfg::new(mp4_moov_last(200_000));
    cfg.etag_per_request = true;
    cfg.drop_idx = Some((0, 3));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let err = Run::new()
        .go(&rt, &srv.url("/v.mp4"), dir.path())
        .unwrap_err();
    assert_eq!(err, "source changed");
    assert_eq!(classify(&err), errors::SOURCE_CHANGED);
    assert!(files(dir.path()).is_empty());
}

#[test]
fn short_body_is_incomplete_and_leaves_nothing() {
    let _g = serial();
    let mut cfg = Cfg::new(mp4_moov_last(200_000));
    cfg.ranges = false;
    cfg.drop_idx = Some((0, 1000));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let err = Run::new()
        .go(&rt, &srv.url("/v.mp4"), dir.path())
        .unwrap_err();
    assert_eq!(classify(&err), errors::INCOMPLETE, "{err}");
    assert!(files(dir.path()).is_empty());
}

#[test]
fn html_error_page_is_not_saved() {
    let _g = serial();
    let mut cfg = Cfg::new(Vec::new());
    cfg.routes.push(Route::new(
        "/v.mp4",
        "text/html",
        b"<html><body>Not today</body></html>".to_vec(),
        false,
    ));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let err = Run::new()
        .go(&rt, &srv.url("/v.mp4"), dir.path())
        .unwrap_err();
    assert_eq!(classify(&err), errors::NOT_A_VIDEO);
    assert!(files(dir.path()).is_empty());
}

#[test]
fn a_manifest_is_refused_for_the_segmented_saver() {
    let _g = serial();
    let mut cfg = Cfg::new(Vec::new());
    cfg.routes.push(Route::new(
        "/m.m3u8",
        "application/vnd.apple.mpegurl",
        b"#EXTM3U\n#EXT-X-ENDLIST\n".to_vec(),
        false,
    ));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let err = Run::new()
        .go(&rt, &srv.url("/m.m3u8"), dir.path())
        .unwrap_err();
    assert!(err.starts_with("refused:"), "{err}");
    assert!(files(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_at_destination_is_never_followed() {
    let _g = serial();
    let body = mp4_moov_last(50_000);
    let srv = start(Cfg::new(body.clone()));
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("precious.bin");
    std::fs::write(&target, b"do not touch").unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("Title.mp4")).unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("Title.mp4.part")).unwrap();
    let rt = rt();
    let out = Run::new().go(&rt, &srv.url("/v.mp4"), dir.path()).unwrap();
    assert_eq!(out.file_name().unwrap().to_string_lossy(), "Title (1).mp4");
    assert_eq!(std::fs::read(&out).unwrap(), body);
    assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
}

#[test]
fn fake_low_disk_pauses_then_resumes() {
    let _g = serial();
    let body = mp4_moov_last(300_000);
    let mut cfg = Cfg::new(body.clone());
    cfg.throttle_bps = Some(100_000);
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let space = Arc::new(AtomicU64::new(1 << 40));
    let s2 = space.clone();
    let mut run = Run::new();
    run.free = Arc::new(move |_| Some(s2.load(Ordering::SeqCst)));
    let run = Arc::new(run);
    let h = run.spawn(&rt, srv.url("/v.mp4"), dir.path().to_owned());
    wait_for("some bytes", || {
        run.ctl.done.load(Ordering::SeqCst) > 10_000
    });
    space.store(0, Ordering::SeqCst);
    wait_for("self pause", || run.ctl.paused());
    assert!(run.ctl.disk_low.load(Ordering::SeqCst));
    let at = run.ctl.done.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(run.ctl.done.load(Ordering::SeqCst), at);
    // Nothing was deleted to make room.
    assert!(files(dir.path()).iter().any(|f| f.ends_with(".part")));
    space.store(1 << 40, Ordering::SeqCst);
    run.ctl.resume();
    let out = rt.block_on(h).unwrap().unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), body);
}

#[test]
fn a_full_disk_is_refused_before_anything_is_written() {
    let _g = serial();
    let srv = start(Cfg::new(mp4_moov_last(50_000)));
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let mut run = Run::new();
    run.free = Arc::new(|_| Some(1000));
    let err = run.go(&rt, &srv.url("/v.mp4"), dir.path()).unwrap_err();
    assert_eq!(classify(&err), errors::DISK_FULL);
    assert!(files(dir.path()).is_empty());
}

#[test]
fn kill_switch_flip_mid_download_stops_and_no_more_requests() {
    let _g = serial();
    let mut cfg = Cfg::new(mp4_moov_last(400_000));
    cfg.throttle_bps = Some(100_000);
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let run = Arc::new(Run::new());
    let h = run.spawn(&rt, srv.url("/v.mp4"), dir.path().to_owned());
    wait_for("some bytes", || {
        run.ctl.done.load(Ordering::SeqCst) > 10_000
    });
    let state = tempfile::tempdir().unwrap();
    let _svc = FetchService::new(
        FetchConfig {
            dht: false,
            require_proxy: true,
            ..Default::default()
        },
        state.path().into(),
    )
    .unwrap();
    let err = rt.block_on(h).unwrap().unwrap_err();
    assert_eq!(classify(&err), errors::PROXY_REQUIRED);
    let hits = srv.hits.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(srv.hits.load(Ordering::SeqCst), hits);
    assert!(files(dir.path()).is_empty());
    // And a new save never connects.
    let err = Run::new()
        .go(&rt, &srv.url("/v.mp4"), dir.path())
        .unwrap_err();
    assert_eq!(classify(&err), errors::PROXY_REQUIRED);
    assert_eq!(srv.hits.load(Ordering::SeqCst), hits);
}

#[test]
fn cancel_removes_only_own_part() {
    let _g = serial();
    let mut cfg = Cfg::new(mp4_moov_last(400_000));
    cfg.throttle_bps = Some(100_000);
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.mp4"), b"mine").unwrap();
    let rt = rt();
    let run = Arc::new(Run::new());
    let h = run.spawn(&rt, srv.url("/v.mp4"), dir.path().to_owned());
    wait_for("some bytes", || {
        run.ctl.done.load(Ordering::SeqCst) > 10_000
    });
    run.ctl.cancel.store(true, Ordering::SeqCst);
    let err = rt.block_on(h).unwrap().unwrap_err();
    assert_eq!(classify(&err), errors::CANCELLED);
    assert_eq!(files(dir.path()), ["keep.mp4"]);
    assert_eq!(std::fs::read(dir.path().join("keep.mp4")).unwrap(), b"mine");
}

#[test]
fn a_swapped_part_file_is_left_alone_on_cancel() {
    let _g = serial();
    let mut cfg = Cfg::new(mp4_moov_last(400_000));
    cfg.throttle_bps = Some(100_000);
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let run = Arc::new(Run::new());
    let h = run.spawn(&rt, srv.url("/v.mp4"), dir.path().to_owned());
    wait_for("some bytes", || {
        run.ctl.done.load(Ordering::SeqCst) > 10_000
    });
    let part = dir.path().join("Title.mp4.part");
    std::fs::remove_file(&part).unwrap();
    std::fs::write(&part, b"someone else").unwrap();
    run.ctl.cancel.store(true, Ordering::SeqCst);
    let _ = rt.block_on(h).unwrap().unwrap_err();
    assert_eq!(std::fs::read(&part).unwrap(), b"someone else");
}

#[test]
fn an_expired_link_is_renewed_and_resumed() {
    let _g = serial();
    let body = mp4_moov_last(300_000);
    let mut cfg = Cfg::new(body.clone());
    cfg.drop_idx = Some((0, 1));
    cfg.forbid = Some(("/old".into(), 1));
    cfg.routes
        .push(Route::new("/old", "video/mp4", body.clone(), true));
    cfg.routes
        .push(Route::new("/new", "video/mp4", body.clone(), true));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let fresh = srv.url("/new");
    let renew: Renew = Arc::new(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(Ok(fresh.clone()));
        rx
    });
    let run = Run::new();
    let out = rt
        .block_on(run.save(srv.url("/old"), dir.path().to_owned(), Some(renew)))
        .unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), body);
}

#[test]
fn without_a_renewal_an_expired_link_fails_cleanly() {
    let _g = serial();
    let mut cfg = Cfg::new(mp4_moov_last(300_000));
    cfg.drop_idx = Some((0, 1));
    cfg.forbid = Some(("/".into(), 1));
    let srv = start(cfg);
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let err = Run::new()
        .go(&rt, &srv.url("/v.mp4"), dir.path())
        .unwrap_err();
    assert_eq!(classify(&err), errors::FORBIDDEN);
    assert!(files(dir.path()).is_empty());
}

#[test]
fn speed_cap_is_honoured() {
    let _g = serial();
    let body = mp4_moov_last(1_000_000);
    let srv = start(Cfg::new(body.clone()));
    let dir = tempfile::tempdir().unwrap();
    let rt = rt();
    let mut run = Run::new();
    run.limiter = Arc::new(RateLimiter::new(Some(250_000)));
    let t0 = Instant::now();
    let out = run.go(&rt, &srv.url("/v.mp4"), dir.path()).unwrap();
    assert!(
        t0.elapsed() >= Duration::from_millis(3600),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(std::fs::read(&out).unwrap(), body);
}
