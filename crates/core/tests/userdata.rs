use jizzcraft_core::{Db, Filter, Registry, SortKey, register_core, scan};
use serde_json::json;
use std::fs;
use std::path::Path;

fn png(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 7]))
        .save(path)
        .unwrap();
}

fn library() -> (tempfile::TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    for n in ["a.png", "b.png", "c.png"] {
        png(&tmp.path().join(n), 16, 16);
    }
    fs::write(tmp.path().join("v.mp4"), b"not a real video").unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, tmp.path()).unwrap();
    (tmp, db)
}

fn id_of(db: &Db, name: &str) -> i64 {
    db.query(&Filter::default())
        .unwrap()
        .into_iter()
        .find(|m| m.name == name)
        .unwrap()
        .id
}

#[test]
fn rating_hide_and_inbox() {
    let (_t, db) = library();
    let a = id_of(&db, "a.png");
    // Fresh imports start in the inbox.
    assert!(db.get(a).unwrap().unwrap().inbox);

    db.set_rating(a, 4).unwrap();
    assert_eq!(db.get(a).unwrap().unwrap().rating, 4);
    assert!(db.set_rating(a, 6).is_err());
    assert!(db.set_rating(9999, 3).is_err());

    db.set_hidden(a, true).unwrap();
    let m = db.get(a).unwrap().unwrap();
    assert!(m.hidden && !m.inbox, "hiding leaves the inbox");
    assert!(!db.toggle_hidden(a).unwrap());

    assert_eq!(db.clear_inbox(None).unwrap(), 3);
    assert_eq!(db.stats().unwrap().inbox, 0);
}

#[test]
fn history_respects_settings_and_clears() {
    let (_t, mut db) = library();
    let v = id_of(&db, "v.mp4");
    let ten_min = Some(600_000);

    let p = db.record_progress(v, 120_000, ten_min, true).unwrap();
    assert_eq!(p.resume_ms, Some(120_000));
    assert!(p.counted && p.recorded);
    let m = db.get(v).unwrap().unwrap();
    assert_eq!((m.play_count, m.resume_ms), (1, Some(120_000)));
    assert!(m.last_played_at.is_some());

    // Near the end: no resume point, still counted as played.
    db.record_progress(v, 595_000, ten_min, false).unwrap();
    assert_eq!(db.get(v).unwrap().unwrap().resume_ms, None);

    // Paused history: nothing counted, last_played untouched. What happens in
    // pause mode stays in pause mode.
    db.set_setting("history.enabled", "0").unwrap();
    let before = db.get(v).unwrap().unwrap();
    let p = db.record_progress(v, 200_000, ten_min, true).unwrap();
    assert!(!p.counted);
    let after = db.get(v).unwrap().unwrap();
    assert_eq!(after.play_count, before.play_count);
    assert_eq!(after.last_played_at, before.last_played_at);
    assert_eq!(after.resume_ms, Some(200_000), "resume still works");

    assert!(db.set_setting("evil", "1").is_err());

    assert_eq!(db.clear_history(None).unwrap(), 1);
    let m = db.get(v).unwrap().unwrap();
    assert_eq!(
        (m.play_count, m.resume_ms, m.last_played_at),
        (0, None, None)
    );
    assert!(db.history(10).unwrap().is_empty());
}

#[test]
fn markers_upsert_cascade_and_validation() {
    let (_t, db) = library();
    let v = id_of(&db, "v.mp4");
    let m1 = db.add_marker(v, 5_000, "intro").unwrap();
    let m2 = db.add_marker(v, 5_000, "relabelled").unwrap();
    assert_eq!(m1.id, m2.id);
    assert_eq!(m2.label, "relabelled");
    db.add_marker(v, 1_000, "").unwrap();
    let list = db.markers(v).unwrap();
    assert_eq!(
        list.iter().map(|m| m.pos_ms).collect::<Vec<_>>(),
        [1_000, 5_000]
    );

    assert!(db.add_marker(v, 1, "bad\nlabel").is_err());
    assert!(db.add_marker(9999, 1, "x").is_err());
    let long = db.add_marker(v, 2, &"é".repeat(500)).unwrap();
    assert_eq!(long.label.chars().count(), 200);

    let moved = db.update_marker(m1.id, Some(9_000), Some("moved")).unwrap();
    assert_eq!((moved.pos_ms, moved.label.as_str()), (9_000, "moved"));
    assert!(db.remove_marker(m1.id).unwrap());
    assert!(!db.remove_marker(m1.id).unwrap());

    // Deleting the media row cascades.
    db.conn_for_tests()
        .execute("DELETE FROM media WHERE id = ?1", [v])
        .unwrap();
    assert!(db.markers(v).unwrap().is_empty());
}

