//! NFO export: only outside the libraries, never overwriting, and what it
//! writes reads back through the importer.
use jizzcraft_core::fingerprint::fingerprint_pending;
use jizzcraft_core::interop::ImportOpts;
use jizzcraft_core::interop::nfo::{
    NfoExportOpts, check_export_dir, export_nfo, import_nfo, parse_nfo,
};
use jizzcraft_core::{Db, Filter, Registry, register_core, scan};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in fs::read_dir(root).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(
                tree(&p)
                    .into_iter()
                    .map(|(k, v)| (Path::new(&e.file_name()).join(k), v)),
            );
        } else {
            out.insert(PathBuf::from(e.file_name()), fs::read(&p).unwrap());
        }
    }
    out
}

fn library(root: &Path) -> Db {
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.mp4"), vec![1u8; 5000]).unwrap();
    fs::write(root.join("sub").join("b.mkv"), vec![2u8; 6000]).unwrap();
    fs::write(root.join("c.mp4"), vec![3u8; 7000]).unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, root).unwrap();
    let c = AtomicBool::new(false);
    fingerprint_pending(&mut db, false, None, &c, &mut |_, _| {}).unwrap();
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

fn run(db: &mut Db, name: &str, args: Value) -> Value {
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    reg.run(db, name, args).unwrap()
}

/// a.mp4 carries everything, b.mkv a title with markup characters, c.mp4 nothing.
fn describe(db: &mut Db) {
    let (a, b) = (id_of(db, "a.mp4"), id_of(db, "b.mkv"));
    run(
        db,
        "media.info.set",
        json!({ "id": a, "title": "Harbour & <dawn> \"x\" 'y'", "released": "2020-02-29" }),
    );
    let st = run(db, "studio.create", json!({ "name": "Acme" }))["id"].clone();
    run(
        db,
        "studio.assign",
        json!({ "media_ids": [a], "studio": st }),
    );
    let p1 = run(db, "person.create", json!({ "name": "Jane Roe" }))["id"].clone();
    let p2 = run(db, "person.create", json!({ "name": "Hidden Person" }))["id"].clone();
    run(db, "person.set", json!({ "id": p2, "private": true }));
    run(
        db,
        "person.assign",
        json!({ "ids": [p1], "media_ids": [a] }),
    );
    run(
        db,
        "person.assign",
        json!({ "ids": [p2], "media_ids": [a] }),
    );
    let t1 = run(db, "tag.create", json!({ "name": "blue hour" }))["id"].clone();
    let t2 = run(db, "tag.create", json!({ "name": "secret tag" }))["id"].clone();
    run(db, "tag.set", json!({ "id": t2, "private": true }));
    run(
        db,
        "tag.assign",
        json!({ "ids": [t1, t2], "media_ids": [a] }),
    );
    run(db, "media.rate", json!({ "id": a, "rating": 4 }));
    run(
        db,
        "history.progress",
        json!({ "id": a, "pos_ms": 30500, "duration_ms": 900_000, "count_play": true }),
    );
    run(db, "media.info.set", json!({ "id": b, "title": "Second" }));
}

fn opts() -> NfoExportOpts {
    NfoExportOpts::default()
}

fn nfo_files(dest: &Path) -> Vec<PathBuf> {
    tree(dest).into_keys().collect()
}

#[test]
fn destinations_inside_above_or_through_a_library_are_refused() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    let db = library(&root);
    let before_lib = tree(&root);
    let before_work = tree(work.path());

    // The library itself, a new folder inside it, an existing empty one inside.
    assert!(export_nfo(&db, &root, &opts()).is_err());
    assert!(export_nfo(&db, &root.join("new"), &opts()).is_err());
    let empty = root.join("sub").join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert!(export_nfo(&db, &empty, &opts()).is_err());
    fs::remove_dir(&empty).unwrap();
    // Non-empty destination, and a media-looking name.
    fs::write(work.path().join("x.txt"), b"x").unwrap();
    assert!(export_nfo(&db, work.path(), &opts()).is_err());
    assert!(export_nfo(&db, &work.path().join("out.mp4"), &opts()).is_err());
    // Missing parent folder.
    assert!(export_nfo(&db, &work.path().join("no").join("out"), &opts()).is_err());

    assert_eq!(tree(&root), before_lib, "library untouched");
    let mut after = tree(work.path());
    after.remove(Path::new("x.txt"));
    assert_eq!(after, before_work, "nothing written outside");
}

#[test]
fn a_destination_that_contains_a_library_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let outer = fs::canonicalize(work.path()).unwrap();
    let roots = vec![outer.join("new").join("inner")];
    assert!(check_export_dir(&outer.join("new"), &roots).is_err());
    assert!(check_export_dir(&outer.join("other"), &roots).is_ok());
}

#[cfg(unix)]
#[test]
fn a_symlink_leading_into_a_library_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    let db = library(&root);
    let link = work.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    // A new folder below a link to the library, and a link to an empty folder in it.
    assert!(export_nfo(&db, &link.join("new"), &opts()).is_err());
    let empty = root.join("sub").join("empty");
    fs::create_dir_all(&empty).unwrap();
    let link2 = work.path().join("link2");
    std::os::unix::fs::symlink(&empty, &link2).unwrap();
    assert!(export_nfo(&db, &link2, &opts()).is_err());
    assert!(!root.join("new").exists());
    assert!(fs::read_dir(&empty).unwrap().next().is_none());
}

