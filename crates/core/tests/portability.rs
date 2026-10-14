use jizzcraft_core::fingerprint::fingerprint_pending;
use jizzcraft_core::integrity::{Scope, checksum_pending};
use jizzcraft_core::portability::{ExportOpts, backup, export_meta, import_meta};
use jizzcraft_core::{Db, Filter, Registry, register_core, scan};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn png(path: &Path, w: u32, salt: u8) {
    image::RgbImage::from_fn(w, w, |x, y| image::Rgb([x as u8 ^ salt, y as u8, salt]))
        .save(path)
        .unwrap();
}

fn lib(root: &Path) -> Db {
    png(&root.join("a.png"), 33, 1);
    png(&root.join("b.png"), 34, 2);
    fs::write(root.join("v.mp4"), vec![5u8; 5000]).unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, root).unwrap();
    db
}

fn id_of(db: &Db, name: &str) -> i64 {
    db.query(&Filter::default())
        .unwrap()
        .into_iter()
        .find(|m| m.name == name)
        .unwrap()
        .id
}

fn fingerprint(db: &mut Db) {
    let c = AtomicBool::new(false);
    fingerprint_pending(db, false, None, &c, &mut |_, _| {}).unwrap();
}

fn hash_tree(root: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    walk(root)
        .into_iter()
        .map(|p| {
            let h = *blake3::hash(&fs::read(&p).unwrap()).as_bytes();
            (p, h)
        })
        .collect()
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn backup_opens_and_refuses_overwrite_and_media_names() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let db = lib(t.path());
    // `VACUUM INTO` needs a file database to be meaningful; in-memory works too.
    let dest = work.path().join("copy.db");
    let n = backup(&db, &dest).unwrap();
    assert!(n > 0);
    let copy = Db::open(&dest).unwrap();
    assert_eq!(copy.stats().unwrap().total, 3);

    assert!(
        backup(&db, &dest).is_err(),
        "no overwrite: a backup never eats an older one"
    );
    assert!(
        backup(&db, &work.path().join("oops.mp4")).is_err(),
        "media extension"
    );
    assert!(backup(&db, &work.path().join("missing/x.db")).is_err());
}

