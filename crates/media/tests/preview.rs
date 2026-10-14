mod common;

use jizzcraft_media::preview::{PREVIEW_W, PreviewService, bucket_for};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait_for<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out");
}

#[test]
fn preview_frame_is_small_cached_and_drop_is_quick() {
    if !common::clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = tmp.path().join("p.webm");
    common::make_video(&src, 150, 640, 360);
    let md = std::fs::metadata(&src).unwrap();
    let mk = || {
        PreviewService::new(
            src.clone(),
            md.len(),
            0,
            cache.path().to_path_buf(),
            Arc::new(|| {}),
        )
    };

    let svc = mk();
    let bucket = bucket_for(Duration::from_secs(1), Duration::from_secs(5));
    svc.request(bucket, Duration::from_secs(1));
    let f = wait_for(15, || svc.take());
    assert_eq!(f.bucket, bucket);
    assert!(f.full.w <= PREVIEW_W && f.full.w > 0, "width {}", f.full.w);
    assert_eq!(f.full.data.len(), (f.full.w * f.full.h * 4) as usize);
    assert!(f.blur.w < f.full.w);

    let t = Instant::now();
    drop(svc);
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "drop must not block"
    );

    // Cached on disk: a fresh service answers from the file. Déjà vu, on purpose.
    let previews = cache.path().join("previews");
    assert!(previews.is_dir());
    let svc2 = mk();
    svc2.request(bucket, Duration::from_secs(1));
    let again = wait_for(5, || svc2.take());
    assert_eq!(again.bucket, bucket);
}

#[test]
fn missing_file_marks_failed_without_panicking() {
    let cache = tempfile::tempdir().unwrap();
    let svc = PreviewService::new(
        "/nonexistent/x.mp4".into(),
        0,
        0,
        cache.path().to_path_buf(),
        Arc::new(|| {}),
    );
    svc.request(0, Duration::ZERO);
    wait_for(10, || svc.failed().then_some(()));
    assert!(svc.take().is_none());
}
