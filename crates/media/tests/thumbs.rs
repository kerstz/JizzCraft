mod common;

use jizzcraft_media::{probe, thumbs};

#[test]
fn image_thumbnail_is_bounded_and_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("big.png");
    image::RgbImage::from_fn(1600, 900, |x, y| {
        image::Rgb([(x / 7) as u8, (y / 4) as u8, 90])
    })
    .save(&src)
    .unwrap();
    let t = thumbs::image_thumb(&src).unwrap();
    assert_eq!(t.width(), 320);
    assert_eq!(t.height(), 180);

    let key = thumbs::cache_key(&src, 1, 2);
    assert_eq!(key, thumbs::cache_key(&src, 1, 2));
    assert_ne!(key, thumbs::cache_key(&src, 1, 3));
    let file = thumbs::thumb_file(&tmp.path().join("cache"), &key);
    thumbs::save_cached(&file, &t).unwrap();
    assert_eq!(thumbs::load_cached(&file).unwrap().dimensions(), (320, 180));
    // Nothing next to the original.
    let siblings: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(siblings.iter().all(|n| n == "big.png" || n == "cache"));
}

#[test]
fn video_thumbnail_and_probe() {
    if !common::clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("v.webm");
    common::make_video(&src, 60, 640, 360);
    let t = thumbs::video_thumb(&src).expect("video thumb");
    assert!(t.width() <= 320 && t.height() <= 320);
    assert_eq!(
        t.width(),
        320,
        "landscape clip should hit the width cap: {}x{}",
        t.width(),
        t.height()
    );
    // Not a black frame.
    let sum: u64 = t
        .pixels()
        .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
        .sum();
    assert!(sum > 0);

    let m = probe::probe(&src).expect("probe");
    assert_eq!((m.width, m.height), (Some(640), Some(360)));
    let d = m.duration_ms.unwrap();
    assert!((1500..2500).contains(&d), "duration {d}");
    assert!(m.codec.is_some());
}

#[test]
fn garbage_video_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("bad.mp4");
    std::fs::write(&src, b"definitely not a video").unwrap();
    assert!(thumbs::video_thumb(&src).is_err());
}
