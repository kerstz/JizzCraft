//! The encrypted thumbnail cache: round trip, tamper, no image signature on
//! disk, nothing in the clear directories, prune and clear.

use jizzcraft_core::MediaKind;
use jizzcraft_core::vault::{ThumbKeys, VaultKey};
use jizzcraft_media::integrity::{CacheKind, clear_cache};
use jizzcraft_media::preview::PreviewService;
use jizzcraft_media::service::{Request, ThumbService};
use jizzcraft_media::thumbs::{self, ThumbStore, VAULT_DIR};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn keys(seed: u8) -> Arc<ThumbKeys> {
    Arc::new(VaultKey::from_bytes_for_tests([seed; 32]).thumb_keys())
}

fn img() -> image::RgbImage {
    image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 5) as u8, 90])
    })
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn round_trip_with_different_name_and_no_image_signature() {
    let d = tempfile::tempdir().unwrap();
    let store = ThumbStore::new(d.path().to_path_buf(), Some(keys(1)));
    let p = Path::new("/media/some/clip.mp4");
    assert!(store.load_thumb(p, 10, 20).is_none());
    store.save_thumb(p, 10, 20, &img()).unwrap();
    let back = store.load_thumb(p, 10, 20).expect("cached");
    assert_eq!(back.dimensions(), (64, 48));
    assert!(store.load_thumb(p, 10, 21).is_none());

    let fs = files(&d.path().join(VAULT_DIR));
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0].extension().unwrap(), "jzt");
    let name = fs[0].file_stem().unwrap().to_str().unwrap();
    assert_ne!(name, thumbs::cache_key(p, 10, 20));
    let bytes = std::fs::read(&fs[0]).unwrap();
    assert_eq!(&bytes[..4], b"JZT1");
    for sig in [&[0xFF, 0xD8, 0xFF][..], b"\x89PNG", b"RIFF", b"WEBP"] {
        assert!(
            !bytes.windows(sig.len()).any(|w| w == sig),
            "image signature {sig:?} found"
        );
    }
    assert!(!d.path().join("thumbs").exists() && !d.path().join("previews").exists());
}

#[test]
fn tampered_or_foreign_file_is_absent_and_deleted() {
    let d = tempfile::tempdir().unwrap();
    let store = ThumbStore::new(d.path().to_path_buf(), Some(keys(1)));
    let p = Path::new("/m/a.jpg");
    store.save_thumb(p, 1, 2, &img()).unwrap();
    let f = files(&d.path().join(VAULT_DIR)).remove(0);
    let mut b = std::fs::read(&f).unwrap();
    let n = b.len();
    b[n / 2] ^= 1;
    std::fs::write(&f, &b).unwrap();
    assert!(store.load_thumb(p, 1, 2).is_none());
    assert!(!f.exists());

    // Another key: same path, different name; a copied file does not open.
    store.save_thumb(p, 1, 2, &img()).unwrap();
    let other = ThumbStore::new(d.path().to_path_buf(), Some(keys(2)));
    let id = store.thumb_id(p, 1, 2);
    assert_ne!(id, other.thumb_id(p, 1, 2));
    assert!(other.load(&store.thumb_path(&id), &id).is_none());
    assert!(!store.thumb_path(&id).exists());
}

#[test]
fn service_and_preview_write_only_vault_thumbs() {
    let media = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = media.path().join("a.png");
    img().save(&src).unwrap();
    let svc = ThumbService::with_keys(cache.path().to_path_buf(), Some(keys(3)), Arc::new(|| {}));
    let mk = || {
        vec![Request {
            id: 1,
            path: src.clone(),
            kind: MediaKind::Image,
            size: 7,
            mtime: 0,
            need_meta: false,
        }]
    };
    svc.set_wanted(mk());
    let end = Instant::now() + Duration::from_secs(20);
    let done = loop {
        if let Some(d) = svc.try_recv() {
            break d;
        }
        assert!(Instant::now() < end, "no result");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(done.thumb.is_ok());
    assert!(!cache.path().join("thumbs").exists());
    assert_eq!(files(&cache.path().join(VAULT_DIR)).len(), 1);

    // A preview of a missing file fails without writing anything in clear.
    let pv = PreviewService::with_keys(
        "/nonexistent/x.mp4".into(),
        0,
        0,
        cache.path().to_path_buf(),
        Some(keys(3)),
        Arc::new(|| {}),
    );
    drop(pv);
    assert!(!cache.path().join("previews").exists());
}

#[test]
fn prune_and_clear_work_on_vault_thumbs() {
    let d = tempfile::tempdir().unwrap();
    let store = ThumbStore::new(d.path().to_path_buf(), Some(keys(4)));
    let live_p = Path::new("/m/live.jpg");
    let dead_p = Path::new("/m/dead.jpg");
    store.save_thumb(live_p, 1, 1, &img()).unwrap();
    store.save_thumb(dead_p, 1, 1, &img()).unwrap();
    // A preview, so Previews/Thumbs clearing can be told apart.
    let (id, file) = store.preview_slot_for_tests(live_p, 1, 1, 3);
    store.save(&file, &id, &img()).unwrap();
    assert_eq!(files(&d.path().join(VAULT_DIR)).len(), 3);

    let live: std::collections::HashSet<String> =
        [store.thumb_id(live_p, 1, 1)].into_iter().collect();
    let r = thumbs::prune_store(&store, Some(&live), u64::MAX);
    assert_eq!((r.orphans, r.kept_files), (1, 2));
    assert!(store.load_thumb(live_p, 1, 1).is_some());
    assert!(store.load_thumb(dead_p, 1, 1).is_none());

    let r = thumbs::prune_store(&store, Some(&live), 0);
    assert_eq!((r.evicted, r.kept_files), (2, 0));

    store.save_thumb(live_p, 1, 1, &img()).unwrap();
    store.save(&file, &id, &img()).unwrap();
    let n = clear_cache(d.path(), CacheKind::Previews).unwrap();
    assert!(n > 0);
    assert!(store.load_thumb(live_p, 1, 1).is_some());
    assert!(store.load(&file, &id).is_none());
    store.save(&file, &id, &img()).unwrap();
    clear_cache(d.path(), CacheKind::Thumbs).unwrap();
    assert!(store.load_thumb(live_p, 1, 1).is_none());
    assert!(store.load(&file, &id).is_some());
    clear_cache(d.path(), CacheKind::All).unwrap();
    assert!(!d.path().join(VAULT_DIR).exists());
}

#[test]
fn plain_mode_is_unchanged() {
    let d = tempfile::tempdir().unwrap();
    let store = ThumbStore::new(d.path().to_path_buf(), None);
    let p = Path::new("/m/a.jpg");
    store.save_thumb(p, 1, 2, &img()).unwrap();
    let key = thumbs::cache_key(p, 1, 2);
    assert!(thumbs::thumb_file(d.path(), &key).is_file());
    assert!(store.load_thumb(p, 1, 2).is_some());
    assert!(!d.path().join(VAULT_DIR).exists());
}
