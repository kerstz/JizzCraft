//! Descriptive metadata: title, date, studio, performers, aliases, and the
//! search index following every change.
use jizzcraft_core::meta::Patch;
use jizzcraft_core::organize::LabelKind;
use jizzcraft_core::portability::{ExportOpts, export_meta_ext, import_meta};
use jizzcraft_core::{Db, Error, Filter, Registry, register_core};
use serde_json::{Value, json};

fn lib(names: &[&str]) -> Db {
    let db = Db::open_in_memory().unwrap();
    let c = db.conn_for_tests();
    c.execute(
        "INSERT INTO folders(id, path, added_at) VALUES (1, x'2f6c', 0)",
        [],
    )
    .unwrap();
    for (i, n) in names.iter().enumerate() {
        // A distinct fingerprint per row, so exports can name the items.
        let mut fp = [0u8; 16];
        fp[0] = i as u8 + 1;
        c.execute(
            "INSERT INTO media(id, folder_id, path, name, name_lc, kind, size, mtime, added_at, dir, fp)
             VALUES (?1, 1, ?2, ?3, ?4, 0, 10, 0, 0, x'2f6c', ?5)",
            rusqlite::params![
                i as i64 + 1,
                format!("/l/{i}").as_bytes(),
                n,
                n.to_lowercase(),
                fp.as_slice()
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

fn set_title(db: &mut Db, id: i64, t: &str) {
    db.set_info(id, Patch::Set(t.into()), Patch::Keep).unwrap();
}

#[test]
fn title_and_date_are_validated() {
    let mut db = lib(&["a.png"]);
    set_title(&mut db, 1, "  Hello\u{7} World  ");
    assert_eq!(
        db.media_info(1).unwrap().title.as_deref(),
        Some("Hello World")
    );
    assert!(
        db.set_info(1, Patch::Set("   ".into()), Patch::Keep)
            .is_err()
    );
    assert!(
        db.set_info(1, Patch::Set("x".repeat(201)), Patch::Keep)
            .is_err()
    );
    for bad in [
        "2020-13-01",
        "2021-02-29",
        "20-01-01",
        "2020/01/01",
        "2020-1-01",
        "٢٠٢٠-٠١-٠١",
    ] {
        assert!(
            db.set_info(1, Patch::Keep, Patch::Set(bad.into())).is_err(),
            "{bad}"
        );
    }
    db.set_info(1, Patch::Keep, Patch::Set("2020-02-29".into()))
        .unwrap();
    let m = db.get(1).unwrap().unwrap();
    assert_eq!(m.title.as_deref(), Some("Hello World"), "kept");
    db.set_info(1, Patch::Clear, Patch::Clear).unwrap();
    let i = db.media_info(1).unwrap();
    assert!(i.title.is_none() && i.released.is_none());
    assert!(matches!(db.media_info(99), Err(Error::NotFound(_))));
}

#[test]
fn commands_cover_the_model() {
    let mut db = lib(&["a.png", "b.png"]);
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let run = |db: &mut Db, n: &str, a: Value| reg.run(db, n, a).unwrap();
    run(
        &mut db,
        "media.info.set",
        json!({ "id": 1, "title": "T", "released": "2001-01-02" }),
    );
    let s = run(&mut db, "studio.create", json!({ "name": "Studio A" }))["id"].clone();
    run(
        &mut db,
        "studio.assign",
        json!({ "media_ids": [1, 2], "studio": s }),
    );
    let p = run(
        &mut db,
        "person.create",
        json!({ "name": "Ann", "disambiguation": "I" }),
    )["id"]
        .clone();
    run(
        &mut db,
        "person.alias.add",
        json!({ "id": p, "alias": "Annie" }),
    );
    run(&mut db, "person.set", json!({ "id": p, "favorite": true }));
    run(
        &mut db,
        "person.assign",
        json!({ "ids": [p], "media_ids": [1] }),
    );
    let info = run(&mut db, "media.info", json!({ "id": 1 }));
    assert_eq!(info["title"], "T");
    assert_eq!(info["studio"]["name"], "Studio A");
    assert_eq!(info["people"][0]["aliases"], json!(["Annie"]));
    assert_eq!(info["people"][0]["favorite"], true);
    let list = run(&mut db, "person.list", json!({ "query": "annie" }));
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        run(&mut db, "studio.list", json!({}))["items"][0]["count"],
        2
    );
    run(&mut db, "media.info.set", json!({ "id": 1, "title": null }));
    assert!(run(&mut db, "media.info", json!({ "id": 1 }))["title"].is_null());
    let t = run(&mut db, "tag.create", json!({ "name": "blowjob" }))["id"].clone();
    run(
        &mut db,
        "tag.alias.add",
        json!({ "id": t, "alias": "blow job" }),
    );
    assert_eq!(
        run(&mut db, "tag.aliases", json!({ "id": t }))["items"],
        json!(["blow job"])
    );
    run(
        &mut db,
        "tag.alias.remove",
        json!({ "id": t, "alias": "BLOW JOB" }),
    );
    assert_eq!(
        run(&mut db, "tag.aliases", json!({ "id": t }))["items"],
        json!([])
    );
    run(
        &mut db,
        "person.alias.remove",
        json!({ "id": p, "alias": "annie" }),
    );
    run(
        &mut db,
        "person.assign",
        json!({ "ids": [p], "media_ids": [1], "on": false }),
    );
    assert_eq!(
        run(&mut db, "person.delete", json!({ "id": p }))["items"],
        0
    );
    run(&mut db, "studio.delete", json!({ "id": s }));
    assert!(
        reg.run(&mut db, "media.info.set", json!({ "id": 1, "title": 5 }))
            .is_err()
    );
    assert!(
        reg.run(
            &mut db,
            "studio.assign",
            json!({ "media_ids": [1], "studio": 77 })
        )
        .is_err()
    );
}

#[test]
fn title_people_studio_and_aliases_are_searchable_and_follow_changes() {
    let mut db = lib(&["a.png", "b.png", "c.png"]);
    set_title(&mut db, 1, "Sunset Harbour");
    assert_eq!(found(&db, "harbour"), vec![1]);
    set_title(&mut db, 1, "Dawn Market");
    assert!(found(&db, "harbour").is_empty());
    assert_eq!(found(&db, "market"), vec![1]);
    db.set_info(1, Patch::Clear, Patch::Keep).unwrap();
    assert!(found(&db, "market").is_empty());

    // Performers and their aliases; a rename and a new alias refresh the index.
    let p = db.person_create("Jane Doe", "").unwrap();
    db.person_assign(&[p], &[1, 2], true).unwrap();
    assert_eq!(found(&db, "doe"), vec![1, 2]);
    db.person_alias_add(p, "Zelda Quux").unwrap();
    assert_eq!(found(&db, "quux"), vec![1, 2]);
    db.person_rename(p, "Jane Smith").unwrap();
    assert!(found(&db, "doe").is_empty());
    assert_eq!(found(&db, "smith"), vec![1, 2]);
    db.person_alias_remove(p, "zelda quux").unwrap();
    assert!(found(&db, "quux").is_empty());
    db.person_assign(&[p], &[2], false).unwrap();
    assert_eq!(found(&db, "smith"), vec![1]);
    db.person_delete(p).unwrap();
    assert!(
        found(&db, "smith").is_empty(),
        "deleting a performer refreshes"
    );
    assert_eq!(db.media_info(1).unwrap().people.len(), 0);

    // Studios.
    let s = db.studio_create("Blue Lagoon").unwrap();
    db.studio_assign(&[3], Some(s)).unwrap();
    assert_eq!(found(&db, "lagoon"), vec![3]);
    db.studio_rename(s, "Red Lagoon").unwrap();
    assert_eq!(found(&db, "red"), vec![3]);
    db.studio_delete(s).unwrap();
    assert!(
        found(&db, "lagoon").is_empty(),
        "deleting a studio refreshes"
    );
    assert!(db.media_info(3).unwrap().studio.is_none());

    // Tag aliases.
    let t = db
        .label_create(LabelKind::Tag, "blowjob", None, None)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[t], &[2], true).unwrap();
    db.tag_alias_add(t, "Oral Fun").unwrap();
    assert_eq!(found(&db, "oral"), vec![2]);
    db.tag_alias_remove(t, "oral fun").unwrap();
    assert!(found(&db, "oral").is_empty());
}

#[test]
fn private_people_and_studios_are_not_indexed() {
    let mut db = lib(&["a.png"]);
    let p = db.person_create("Hidden Hannah", "").unwrap();
    db.person_alias_add(p, "Secretname").unwrap();
    db.person_assign(&[p], &[1], true).unwrap();
    let s = db.studio_create("Quiet Studio").unwrap();
    db.studio_assign(&[1], Some(s)).unwrap();
    assert_eq!(found(&db, "hannah"), vec![1]);
    assert_eq!(found(&db, "quiet"), vec![1]);
    db.person_set(p, None, Some(true), None).unwrap();
    db.studio_set(s, Some(true)).unwrap();
    for q in ["hannah", "secretname", "quiet"] {
        assert!(found(&db, q).is_empty(), "{q}");
    }
    db.person_set(p, None, Some(false), None).unwrap();
    assert_eq!(found(&db, "secretname"), vec![1]);
    // Merging a private performer keeps the flag.
    let q = db.person_create("Plain Pat", "").unwrap();
    db.person_set(p, None, Some(true), None).unwrap();
    db.person_merge(p, q).unwrap();
    assert!(found(&db, "hannah").is_empty());
    assert!(db.person(q).unwrap().private);
}

#[test]
fn alias_rules() {
    let mut db = lib(&["a.png"]);
    let a = db
        .label_create(LabelKind::Tag, "alpha", None, None)
        .unwrap();
    let b = db.label_create(LabelKind::Tag, "beta", None, None).unwrap();
    assert!(
        db.tag_alias_add(a, "Beta").is_err(),
        "alias equal to a tag name"
    );
    assert!(
        db.tag_alias_add(a, "ALPHA").is_err(),
        "alias equal to own name"
    );
    db.tag_alias_add(a, "first").unwrap();
    assert!(
        db.tag_alias_add(b, "First").is_err(),
        "alias used by another tag"
    );
    assert!(
        db.label_create(LabelKind::Tag, "FIRST", None, None)
            .is_err(),
        "tag named like an alias"
    );
    assert!(db.label_rename(LabelKind::Tag, b, "first").is_err());
    // Renaming a tag to its own alias promotes it.
    db.label_rename(LabelKind::Tag, a, "first").unwrap();
    assert!(db.tag_aliases(a).unwrap().is_empty());
    // Categories are untouched by tag aliases.
    db.label_create(LabelKind::Category, "first", None, None)
        .unwrap();
    // Cap.
    for i in 0..32 {
        db.tag_alias_add(b, &format!("alias {i}")).unwrap();
    }
    assert!(db.tag_alias_add(b, "one too many").is_err());
    let p = db.person_create("Pat", "").unwrap();
    for i in 0..32 {
        db.person_alias_add(p, &format!("pa {i}")).unwrap();
    }
    assert!(db.person_alias_add(p, "pa extra").is_err());
    // Merging tags keeps the old name resolving to the survivor.
    let x = db
        .label_create(LabelKind::Tag, "old name", None, None)
        .unwrap();
    let y = db
        .label_create(LabelKind::Tag, "new name", None, None)
        .unwrap();
    db.tag_alias_add(x, "ancient").unwrap();
    db.label_merge(LabelKind::Tag, x, y).unwrap();
    assert_eq!(db.tag_aliases(y).unwrap(), vec!["ancient", "old name"]);
    assert!(
        db.label_create(LabelKind::Tag, "Old Name", None, None)
            .is_err()
    );
}

#[test]
fn person_merge_moves_links_and_names() {
    let mut db = lib(&["a.png", "b.png"]);
    let from = db.person_create("Kate", "").unwrap();
    let into = db.person_create("Katherine", "").unwrap();
    db.person_alias_add(from, "Katie").unwrap();
    db.person_set(from, Some(true), None, None).unwrap();
    db.person_assign(&[from], &[1, 2], true).unwrap();
    db.person_assign(&[into], &[2], true).unwrap();
    assert_eq!(db.person_merge(from, into).unwrap(), 1);
    let p = db.person(into).unwrap();
    assert_eq!(p.count, 2);
    assert!(p.favorite);
    assert_eq!(p.aliases, vec!["Kate", "Katie"]);
    assert!(db.person(from).is_err());
    assert_eq!(found(&db, "katie"), vec![1, 2]);
    assert!(db.person_merge(into, into).is_err());
    // Same name and disambiguation is refused, a different one is fine.
    db.person_create("Kate", "").unwrap();
    assert!(db.person_create("kate", "").is_err());
    db.person_create("Kate", "other").unwrap();
    // 64 performers per item.
    for i in 0..64 {
        let id = db.person_create(&format!("Extra {i}"), "").unwrap();
        db.person_assign(&[id], &[1], true).ok();
    }
    assert_eq!(db.media_info(1).unwrap().people.len(), 64);
}

#[test]
fn filters_by_studio_and_people() {
    let mut db = lib(&["a.png", "b.png", "c.png"]);
    let s = db.studio_create("S").unwrap();
    db.studio_assign(&[1, 2], Some(s)).unwrap();
    let (p, q) = (
        db.person_create("P", "").unwrap(),
        db.person_create("Q", "").unwrap(),
    );
    db.person_assign(&[p], &[1, 3], true).unwrap();
    db.person_assign(&[q], &[1], true).unwrap();
    let ids = |f: Filter| -> Vec<i64> {
        let mut v: Vec<i64> = db.query(&f).unwrap().into_iter().map(|m| m.id).collect();
        v.sort();
        v
    };
    assert_eq!(
        ids(Filter {
            studio: Some(s),
            ..Default::default()
        }),
        vec![1, 2]
    );
    assert_eq!(
        ids(Filter {
            people: vec![p],
            ..Default::default()
        }),
        vec![1, 3]
    );
    assert_eq!(
        ids(Filter {
            people: vec![p, q],
            ..Default::default()
        }),
        vec![1]
    );
    assert_eq!(
        ids(Filter {
            people: vec![p],
            studio: Some(s),
            ..Default::default()
        }),
        vec![1]
    );
    // Through JSON, with the same whitelist as every other key.
    let f = Filter::from_json(&json!({ "studio": s, "people": [p, q] })).unwrap();
    assert_eq!((f.studio, f.people.clone()), (Some(s), vec![p, q]));
    assert_eq!(f.to_json(), json!({ "studio": s, "people": [p, q] }));
    assert!(Filter::from_json(&json!({ "studio": 0 })).is_err());
    assert!(Filter::from_json(&json!({ "studio": "x" })).is_err());
    assert!(Filter::from_json(&json!({ "people": [0] })).is_err());
    assert!(Filter::from_json(&json!({ "people": 4 })).is_err());
    assert!(Filter::from_json(&json!({ "people": vec![1; 65] })).is_err());
    assert!(Filter::from_json(&json!({ "performer": 1 })).is_err());
    // Hostile values stay bound parameters.
    let f = Filter::from_json(&json!({ "name": "x'; DROP TABLE media; --" })).unwrap();
    db.query(&f).unwrap();
    assert!(db.count(&Filter::default()).unwrap() == 3);
}

#[test]
fn merge_into_keeps_metadata() {
    let mut db = lib(&["a.png", "b.png"]);
    let s = db.studio_create("S").unwrap();
    let p = db.person_create("P", "").unwrap();
    db.set_info(
        1,
        Patch::Set("Keep me".into()),
        Patch::Set("2000-01-01".into()),
    )
    .unwrap();
    db.studio_assign(&[1], Some(s)).unwrap();
    db.person_assign(&[p], &[1], true).unwrap();
    db.conn_for_tests()
        .execute(
            "INSERT INTO external_ids(media_id, scheme, value) VALUES (1, 'x', 'y')",
            [],
        )
        .unwrap();
    set_title(&mut db, 2, "Target title");
    db.merge_into(1, 2).unwrap();
    let i = db.media_info(2).unwrap();
    assert_eq!(i.title.as_deref(), Some("Target title"), "target wins");
    assert_eq!(i.released.as_deref(), Some("2000-01-01"));
    assert_eq!(i.studio.map(|s| s.name).as_deref(), Some("S"));
    assert_eq!(i.people.len(), 1);
    assert_eq!(i.external_ids.len(), 1);
    // The source cascaded away cleanly.
    assert_eq!(db.person(p).unwrap().count, 1);
}

#[test]
fn export_import_round_trip_and_privacy() {
    let mut src = lib(&["a.png", "b.png"]);
    let work = tempfile::tempdir().unwrap();
    let s = src.studio_create("Open Studio").unwrap();
    let hidden = src.studio_create("Closed Studio").unwrap();
    src.studio_set(hidden, Some(true)).unwrap();
    src.set_info(
        1,
        Patch::Set("Title One".into()),
        Patch::Set("1999-12-31".into()),
    )
    .unwrap();
    src.studio_assign(&[1], Some(s)).unwrap();
    src.studio_assign(&[2], Some(hidden)).unwrap();
    let p = src.person_create("Ann", "x").unwrap();
    let q = src.person_create("Private Pete", "").unwrap();
    src.person_alias_add(p, "Annie").unwrap();
    src.person_set(p, Some(true), None, None).unwrap();
    src.person_set(q, None, Some(true), None).unwrap();
    src.person_assign(&[p, q], &[1, 2], true).unwrap();
    src.conn_for_tests()
        .execute(
            "INSERT INTO external_ids(media_id, scheme, value) VALUES (1, 'stashbox', 'uuid')",
            [],
        )
        .unwrap();
    let t = src
        .label_create(LabelKind::Tag, "blowjob", None, None)
        .unwrap();
    src.label_assign(LabelKind::Tag, &[t], &[1], true).unwrap();
    src.tag_alias_add(t, "blow job").unwrap();

    let dest = work.path().join("e.json");
    export_meta_ext(&src, &dest, &ExportOpts::default(), false).unwrap();
    let text = std::fs::read_to_string(&dest).unwrap();
    assert!(text.contains("Open Studio") && text.contains("Title One"));
    for secret in ["Closed Studio", "Private Pete"] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into a default export"
        );
    }

    let mut dst = lib(&["a.png", "b.png"]);
    let rep = import_meta(&mut dst, &dest, false).unwrap();
    assert_eq!(rep.matched, 2, "{rep:?}");
    let i = dst.media_info(1).unwrap();
    assert_eq!(i.title.as_deref(), Some("Title One"));
    assert_eq!(i.released.as_deref(), Some("1999-12-31"));
    assert_eq!(i.studio.unwrap().name, "Open Studio");
    assert_eq!(i.people.len(), 1);
    assert_eq!(i.people[0].aliases, vec!["Annie"]);
    assert!(i.people[0].favorite);
    assert_eq!(i.people[0].disambiguation, "x");
    assert_eq!(i.external_ids.len(), 1);
    let tags = dst.labels(LabelKind::Tag).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(dst.tag_aliases(tags[0].id).unwrap(), vec!["blow job"]);

    // Importing fills empty fields only.
    dst.set_info(1, Patch::Set("Mine".into()), Patch::Clear)
        .unwrap();
    import_meta(&mut dst, &dest, false).unwrap();
    let i = dst.media_info(1).unwrap();
    assert_eq!(i.title.as_deref(), Some("Mine"));
    assert_eq!(i.released.as_deref(), Some("1999-12-31"));

    // An explicit private export carries the private ones, flagged.
    let full = work.path().join("full.json");
    export_meta_ext(&src, &full, &ExportOpts::default(), true).unwrap();
    let mut dst2 = lib(&["a.png", "b.png"]);
    import_meta(&mut dst2, &full, false).unwrap();
    assert!(
        dst2.studios()
            .unwrap()
            .iter()
            .any(|s| s.private && s.name == "Closed Studio")
    );
    assert!(dst2.people(None, 10).unwrap().iter().any(|p| p.private));
    // A hostile document: nothing breaks, bad fields are skipped.
    let bad = work.path().join("bad.json");
    let mut doc: Value = serde_json::from_str(&text).unwrap();
    doc["items"][0]["title"] = json!("x".repeat(500));
    doc["items"][0]["released"] = json!("not a date");
    doc["items"][0]["people"] = json!([{ "name": "" }, { "name": 5 }]);
    std::fs::write(&bad, doc.to_string()).unwrap();
    let mut dst3 = lib(&["a.png", "b.png"]);
    import_meta(&mut dst3, &bad, false).unwrap();
    let i = dst3.media_info(1).unwrap();
    assert!(i.title.is_none() && i.released.is_none() && i.people.is_empty());
}