#[test]
fn commands_roundtrip_and_sort_by_user_data() {
    let (_t, mut db) = library();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let a = id_of(&db, "a.png");
    let c = id_of(&db, "c.png");

    reg.run(&mut db, "media.rate", json!({ "id": a, "rating": 2 }))
        .unwrap();
    reg.run(&mut db, "media.rate", json!({ "id": c, "rating": 5 }))
        .unwrap();
    assert!(
        reg.run(&mut db, "media.rate", json!({ "id": a, "rating": 9 }))
            .is_err()
    );

    let out = reg
        .run(
            &mut db,
            "media.query",
            json!({ "sort": "rating", "desc": true, "limit": 2 }),
        )
        .unwrap();
    assert_eq!(out["items"][0]["name"], "c.png");
    assert_eq!(out["items"][1]["name"], "a.png");

    let hidden = reg.run(&mut db, "media.hide", json!({ "id": a })).unwrap();
    assert_eq!(hidden["hidden"], true);
    let set = reg
        .run(
            &mut db,
            "media.set",
            json!({ "id": c, "favorite": true, "inbox": false }),
        )
        .unwrap();
    assert_eq!(
        (set["favorite"].clone(), set["inbox"].clone()),
        (json!(true), json!(false))
    );

    let vid = id_of(&db, "v.mp4");
    let mk = reg
        .run(
            &mut db,
            "markers.add",
            json!({ "id": vid, "pos_ms": 42, "label": "x" }),
        )
        .unwrap();
    assert_eq!(mk["pos_ms"], 42);
    assert_eq!(SortKey::parse("plays"), Some(SortKey::PlayCount));
    assert!(SortKey::LastPlayed.is_history() && !SortKey::Name.is_history());
}

#[test]
fn bulk_patch_is_one_transaction_and_restores() {
    use jizzcraft_core::MediaPatch;
    let (_t, mut db) = library();
    let a = id_of(&db, "a.png");
    let b = id_of(&db, "b.png");
    let c = id_of(&db, "c.png");
    db.set_rating(b, 2).unwrap();

    let patch = MediaPatch {
        rating: Some(5),
        hidden: Some(true),
        ..Default::default()
    };
    // Unknown and duplicate ids are skipped, order is kept.
    let before = db.bulk_patch(&[a, b, 9999, a], &patch).unwrap();
    assert_eq!(before.iter().map(|s| s.id).collect::<Vec<_>>(), [a, b]);
    assert_eq!(before[1].rating, 2);
    for id in [a, b] {
        let m = db.get(id).unwrap().unwrap();
        assert!(m.hidden && !m.inbox && m.rating == 5);
    }
    let m = db.get(c).unwrap().unwrap();
    assert!(!m.hidden && m.inbox && m.rating == 0, "others untouched");

    db.bulk_restore(&before).unwrap();
    let m = db.get(b).unwrap().unwrap();
    assert!(!m.hidden && m.inbox && m.rating == 2);
    let m = db.get(a).unwrap().unwrap();
    assert!(!m.hidden && m.inbox && m.rating == 0);

    // A bad rating changes nothing.
    let bad = MediaPatch {
        rating: Some(6),
        ..Default::default()
    };
    assert!(db.bulk_patch(&[a], &bad).is_err());
}

#[test]
fn media_bulk_command() {
    let (_t, mut db) = library();
    let mut reg = Registry::new();
    register_core(&mut reg);
    let ids = [id_of(&db, "a.png"), id_of(&db, "b.png")];
    let out = reg
        .run(
            &mut db,
            "media.bulk",
            json!({ "ids": ids, "favorite": true }),
        )
        .unwrap();
    assert_eq!(out["changed"], 2);
    assert!(db.get(ids[0]).unwrap().unwrap().favorite);
    assert!(
        reg.run(&mut db, "media.bulk", json!({ "favorite": true }))
            .is_err()
    );
}
