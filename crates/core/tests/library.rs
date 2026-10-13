use jizzcraft_core::{Db, Filter, MediaKind, Registry, register_core, scan};
use serde_json::json;
use std::fs;
use std::path::Path;

fn png(path: &Path, w: u32, h: u32) {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    img.save(path).unwrap();
}

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("sub dir/été")).unwrap();
    png(&root.join("a.png"), 64, 48);
    png(&root.join("sub dir/été/ünï cödé.PNG"), 32, 32);
    fs::write(root.join("clip.mp4"), b"not really a video").unwrap();
    fs::write(root.join("notes.txt"), b"ignored").unwrap();
    fs::create_dir_all(root.join(".hidden")).unwrap();
    png(&root.join(".hidden/secret.png"), 8, 8);
    std::os::unix::fs::symlink(root.join("a.png"), root.join("link.png")).unwrap();
}

#[test]
fn scan_query_favorite_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut db = Db::open_in_memory().unwrap();

    let (_, rep) = scan::import_folder(&mut db, tmp.path()).unwrap();
    assert_eq!(rep.added, 3, "{rep:?}");
    assert_eq!(rep.skipped_symlinks, 1);
    assert_eq!(rep.skipped_unsupported, 1);

    // Duplicate import is idempotent.
    let (_, rep2) = scan::import_folder(&mut db, tmp.path()).unwrap();
    assert_eq!((rep2.added, rep2.unchanged), (0, 3));
    assert_eq!(db.folders().unwrap().len(), 1);

    let all = db.query(&Filter::default()).unwrap();
    assert_eq!(all.len(), 3);
    let a = all.iter().find(|m| m.name == "a.png").unwrap();
    assert_eq!((a.width, a.height), (Some(64), Some(48)));
    assert!(all.iter().any(|m| m.name == "ünï cödé.PNG"));

    let videos = db
        .query(&Filter {
            kind: Some(MediaKind::Video),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(videos.len(), 1);
    // Unicode, case-insensitive substring search.
    let found = db
        .query(&Filter {
            name: Some("CÖDÉ".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(found.len(), 1);
    // LIKE wildcards are literal.
    assert_eq!(
        db.count(&Filter {
            name: Some("%".into()),
            ..Default::default()
        })
        .unwrap(),
        0
    );

    assert!(db.toggle_favorite(a.id).unwrap());
    let favs = db
        .query(&Filter {
            favorites: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(favs.len(), 1);

    // Deleted file is flagged, not dropped; favorite survives.
    fs::remove_file(tmp.path().join("a.png")).unwrap();
    let rep3 = scan::rescan_all(&mut db).unwrap();
    assert_eq!(rep3.marked_missing, 1);
    assert_eq!(db.query(&Filter::default()).unwrap().len(), 2);
    let with_missing = db
        .query(&Filter {
            include_missing: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(with_missing.len(), 3);
    assert!(with_missing.iter().any(|m| m.missing && m.favorite));
    assert_eq!(db.stats().unwrap().missing, 1);

    // File comes back: flag cleared.
    png(&tmp.path().join("a.png"), 64, 48);
    scan::rescan_all(&mut db).unwrap();
    assert_eq!(db.stats().unwrap().missing, 0);
}

#[test]
fn unreadable_dir_does_not_mark_missing() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("locked")).unwrap();
    png(&tmp.path().join("locked/x.png"), 8, 8);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, tmp.path()).unwrap();

    fs::set_permissions(tmp.path().join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let rep = scan::rescan_all(&mut db).unwrap();
    fs::set_permissions(tmp.path().join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    // Running as root bypasses permissions; only assert when the error fired.
    if rep.errors > 0 {
        assert_eq!(rep.marked_missing, 0);
        assert_eq!(db.stats().unwrap().missing, 0);
    }
}

#[test]
fn migrations_persist_across_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let dbp = tmp.path().join("lib.db");
    let media = tmp.path().join("media");
    fs::create_dir(&media).unwrap();
    png(&media.join("p.png"), 4, 4);

    let id = {
        let mut db = Db::open(&dbp).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            jizzcraft_core::db::SCHEMA_VERSION
        );
        scan::import_folder(&mut db, &media).unwrap();
        let id = db.query(&Filter::default()).unwrap()[0].id;
        db.set_favorite(id, true).unwrap();
        id
    };
    let db = Db::open(&dbp).unwrap();
    let favs = db
        .query(&Filter {
            favorites: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(favs.len(), 1);
    assert_eq!(favs[0].id, id);
}

#[test]
fn registry_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    png(&tmp.path().join("z.png"), 4, 4);
    let mut db = Db::open_in_memory().unwrap();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);

    let r = reg
        .run(&mut db, "library.import", json!({ "path": tmp.path() }))
        .unwrap();
    assert_eq!(r["report"]["added"], 1);
    let q = reg.run(&mut db, "media.query", json!({})).unwrap();
    let id = q["items"][0]["id"].as_i64().unwrap();
    let t = reg
        .run(&mut db, "media.favorite.toggle", json!({ "id": id }))
        .unwrap();
    assert_eq!(t["favorite"], true);
    assert!(reg.run(&mut db, "nope", json!({})).is_err());
    assert!(
        reg.run(&mut db, "media.favorite.toggle", json!({ "id": 9999 }))
            .is_err()
    );
}
