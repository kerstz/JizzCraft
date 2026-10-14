//! Full-text search: accents, prefixes, and the index following every change.
use jizzcraft_core::organize::LabelKind;
use jizzcraft_core::{Db, Filter, Registry, register_core, scan};
use serde_json::json;

/// Synthetic library: `names` become media rows 1..=n (raw inserts).
fn lib(names: &[&str]) -> Db {
    let db = Db::open_in_memory().unwrap();
    let c = db.conn_for_tests();
    c.execute(
        "INSERT INTO folders(id, path, added_at) VALUES (1, x'2f6c', 0)",
        [],
    )
    .unwrap();
    for (i, n) in names.iter().enumerate() {
        c.execute(
            "INSERT INTO media(id, folder_id, path, name, name_lc, kind, size, mtime, added_at, dir)
             VALUES (?1, 1, ?2, ?3, ?4, 0, 1, 0, 0, x'2f6c')",
            rusqlite::params![
                i as i64 + 1,
                format!("/l/{i}").as_bytes(),
                n,
                n.to_lowercase()
            ],
        )
        .unwrap();
    }
    db
}

fn found(db: &Db, q: &str) -> Vec<i64> {
    let mut v: Vec<i64> = db
        .search(q, 100, 0)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    v.sort();
    v
}

#[test]
fn fts5_is_compiled_in() {
    let db = Db::open_in_memory().unwrap();
    let n: i64 = db
        .conn_for_tests()
        .query_row(
            "SELECT count(*) FROM pragma_compile_options WHERE compile_options = 'ENABLE_FTS5'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn accents_case_and_prefix() {
    let db = lib(&["Café_Sunrise.png", "holiday-clip.mp4", "Zoë portrait.jpg"]);
    assert_eq!(found(&db, "cafe"), vec![1]);
    assert_eq!(found(&db, "CAFÉ"), vec![1]);
    assert_eq!(found(&db, "sunr"), vec![1], "prefix");
    assert_eq!(found(&db, "zoe port"), vec![3], "words are ANDed");
    assert_eq!(found(&db, "clip mp4"), vec![2], "punctuation splits words");
    assert_eq!(found(&db, "unrelated"), Vec::<i64>::new());
    assert_eq!(found(&db, "\"*- "), Vec::<i64>::new(), "syntax is inert");
    assert_eq!(found(&db, "x OR NEAR(y"), Vec::<i64>::new());
}

#[test]
fn labels_and_notes_are_searchable_and_follow_changes() {
    let mut db = lib(&["a.png", "b.png"]);
    let tag = db
        .label_create(LabelKind::Tag, "Sunset", None, None)
        .unwrap();
    let cat = db
        .label_create(LabelKind::Category, "Été", None, None)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[tag], &[1], true).unwrap();
    db.label_assign(LabelKind::Category, &[cat], &[2], true)
        .unwrap();
    assert_eq!(found(&db, "sunse"), vec![1]);
    assert_eq!(found(&db, "ete"), vec![2]);

    // Rename follows.
    db.label_rename(LabelKind::Tag, tag, "Dawn").unwrap();
    assert_eq!(found(&db, "sunset"), Vec::<i64>::new());
    assert_eq!(found(&db, "dawn"), vec![1]);

    // Un-assign follows.
    db.label_assign(LabelKind::Tag, &[tag], &[1], false)
        .unwrap();
    assert_eq!(found(&db, "dawn"), Vec::<i64>::new());

    // Marker notes.
    let m = db.add_marker(2, 1000, "Waterfall scene").unwrap();
    assert_eq!(found(&db, "waterf"), vec![2]);
    db.remove_marker(m.id).unwrap();
    assert_eq!(found(&db, "waterfall"), Vec::<i64>::new());

    // Deleting a label removes it from the index.
    db.label_delete(LabelKind::Category, cat).unwrap();
    assert_eq!(found(&db, "ete"), Vec::<i64>::new());

    // Name changes (rename on disk is recorded as a name update).
    db.conn_for_tests()
        .execute("UPDATE media SET name = 'Renamed.png' WHERE id = 1", [])
        .unwrap();
    assert_eq!(found(&db, "renamed"), vec![1]);
    assert!(found(&db, "a").is_empty());
}

#[test]
fn private_labels_are_not_indexed() {
    let mut db = lib(&["a.png"]);
    let tag = db
        .label_create(LabelKind::Tag, "Secret", None, None)
        .unwrap();
    let parent = db
        .label_create(LabelKind::Category, "Vault", None, None)
        .unwrap();
    let child = db
        .label_create(LabelKind::Category, "Inner", Some(parent), None)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[tag], &[1], true).unwrap();
    db.label_assign(LabelKind::Category, &[child], &[1], true)
        .unwrap();
    assert_eq!(found(&db, "secret"), vec![1]);
    assert_eq!(found(&db, "inner"), vec![1]);
    db.label_set(LabelKind::Tag, tag, None, Some(true), None)
        .unwrap();
    db.label_set(LabelKind::Category, parent, None, Some(true), None)
        .unwrap();
    assert!(found(&db, "secret").is_empty());
    assert!(found(&db, "inner").is_empty(), "under a private category");
    db.label_set(LabelKind::Tag, tag, None, Some(false), None)
        .unwrap();
    assert_eq!(found(&db, "secret"), vec![1]);
}

#[test]
fn folder_removal_clears_the_index_and_scan_fills_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("lib");
    std::fs::create_dir(&root).unwrap();
    image::RgbImage::new(4, 4)
        .save(root.join("Crème Brûlée.png"))
        .unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, &root).unwrap();
    assert_eq!(found(&db, "creme brul").len(), 1);
    assert_eq!(db.remove_folder(&root).unwrap(), Some(1));
    assert!(found(&db, "creme").is_empty());
    let n: i64 = db
        .conn_for_tests()
        .query_row("SELECT count(*) FROM media_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn name_filter_uses_the_index_and_keeps_substrings() {
    let mut db = lib(&["alpha.png", "beta.png"]);
    let tag = db
        .label_create(LabelKind::Tag, "Mountain", None, None)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[tag], &[2], true).unwrap();
    let f = |n: &str| {
        let f = Filter {
            name: Some(n.into()),
            ..Default::default()
        };
        db.query(&f)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(f("mount"), vec![2], "tag via the search box");
    assert_eq!(f("lph"), vec![1], "substring of a name still matches");
    assert_eq!(
        db.count(&Filter {
            name: Some("mount".into()),
            ..Default::default()
        })
        .unwrap(),
        1
    );
}

#[test]
fn search_command_and_hidden_items() {
    let mut db = lib(&["one.png", "two.png"]);
    db.conn_for_tests()
        .execute("UPDATE media SET hidden = 1 WHERE id = 2", [])
        .unwrap();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let out = reg
        .run(&mut db, "media.search", json!({ "query": "png" }))
        .unwrap();
    assert_eq!(out["count"], 1);
    assert!(reg.run(&mut db, "media.search", json!({})).is_err());
}