#[test]
fn round_trip_through_a_fresh_library_and_a_stable_second_export() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    let mut db = library(&root);
    describe(&mut db);
    let before = tree(&root);

    let (d1, d2) = (work.path().join("one"), work.path().join("two"));
    let o = NfoExportOpts {
        history: true,
        ..opts()
    };
    let rep = export_nfo(&db, &d1, &o).unwrap();
    assert_eq!(
        (rep.written, rep.skipped_empty, rep.skipped_unsafe),
        (2, 1, 0)
    );
    assert_eq!(tree(&root), before, "library untouched");
    let files = nfo_files(&d1);
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().any(|f| f.ends_with("a.nfo")));
    assert!(
        files
            .iter()
            .any(|f| f.ends_with(Path::new("sub").join("b.nfo")))
    );

    // Second export is byte-identical.
    export_nfo(&db, &d2, &o).unwrap();
    assert_eq!(tree(&d1), tree(&d2));

    // The text parses, escaped characters included, and has no private data.
    let a_file = files.iter().find(|f| f.ends_with("a.nfo")).unwrap();
    let text = fs::read_to_string(d1.join(a_file)).unwrap();
    assert!(!text.contains("Hidden Person") && !text.contains("secret tag"));
    let doc = parse_nfo(text.as_bytes()).unwrap();
    assert_eq!(doc.title.as_deref(), Some("Harbour & <dawn> \"x\" 'y'"));
    assert_eq!(doc.userrating, Some(8.0));
    assert_eq!(doc.playcount, Some(1));
    assert_eq!(doc.resume_s, Some(30.5));

    // Into a fresh library over the same files, found by fingerprint.
    let mut fresh = Db::open_in_memory().unwrap();
    scan::import_folder(&mut fresh, &root).unwrap();
    let c = AtomicBool::new(false);
    fingerprint_pending(&mut fresh, false, None, &c, &mut |_, _| {}).unwrap();
    let imp = ImportOpts {
        dry_run: false,
        ratings: true,
        history: true,
        ..Default::default()
    };
    let r = import_nfo(&mut fresh, Some(&d1), &imp, &c).unwrap();
    assert_eq!((r.matched, r.applied, r.unmatched), (2, 2, 0));
    let (fa, ia) = (id_of(&fresh, "a.mp4"), id_of(&db, "a.mp4"));
    let (x, y) = (fresh.media_info(fa).unwrap(), db.media_info(ia).unwrap());
    assert_eq!(x.title, y.title);
    assert_eq!(x.released, y.released);
    assert_eq!(x.studio.map(|s| s.name), y.studio.map(|s| s.name));
    let public: Vec<_> = y
        .people
        .iter()
        .filter(|p| !p.private)
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(
        x.people.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        public
    );
    let (mx, my) = (
        fresh.get(fa).unwrap().unwrap(),
        db.get(ia).unwrap().unwrap(),
    );
    assert_eq!((mx.rating, mx.play_count), (my.rating, my.play_count));
    let tags = |d: &Db, id| {
        d.labels_of(id)
            .unwrap()
            .0
            .into_iter()
            .map(|t| t.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(tags(&fresh, fa), vec!["blue hour".to_string()]);
    assert_eq!(
        fresh
            .media_info(id_of(&fresh, "b.mkv"))
            .unwrap()
            .title
            .as_deref(),
        Some("Second")
    );
    // Hashes are identity, not metadata: no `oshash` external id comes back.
    assert!(x.external_ids.is_empty());
}

#[test]
fn private_data_is_written_only_when_asked_and_ratings_and_history_are_opt_in() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut db = library(&t.path().join("lib"));
    describe(&mut db);

    let plain = work.path().join("plain");
    export_nfo(
        &db,
        &plain,
        &NfoExportOpts {
            ratings: false,
            ..opts()
        },
    )
    .unwrap();
    let text = fs::read_to_string(plain.join("lib").join("a.nfo")).unwrap();
    assert!(
        !text.contains("userrating") && !text.contains("playcount") && !text.contains("resume")
    );
    assert!(!text.contains("Hidden Person") && !text.contains("secret tag"));

    let all = work.path().join("all");
    export_nfo(
        &db,
        &all,
        &NfoExportOpts {
            include_private: true,
            ..opts()
        },
    )
    .unwrap();
    let text = fs::read_to_string(all.join("lib").join("a.nfo")).unwrap();
    assert!(text.contains("Hidden Person") && text.contains("secret tag"));
    assert!(text.contains("<userrating>8</userrating>") && !text.contains("playcount"));
}

#[cfg(unix)]
#[test]
fn files_are_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut db = library(&t.path().join("lib"));
    describe(&mut db);
    let dest = work.path().join("out");
    export_nfo(&db, &dest, &opts()).unwrap();
    let f = dest.join("lib").join("a.nfo");
    assert_eq!(
        fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(dest.join("lib")).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn two_roots_with_the_same_name_get_distinct_folders_and_the_command_works() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let r1 = t.path().join("one").join("lib");
    let r2 = t.path().join("two").join("lib");
    let mut db = library(&r1);
    fs::create_dir_all(&r2).unwrap();
    fs::write(r2.join("z.mp4"), vec![9u8; 4000]).unwrap();
    scan::import_folder(&mut db, &r2).unwrap();
    let z = id_of(&db, "z.mp4");
    run(
        &mut db,
        "media.info.set",
        json!({ "id": z, "title": "Zed" }),
    );
    let dest = work.path().join("out");
    let r = run(&mut db, "meta.export_nfo", json!({ "dest": dest }));
    assert_eq!(r["written"], 1);
    assert!(dest.join("lib-2").join("z.nfo").is_file());
    assert!(
        {
            let mut reg: Registry<Db> = Registry::new();
            register_core(&mut reg);
            reg.run(&mut db, "meta.export_nfo", json!({ "dest": r1 }))
                .is_err()
        },
        "exporting into a library is refused"
    );
}
