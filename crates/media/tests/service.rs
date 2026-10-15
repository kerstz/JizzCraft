//! The background thumbnail service: results arrive, the newest request wins,
//! errors are reported per item and the cache lives outside the media folder.

use jizzcraft_core::MediaKind;
use jizzcraft_media::service::{Request, ThumbService};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn png(path: &std::path::Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 80])
    })
    .save(path)
    .unwrap();
}

fn req(id: i64, path: std::path::PathBuf) -> Request {
    let meta = std::fs::metadata(&path).ok();
    Request {
        id,
        path,
        kind: MediaKind::Image,
        size: meta.as_ref().map_or(0, |m| m.len()),
        mtime: 0,
        need_meta: false,
    }
}

fn collect(svc: &ThumbService, n: usize) -> Vec<jizzcraft_media::service::Done> {
    let end = Instant::now() + Duration::from_secs(20);
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < end {
        match svc.try_recv() {
            Some(d) => out.push(d),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    out
}

#[test]
fn thumbnails_arrive_for_good_files_and_errors_for_bad_ones() {
    let media = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let good = media.path().join("good.png");
    png(&good, 640, 480);
    let bad = media.path().join("bad.png");
    std::fs::write(&bad, b"this is not a png").unwrap();
    let missing = media.path().join("missing.png");

    let before: Vec<_> = std::fs::read_dir(media.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    let svc = ThumbService::new(cache.path().to_path_buf(), Arc::new(|| {}));
    svc.set_wanted(vec![req(1, good), req(2, bad), req(3, missing)]);
    let mut done = collect(&svc, 3);
    done.sort_by_key(|d| d.id);
    assert_eq!(done.len(), 3);

    let t = done[0].thumb.as_ref().expect("good file gets a thumbnail");
    assert!(t.full.w <= 320 && t.full.h <= 320 && t.full.w > 0);
    assert_eq!(t.full.data.len(), (t.full.w * t.full.h * 4) as usize);
    assert!(done[1].thumb.is_err());
    assert!(done[2].thumb.is_err());
    // Error text never carries the path. It can say "no" without saying who.
    for d in &done[1..] {
        let e = d.thumb.as_ref().err().unwrap();
        assert!(!e.contains("bad.png") && !e.contains("missing.png"), "{e}");
    }
    // Nothing was written next to the originals.
    let after: Vec<_> = std::fs::read_dir(media.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(before.len(), after.len());
    assert!(!svc.is_busy() || collect(&svc, 1).is_empty());
}

#[test]
fn a_second_pass_reuses_the_cache() {
    let media = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let p = media.path().join("a.png");
    png(&p, 400, 300);
    let svc = ThumbService::new(cache.path().to_path_buf(), Arc::new(|| {}));
    svc.set_wanted(vec![req(7, p.clone())]);
    assert_eq!(collect(&svc, 1).len(), 1);
    let cached: usize = walk(cache.path());
    assert!(cached > 0, "thumbnails are cached on disk");
    svc.set_wanted(vec![req(7, p)]);
    let again = collect(&svc, 1);
    assert!(again[0].thumb.is_ok());
    assert_eq!(
        walk(cache.path()),
        cached,
        "no new cache entries on a second pass"
    );
}

fn walk(p: &std::path::Path) -> usize {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Audit M1: a decode still in flight when the service is dropped must never
/// reach the cache, even after the owner cleaned it (an encryption does).
#[test]
fn a_dropped_plain_service_writes_nothing_after_its_workers_left() {
    use image::RgbImage;
    use jizzcraft_media::service::Workers;
    let media = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let f = media.path().join("a.png");
    png(&f, 64, 64);
    let workers = Workers::new();
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = started.clone();
    let slow: jizzcraft_media::service::Decoder = Arc::new(move |_| {
        s2.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(600));
        Ok(RgbImage::from_pixel(8, 8, image::Rgb([9, 9, 9])))
    });
    let svc = ThumbService::with_decoder(
        cache.path().to_path_buf(),
        None,
        Arc::new(|| {}),
        &workers,
        slow,
    );
    assert!(
        !workers.quiesced(),
        "workers are alive while the service is"
    );
    svc.set_wanted(vec![req(1, f)]);
    let end = Instant::now() + Duration::from_secs(10);
    while !started.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(Instant::now() < end, "decode never started");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The request is in flight: lock/convert drops the service now.
    drop(svc);
    assert!(
        workers.wait_quiesced(Duration::from_secs(20)),
        "workers returned"
    );
    // The conversion's clean-up runs once the workers are gone...
    let _ = std::fs::remove_dir_all(cache.path().join("thumbs"));
    // ...and nothing appears afterwards.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!cache.path().join("thumbs").exists(), "no clear thumbnail");
    assert!(std::fs::read_dir(cache.path()).unwrap().next().is_none());
}

/// Audit M1: the thumbnail keys are gone once the workers are, not whenever a
/// detached thread happens to finish.
#[test]
fn thumb_keys_are_freed_with_the_workers() {
    use jizzcraft_core::vault::{ThumbKeys, VaultKey};
    use jizzcraft_media::service::Workers;
    let cache = tempfile::tempdir().unwrap();
    let keys: Arc<ThumbKeys> = Arc::new(VaultKey::from_bytes_for_tests([7; 32]).thumb_keys());
    let weak = Arc::downgrade(&keys);
    let workers = Workers::new();
    let svc = ThumbService::tracked(
        cache.path().to_path_buf(),
        Some(keys),
        Arc::new(|| {}),
        &workers,
    );
    assert!(
        weak.upgrade().is_none(),
        "the service keeps no key, only a cipher"
    );
    svc.shutdown();
    assert!(workers.wait_quiesced(Duration::from_secs(10)));
    assert!(svc.quiesced());
    assert_eq!(workers.live(), 0);
    // A request after the shutdown is ignored.
    svc.set_wanted(vec![req(1, std::path::PathBuf::from("/nonexistent/x.png"))]);
    assert!(!svc.is_busy());
}
