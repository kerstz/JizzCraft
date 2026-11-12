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
    // Not a black frame. Every film student's first thumbnail.
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

#[test]
fn a_chosen_frame_makes_another_thumbnail() {
    if !common::clips_ready() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("v.webm");
    // Two seconds of a moving ball: early and late frames differ.
    common::make_video(&src, 60, 320, 240);
    let auto = thumbs::video_thumb_at(&src, None).expect("automatic");
    assert_eq!(
        auto,
        thumbs::video_thumb(&src).unwrap(),
        "None is the automatic one"
    );
    let early = thumbs::video_thumb_at(&src, Some(0)).expect("frame at 0");
    let late = thumbs::video_thumb_at(&src, Some(1800)).expect("frame at 1.8 s");
    assert_eq!(early.dimensions(), late.dimensions());
    assert_ne!(early, late, "the chosen frame is used");
    // Past the end there is still a picture.
    assert!(thumbs::video_thumb_at(&src, Some(3_600_000)).is_ok());
    let bad = tmp.path().join("bad.mp4");
    std::fs::write(&bad, b"definitely not a video").unwrap();
    assert!(thumbs::video_thumb_at(&bad, Some(10)).is_err());
}

/// The automatic thumbnail keeps the name it always had (no cache is made
/// again after an upgrade); a chosen frame gets a name of its own.
#[test]
fn a_chosen_frame_has_its_own_cache_name() {
    let p = std::path::Path::new("/lib/clip.webm");
    let mut h = blake3::Hasher::new();
    h.update(b"thumb-v1");
    h.update(b"/lib/clip.webm");
    h.update(&7u64.to_le_bytes());
    h.update(&9i64.to_le_bytes());
    let before = h.finalize().to_hex().as_str()[..32].to_owned();
    assert_eq!(thumbs::cache_key(p, 7, 9), before);
    assert_eq!(thumbs::cache_key_at(p, 7, 9, None), before);
    let at = thumbs::cache_key_at(p, 7, 9, Some(1500));
    assert_ne!(at, before);
    assert_ne!(at, thumbs::cache_key_at(p, 7, 9, Some(1501)));
    assert_eq!(at.len(), 32);

    let keys = std::sync::Arc::new(
        jizzcraft_core::vault::VaultKey::from_bytes_for_tests([3; 32]).thumb_keys(),
    );
    let tmp = tempfile::tempdir().unwrap();
    let vault = thumbs::ThumbStore::new(tmp.path().to_path_buf(), Some(keys));
    assert_eq!(vault.thumb_id_at(p, 7, 9, None), vault.thumb_id(p, 7, 9));
    assert_ne!(
        vault.thumb_id_at(p, 7, 9, Some(1500)),
        vault.thumb_id(p, 7, 9)
    );
    let plain = thumbs::ThumbStore::new(tmp.path().to_path_buf(), None);
    assert_eq!(plain.thumb_id_at(p, 7, 9, Some(1500)), at);
}

/// The cache prune keeps the thumbnail of a chosen frame.
#[test]
fn live_keys_follow_the_chosen_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("lib");
    std::fs::create_dir(&lib).unwrap();
    std::fs::write(lib.join("clip.webm"), b"a name is enough here").unwrap();
    let mut db = jizzcraft_core::Db::open_in_memory().unwrap();
    jizzcraft_core::scan::import_folder(&mut db, &lib).unwrap();
    let m = db
        .query(&jizzcraft_core::Filter::default())
        .unwrap()
        .remove(0);
    let auto = thumbs::cache_key(&m.path, m.size, m.mtime);
    assert!(thumbs::live_keys(&db).unwrap().contains(&auto));
    db.set_thumb_ms(m.id, Some(1234)).unwrap();
    let live = thumbs::live_keys(&db).unwrap();
    assert!(live.contains(&thumbs::cache_key_at(&m.path, m.size, m.mtime, Some(1234))));
    assert!(!live.contains(&auto));
}