#[test]
fn export_import_roundtrip_with_privacy_flags() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut db = lib(t.path());
    let (a, v) = (id_of(&db, "a.png"), id_of(&db, "v.mp4"));
    db.set_favorite(a, true).unwrap();
    db.set_rating(a, 4).unwrap();
    db.add_marker(v, 500, "mark").unwrap();
    db.record_progress(v, 20_000, Some(600_000), true).unwrap();
    db.save_view(
        "mine",
        &Filter {
            min_rating: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    fingerprint(&mut db);

    let plain = work.path().join("plain.json");
    let rep = export_meta(&db, &plain, &ExportOpts::default()).unwrap();
    assert_eq!(rep.items, 2);
    let text = fs::read_to_string(&plain).unwrap();
    assert!(
        !text.contains("a.png") && !text.contains("v.mp4"),
        "no names"
    );
    assert!(
        !text.contains("play_count") && !text.contains("last_played_at"),
        "no history"
    );
    assert!(!text.contains(t.path().to_str().unwrap()), "no paths");

    let full = work.path().join("full.json");
    export_meta(
        &db,
        &full,
        &ExportOpts {
            include_history: true,
            include_names: true,
        },
    )
    .unwrap();
    let ftext = fs::read_to_string(&full).unwrap();
    assert!(ftext.contains("a.png") && ftext.contains("play_count"));

    assert!(
        export_meta(&db, &plain, &ExportOpts::default()).is_err(),
        "no overwrite"
    );

    // Fresh library over the same files: metadata comes back by fingerprint.
    let mut fresh = Db::open_in_memory().unwrap();
    scan::import_folder(&mut fresh, t.path()).unwrap();
    fingerprint(&mut fresh);
    let dry = import_meta(&mut fresh, &plain, true).unwrap();
    assert_eq!((dry.matched, dry.applied), (2, 0));
    assert_eq!(
        fresh.get(id_of(&fresh, "a.png")).unwrap().unwrap().rating,
        0,
        "dry run"
    );

    let done = import_meta(&mut fresh, &plain, false).unwrap();
    assert_eq!((done.matched, done.applied, done.views), (2, 2, 1));
    let m = fresh.get(id_of(&fresh, "a.png")).unwrap().unwrap();
    assert!(m.favorite && m.rating == 4);
    assert_eq!(fresh.markers(id_of(&fresh, "v.mp4")).unwrap().len(), 1);
    assert_eq!(fresh.views().unwrap().len(), 1);

    // Garbage and wrong format are rejected.
    let bad = work.path().join("bad.json");
    fs::write(&bad, "{\"format\":\"other\"}").unwrap();
    assert!(import_meta(&mut fresh, &bad, true).is_err());
}

#[test]
fn bit_rot_is_flagged_when_mtime_is_unchanged() {
    let t = tempfile::tempdir().unwrap();
    let mut db = lib(t.path());
    let c = AtomicBool::new(false);
    let first = checksum_pending(&mut db, Scope::All, None, &c, &mut |_, _| {}).unwrap();
    assert_eq!((first.checked, first.ok, first.damaged), (3, 3, 0));

    // Flip a byte, keep size and mtime: silent corruption. The perfect crime,
    // foiled by a hash.
    let p = t.path().join("v.mp4");
    let before = fs::metadata(&p).unwrap().modified().unwrap();
    let mut bytes = fs::read(&p).unwrap();
    bytes[100] ^= 0xff;
    fs::write(&p, &bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(before)
        .unwrap();

    let second = checksum_pending(&mut db, Scope::All, None, &c, &mut |_, _| {}).unwrap();
    assert_eq!((second.damaged, second.ok), (1, 2));
    let m = db.get(id_of(&db, "v.mp4")).unwrap().unwrap();
    assert_eq!(
        (m.damaged, m.check_note.as_deref()),
        (2, Some("checksum-changed"))
    );
    assert_eq!(db.stats().unwrap().damaged, 1);

    // Unchecked scope skips everything already checked.
    let none = checksum_pending(&mut db, Scope::Unchecked, None, &c, &mut |_, _| {}).unwrap();
    assert_eq!(none.checked, 0);

    // Cancel flag stops before work.
    let stop = AtomicBool::new(true);
    let r = checksum_pending(&mut db, Scope::All, None, &stop, &mut |_, _| {}).unwrap();
    assert!(r.cancelled);
}

/// The hard rule: nothing in the library engine may modify, move or delete a
/// user's media. Hash every file, run every command, hash again.
#[test]
fn engine_never_touches_media() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut db = lib(t.path());
    let before = hash_tree(t.path());

    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let (a, b, v) = (
        id_of(&db, "a.png"),
        id_of(&db, "b.png"),
        id_of(&db, "v.mp4"),
    );
    let run = |db: &mut Db, name: &str, args| reg.run(db, name, args).unwrap();

    run(&mut db, "library.rescan", json!({}));
    run(&mut db, "media.rate", json!({ "id": a, "rating": 3 }));
    run(&mut db, "media.hide", json!({ "id": b }));
    run(&mut db, "media.hide", json!({ "id": b }));
    run(
        &mut db,
        "media.set",
        json!({ "id": a, "favorite": true, "inbox": false }),
    );
    run(&mut db, "media.inbox.clear", json!({}));
    run(
        &mut db,
        "history.progress",
        json!({ "id": v, "pos_ms": 30000, "duration_ms": 900000, "count_play": true }),
    );
    run(&mut db, "history.pause", json!({ "paused": true }));
    run(&mut db, "history.clear", json!({}));
    let m = run(
        &mut db,
        "markers.add",
        json!({ "id": v, "pos_ms": 1, "label": "x" }),
    );
    run(
        &mut db,
        "markers.update",
        json!({ "marker_id": m["id"], "label": "y" }),
    );
    run(&mut db, "markers.remove", json!({ "marker_id": m["id"] }));
    run(&mut db, "library.fingerprint", json!({ "all": true }));
    run(&mut db, "library.checksum", json!({ "scope": "all" }));
    run(
        &mut db,
        "views.save",
        json!({ "name": "v", "spec": { "fav": true } }),
    );
    run(&mut db, "library.tree", json!({}));
    run(&mut db, "media.random", json!({}));
    run(
        &mut db,
        "library.backup",
        json!({ "dest": work.path().join("b.db") }),
    );
    run(
        &mut db,
        "meta.export",
        json!({ "dest": work.path().join("e.json") }),
    );
    run(
        &mut db,
        "meta.import",
        json!({ "src": work.path().join("e.json") }),
    );
    run(&mut db, "media.relink", json!({ "from": b, "to": a }));

    assert_eq!(hash_tree(t.path()), before, "media tree changed");
}
