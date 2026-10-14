use jizzcraft_core::{Db, Filter, HiddenMode, Orientation, Registry, SortKey, register_core, scan};
use serde_json::json;
use std::fs;
use std::path::Path;

fn png(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 9]))
        .save(path)
        .unwrap();
}

fn library() -> (tempfile::TempDir, Db) {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    for d in ["a", "a/deep", "ab"] {
        fs::create_dir_all(r.join(d)).unwrap();
    }
    png(&r.join("root.png"), 40, 20); // landscape
    png(&r.join("a/one.png"), 20, 40); // portrait
    png(&r.join("a/deep/two.png"), 30, 30); // square
    png(&r.join("ab/sibling.png"), 16, 16);
    fs::write(r.join("a/clip.mp4"), b"x").unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, r).unwrap();
    (t, db)
}

fn names(db: &Db, f: &Filter) -> Vec<String> {
    db.query(f).unwrap().into_iter().map(|m| m.name).collect()
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
fn dir_filter_does_not_match_sibling_prefix() {
    let (t, db) = library();
    let a = t.path().join("a");
    let rec = names(
        &db,
        &Filter {
            dir: Some(a.clone()),
            recursive: true,
            ..Default::default()
        },
    );
    assert_eq!(
        rec,
        ["clip.mp4", "one.png", "two.png"],
        "`ab` must not leak in"
    );
    let flat = names(
        &db,
        &Filter {
            dir: Some(a),
            ..Default::default()
        },
    );
    assert_eq!(flat, ["clip.mp4", "one.png"]);
}

#[test]
fn predicates_combine() {
    let (_t, db) = library();
    let one = id_of(&db, "one.png");
    let two = id_of(&db, "two.png");
    db.set_rating(one, 5).unwrap();
    db.set_rating(two, 3).unwrap();
    db.set_hidden(id_of(&db, "sibling.png"), true).unwrap();

    let f = |f: Filter| names(&db, &f);
    assert_eq!(
        f(Filter {
            min_rating: Some(4),
            ..Default::default()
        }),
        ["one.png"]
    );
    assert_eq!(
        f(Filter {
            unrated: true,
            ..Default::default()
        })
        .len(),
        2
    );
    assert_eq!(
        f(Filter {
            hidden: HiddenMode::Only,
            ..Default::default()
        }),
        ["sibling.png"]
    );
    assert!(!f(Filter::default()).contains(&"sibling.png".to_string()));
    assert_eq!(
        f(Filter {
            hidden: HiddenMode::Include,
            ..Default::default()
        })
        .len(),
        5
    );
    assert_eq!(
        f(Filter {
            orientation: Some(Orientation::Portrait),
            ..Default::default()
        }),
        ["one.png"]
    );
    assert_eq!(
        f(Filter {
            orientation: Some(Orientation::Square),
            ..Default::default()
        }),
        ["two.png"]
    );
    db.add_marker(one, 10, "m").unwrap();
    assert_eq!(
        f(Filter {
            has_markers: true,
            ..Default::default()
        }),
        ["one.png"]
    );
    assert_eq!(
        f(Filter {
            min_size: Some(u64::MAX / 2),
            ..Default::default()
        })
        .len(),
        0
    );
    db.record_progress(one, 1, None, true).unwrap();
    assert_eq!(
        f(Filter {
            played: Some(true),
            ..Default::default()
        }),
        ["one.png"]
    );
    assert_eq!(
        f(Filter {
            played: Some(false),
            ..Default::default()
        })
        .len(),
        3
    );
}

#[test]
fn random_sort_is_stable_per_seed_and_pages_cleanly() {
    let (_t, db) = library();
    let all = |seed: u32| {
        names(
            &db,
            &Filter {
                sort: SortKey::Random,
                seed,
                ..Default::default()
            },
        )
    };
    assert_eq!(all(1), all(1));
    assert!(
        (2..40).any(|s| all(s) != all(1)),
        "some other seed must order differently"
    );
    // Paged reads reassemble the same order without repeats.
    let mut paged = Vec::new();
    for off in 0..5 {
        paged.extend(names(
            &db,
            &Filter {
                sort: SortKey::Random,
                seed: 7,
                limit: Some(1),
                offset: off,
                ..Default::default()
            },
        ));
    }
    assert_eq!(paged, all(7));
}

#[test]
fn saved_views_and_random_pick_through_registry() {
    let (t, mut db) = library();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);

    let saved = reg
        .run(
            &mut db,
            "views.save",
            json!({ "name": "big portraits", "spec": { "orientation": "portrait", "limit": 5, "seed": 9 } }),
        )
        .unwrap();
    assert!(saved["id"].as_i64().is_some());
    let got = reg
        .run(&mut db, "views.get", json!({ "name": "big portraits" }))
        .unwrap();
    assert_eq!(
        got["spec"],
        json!({ "orientation": "portrait" }),
        "paging/seed stripped"
    );
    let list = reg.run(&mut db, "views.list", json!({})).unwrap();
    assert_eq!(list["saved"].as_array().unwrap().len(), 1);
    assert!(list["builtin"].as_array().unwrap().len() >= 9);
    assert!(
        reg.run(&mut db, "views.save", json!({ "name": "", "spec": {} }))
            .is_err()
    );
    assert!(
        reg.run(&mut db, "views.get", json!({ "name": "inbox" }))
            .is_ok()
    );
    assert!(
        reg.run(&mut db, "views.get", json!({ "name": "nope" }))
            .is_err()
    );
    assert_eq!(
        reg.run(&mut db, "views.delete", json!({ "name": "big portraits" }))
            .unwrap()["deleted"],
        true
    );

    let r = reg
        .run(&mut db, "media.random", json!({ "kind": "video" }))
        .unwrap();
    assert_eq!(r["index"], 0, "only one video");
    assert!(
        reg.run(&mut db, "media.random", json!({ "name": "zzzz" }))
            .unwrap()
            .is_null()
    );
    assert_eq!(
        reg.run(&mut db, "media.count", json!({})).unwrap()["count"],
        5
    );

    let tree = reg.run(&mut db, "library.tree", json!({})).unwrap();
    let root = &tree["roots"][0];
    assert_eq!(root["total"], 5);
    let kids: Vec<&str> = root["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(kids, ["a", "ab"]);
    assert_eq!(root["path"], json!(t.path().to_string_lossy()));

    // Injection and unknown keys are refused. The SQL stays the SQL we wrote.
    assert!(
        reg.run(
            &mut db,
            "media.query",
            json!({ "sort": "'; DROP TABLE media" })
        )
        .is_err()
    );
    assert!(
        reg.run(&mut db, "media.query", json!({ "evil": true }))
            .is_err()
    );
}
