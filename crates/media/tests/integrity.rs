mod common;

use jizzcraft_core::MediaKind;
use jizzcraft_media::integrity::{CacheKind, CheckMode, check_media, clear_cache};
use std::sync::atomic::AtomicBool;

#[test]
fn good_video_passes_both_modes() {
    if !common::clips_ready() || !common::ready(&["fakesink"]) {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("ok.webm");
    common::make_video(&p, 60, 320, 180);
    let c = AtomicBool::new(false);
    assert_eq!(
        check_media(&p, MediaKind::Video, CheckMode::Quick, &c),
        Ok(())
    );
    assert_eq!(
        check_media(&p, MediaKind::Video, CheckMode::Full, &c),
        Ok(())
    );
}

#[test]
fn garbage_video_is_flagged() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("bad.mp4");
    std::fs::write(&p, vec![0u8; 8192]).unwrap();
    let c = AtomicBool::new(false);
    assert!(check_media(&p, MediaKind::Video, CheckMode::Quick, &c).is_err());
    assert!(check_media(&p, MediaKind::Video, CheckMode::Full, &c).is_err());
}

#[test]
fn truncated_png_fails_full_but_header_passes_quick() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("t.png");
    image::RgbImage::from_fn(200, 200, |x, y| {
        image::Rgb([x as u8, y as u8, (x ^ y) as u8])
    })
    .save(&p)
    .unwrap();
    let full = std::fs::read(&p).unwrap();
    std::fs::write(&p, &full[..full.len() / 2]).unwrap();
    let c = AtomicBool::new(false);
    assert_eq!(
        check_media(&p, MediaKind::Image, CheckMode::Quick, &c),
        Ok(())
    );
    assert_eq!(
        check_media(&p, MediaKind::Image, CheckMode::Full, &c),
        Err("decode-error")
    );
}

#[test]
fn clear_cache_only_touches_known_subdirs() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    for d in ["previews/ab", "thumbs/cd", "keepme"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
        std::fs::write(root.join(d).join("f"), b"12345").unwrap();
    }
    let n = clear_cache(root, CacheKind::Previews).unwrap();
    assert_eq!(n, 5);
    assert!(!root.join("previews").exists());
    assert!(root.join("thumbs").exists() && root.join("keepme").exists());
    clear_cache(root, CacheKind::All).unwrap();
    assert!(!root.join("thumbs").exists());
    assert!(root.join("keepme/f").exists(), "unrelated dirs survive");
}
