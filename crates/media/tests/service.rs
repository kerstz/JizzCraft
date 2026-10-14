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
    // Error text never carries the path.
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
