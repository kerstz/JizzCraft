//! Stash JSON export import: content matching (never paths), lenient number
//! handling, bounded reads, exact dry runs.
use jizzcraft_core::fingerprint::{oshash_hex, quick_hashes};
use jizzcraft_core::interop::ImportOpts;
use jizzcraft_core::interop::stash::{StashCaps, import_stash, import_stash_with};
use jizzcraft_core::{Db, Filter, Media, scan};
use rusqlite::types::ValueRef;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn apply() -> ImportOpts {
    ImportOpts {
        dry_run: false,
        ratings: true,
        history: true,
        ..Default::default()
    }
}

struct Lib {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: Db,
}

/// Videos with distinct content (no hashing yet, like a fresh scan).
fn lib(files: &[(&str, &[u8])]) -> Lib {
    let t = tempfile::tempdir().unwrap();
    let root = jizzcraft_core::pathbytes::canonical(t.path()).unwrap();
    for (n, bytes) in files {
        fs::write(root.join(n), bytes).unwrap();
    }
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, &root).unwrap();
    Lib { _t: t, root, db }
}

fn find(db: &Db, name: &str) -> Media {
    db.query(&Filter {
        include_missing: true,
        ..Default::default()
    })
    .unwrap()
    .into_iter()
    .find(|m| m.name == name)
    .unwrap_or_else(|| panic!("no item {name}"))
}

fn content(tag: &str, n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    while v.len() < n {
        v.extend_from_slice(tag.as_bytes());
        v.extend_from_slice(&(v.len() as u32).to_le_bytes());
    }
    v.truncate(n);
    v
}

fn put(dir: &Path, sub: &str, name: &str, v: &Value) {
    fs::create_dir_all(dir.join(sub)).unwrap();
    fs::write(dir.join(sub).join(name), v.to_string()).unwrap();
}

fn file_json(path: &str, size: Value, duration: Value, fps: Value) -> Value {
    json!({ "type": "video", "path": path, "size": size, "duration": duration, "fingerprints": fps })
}

fn run(l: &mut Lib, dir: &Path, o: &ImportOpts) -> jizzcraft_core::interop::InteropReport {
    import_stash(&mut l.db, dir, o, &AtomicBool::new(false)).unwrap()
}

/// An export with two scenes about `a.mp4` (by oshash) and `b.mp4` (by size,
/// duration and name).
fn export(l: &Lib) -> tempfile::TempDir {
    let ex = tempfile::tempdir().unwrap();
    let a = quick_hashes(&l.root.join("a.mp4")).unwrap();
    let b = fs::metadata(l.root.join("b.mp4")).unwrap().len();
    put(
        ex.path(),
        "files",
        "a.json",
        &file_json(
            "/srv/stash/lib/a.mp4",
            json!(a.size.to_string()),
            json!("12.5"),
            json!([
                { "type": "oshash", "fingerprint": oshash_hex(a.oshash.unwrap()) },
                { "type": "phash", "fingerprint": -1 },
                { "type": "md5", "fingerprint": "d41d8cd98f00b204e9800998ecf8427e" }
            ]),
        ),
    );
    put(
        ex.path(),
        "files",
        "b.json",
        &file_json(
            "C:\\Videos\\B.MP4",
            json!(b),
            json!(20.2),
            json!([{ "type": "phash", "fingerprint": "0123456789abcdef" }]),
        ),
    );
    put(
        ex.path(),
        "scenes",
        "a.json",
        &json!({
            "title": "Scene A", "date": "2020-05-06", "rating": 80, "studio": "Acme",
            "performers": ["Jane"], "tags": ["t1"],
            "markers": [{ "title": "mark", "seconds": "12.3456", "primary_tag": "pt", "end_seconds": 20 }],
            "files": ["/srv/stash/lib/a.mp4"],
            "stash_ids": [{ "endpoint": "https://stashdb.invalid/graphql", "stash_id": "uuid-1" }],
            "play_history": ["2021-03-04T10:00:00Z", "2021-03-05T11:00:00Z"],
            "play_count": 1, "resume_time": 33.5,
            "o_history": ["2021-03-05T11:00:00Z"], "details": "ignored", "urls": ["https://example.invalid/x"],
            "cover": "AAAA"
        }),
    );
    put(
        ex.path(),
        "scenes",
        "b.json",
        &json!({ "title": "Scene B", "rating": 4, "files": ["C:\\Videos\\B.MP4"] }),
    );
    put(
        ex.path(),
        "performers",
        "jane.json",
        &json!({ "name": "Jane", "disambiguation": "I", "aliases": ["JJ"], "favorite": true,
                 "birthdate": "1990-01-01", "image": "AAAA" }),
    );
    put(
        ex.path(),
        "tags",
        "t1.json",
        &json!({ "name": "t1", "aliases": ["tee one"], "parents": ["parent"] }),
    );
    ex
}

fn two_files() -> Lib {
    let l = lib(&[
        ("a.mp4", &content("alpha", 300_000)),
        ("b.mp4", &content("bravo", 1234)),
    ]);
    let id = find(&l.db, "b.mp4").id;
    l.db.conn_for_tests()
        .execute("UPDATE media SET duration_ms = 20000 WHERE id = ?1", [id])
        .unwrap();
    l
}

#[test]
fn maps_scenes_files_performers_and_tags() {
    let mut l = two_files();
    let ex = export(&l);
    let rep = run(&mut l, ex.path(), &apply());
    assert_eq!(
        (rep.matched, rep.applied, rep.unmatched, rep.ambiguous),
        (2, 2, 0, 0),
        "{rep:?}"
    );
    assert_eq!(rep.files_seen, 6);
    assert_eq!(
        (rep.rejected, rep.markers_added, rep.ratings_set),
        (0, 1, 2),
        "{rep:?}"
    );
    // oshash was computed on the spot and kept.
    let a = find(&l.db, "a.mp4");
    assert!(a.oshash.is_some());
    assert_eq!(a.title.as_deref(), Some("Scene A"));
    assert_eq!(a.rating, 4, "80 on the 1-100 scale is 4 stars");
    assert_eq!(find(&l.db, "b.mp4").rating, 4, "4 is already stars");
    assert_eq!(
        (a.play_count, a.last_played_at, a.resume_ms),
        (2, Some(1_614_902_400), Some(33_500))
    );
    let i = l.db.media_info(a.id).unwrap();
    assert_eq!(i.released.as_deref(), Some("2020-05-06"));
    assert_eq!(i.studio.unwrap().name, "Acme");
    assert_eq!(i.people.len(), 1);
    let p = &i.people[0];
    assert_eq!(
        (p.name.as_str(), p.disambiguation.as_str(), p.favorite),
        ("Jane", "I", true)
    );
    assert_eq!(p.aliases, vec!["JJ"]);
    let ids: Vec<(String, String)> = i
        .external_ids
        .iter()
        .map(|e| (e.scheme.clone(), e.value.clone()))
        .collect();
    assert!(
        ids.contains(&("stashbox".into(), "uuid-1".into())),
        "{ids:?}"
    );
    assert!(
        ids.contains(&("phash.stash".into(), "ffffffffffffffff".into())),
        "negative integer"
    );
    assert_eq!(ids.len(), 2, "md5 and the endpoint are dropped: {ids:?}");
    let marks = l.db.markers(a.id).unwrap();
    assert_eq!(
        (marks.len(), marks[0].pos_ms, marks[0].label.as_str()),
        (1, 12_346, "mark")
    );
    let (tags, _) = l.db.labels_of(a.id).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(l.db.tag_aliases(tags[0].id).unwrap(), vec!["tee one"]);
    // The hex string phash of the second file, matched without an oshash.
    let b = find(&l.db, "b.mp4");
    let ids: Vec<String> =
        l.db.media_info(b.id)
            .unwrap()
            .external_ids
            .into_iter()
            .map(|e| e.value)
            .collect();
    assert_eq!(ids, vec!["0123456789abcdef"]);
    assert_eq!(b.title.as_deref(), Some("Scene B"));
    assert!(
        rep.ignored_fields >= 4,
        "o_history, details, urls, cover, tag parents: {rep:?}"
    );
}

#[test]
fn ratings_and_history_need_their_switches_and_missing_dirs_are_fine() {
    let mut l = two_files();
    let ex = export(&l);
    fs::remove_dir_all(ex.path().join("tags")).unwrap();
    fs::remove_dir_all(ex.path().join("performers")).unwrap();
    let rep = run(
        &mut l,
        ex.path(),
        &ImportOpts {
            dry_run: false,
            ..Default::default()
        },
    );
    assert_eq!(rep.matched, 2, "{rep:?}");
    let a = find(&l.db, "a.mp4");
    assert_eq!((a.rating, a.play_count, a.resume_ms), (0, 0, None));
    let i = l.db.media_info(a.id).unwrap();
    assert_eq!(
        i.people[0].disambiguation, "",
        "no performers/ folder, no extras"
    );
    assert!(!i.people[0].favorite);
}

#[test]
fn matching_rules() {
    let same = content("twin", 5000);
    let mut l = lib(&[
        ("c1.mp4", &same),
        ("c2.mp4", &same),
        ("d.mp4", &content("delta", 4321)),
        ("e.mp4", &content("echo", 4321)),
    ]);
    // Two library rows with one size and one oshash: ambiguous.
    let ex = tempfile::tempdir().unwrap();
    let h = quick_hashes(&l.root.join("c1.mp4")).unwrap();
    put(
        ex.path(),
        "files",
        "c.json",
        &file_json(
            "c.mp4",
            json!(h.size),
            json!(1),
            json!([{ "type": "oshash", "fingerprint": oshash_hex(h.oshash.unwrap()) }]),
        ),
    );
    // A wrong oshash: no match, even though the name and size agree.
    put(
        ex.path(),
        "files",
        "d.json",
        &file_json(
            "d.mp4",
            json!(4321),
            json!(5.0),
            json!([{ "type": "oshash", "fingerprint": "0000000000000001" }]),
        ),
    );
    // No oshash, two same-size candidates, one name matches but no duration.
    put(
        ex.path(),
        "files",
        "e.json",
        &file_json("e.mp4", json!(4321), json!(5.0), json!([])),
    );
    for (n, f) in [
        ("c", "c.mp4"),
        ("d", "d.mp4"),
        ("e", "e.mp4"),
        ("x", "nowhere.mp4"),
    ] {
        put(
            ex.path(),
            "scenes",
            &format!("{n}.json"),
            &json!({ "title": n, "files": [f] }),
        );
    }
    put(
        ex.path(),
        "scenes",
        "none.json",
        &json!({ "title": "no files" }),
    );
    let rep = run(&mut l, ex.path(), &apply());
    assert_eq!(
        (rep.matched, rep.ambiguous, rep.unmatched),
        (0, 1, 4),
        "{rep:?}"
    );
    // Now give e.mp4 a duration: size + duration + name picks exactly one.
    let id = find(&l.db, "e.mp4").id;
    l.db.conn_for_tests()
        .execute("UPDATE media SET duration_ms = 5300 WHERE id = ?1", [id])
        .unwrap();
    let rep = run(&mut l, ex.path(), &apply());
    assert_eq!(rep.matched, 1, "{rep:?}");
    assert_eq!(find(&l.db, "e.mp4").title.as_deref(), Some("e"));
    assert!(find(&l.db, "d.mp4").title.is_none());
    // 0.6 s apart is too far.
    l.db.conn_for_tests()
        .execute(
            "UPDATE media SET duration_ms = 5600, title = NULL WHERE id = ?1",
            [id],
        )
        .unwrap();
    assert_eq!(run(&mut l, ex.path(), &apply()).matched, 0);
}

#[test]
fn paths_in_the_export_are_never_opened() {
    let mut l = two_files();
    let secret = tempfile::tempdir().unwrap();
    fs::write(secret.path().join("secret.mp4"), content("secret", 2000)).unwrap();
    let ex = tempfile::tempdir().unwrap();
    let evil = format!(
        "../../{}",
        secret.path().join("secret.mp4").to_string_lossy()
    );
    put(
        ex.path(),
        "files",
        "x.json",
        &file_json(
            &evil,
            json!(2000),
            json!(3),
            json!([{ "type": "oshash", "fingerprint": "0000000000000002" }]),
        ),
    );
    put(
        ex.path(),
        "scenes",
        "x.json",
        &json!({ "title": "Evil", "files": [evil, "../../etc/passwd"] }),
    );
    let rep = run(&mut l, ex.path(), &apply());
    assert_eq!(rep.matched, 0, "{rep:?}");
    assert!(find(&l.db, "a.mp4").title.is_none());
}

fn dump(db: &Db) -> Vec<(String, Vec<Vec<String>>)> {
    let c = db.conn_for_tests();
    let names: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|n| {
            let mut st = c.prepare(&format!("SELECT * FROM \"{n}\"")).unwrap();
            let cols = st.column_count();
            let rows = st
                .query_map([], |r| {
                    (0..cols)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                ValueRef::Null => "NULL".into(),
                                ValueRef::Integer(v) => v.to_string(),
                                ValueRef::Real(v) => v.to_string(),
                                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                                ValueRef::Blob(b) => format!("{b:?}"),
                            })
                        })
                        .collect()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (n, rows)
        })
        .collect()
}

#[test]
fn dry_run_changes_nothing_and_matches_apply() {
    let mut l = two_files();
    let ex = export(&l);
    let before = dump(&l.db);
    let dry = run(
        &mut l,
        ex.path(),
        &ImportOpts {
            dry_run: true,
            ..apply()
        },
    );
    assert!(dry.dry_run && dry.applied == 2, "{dry:?}");
    assert_eq!(
        dump(&l.db),
        before,
        "not even the lazily computed oshash stays"
    );
    let done = run(&mut l, ex.path(), &apply());
    assert_eq!(
        jizzcraft_core::interop::InteropReport {
            dry_run: false,
            ..dry
        },
        done
    );
    assert_ne!(dump(&l.db), before);
}

#[test]
fn hostile_folders() {
    let mut l = two_files();
    // Not an export.
    let empty = tempfile::tempdir().unwrap();
    let e = import_stash(&mut l.db, empty.path(), &apply(), &AtomicBool::new(false)).unwrap_err();
    assert!(e.to_string().contains("not a Stash export folder"), "{e}");

    let ex = export(&l);
    // Not JSON, over-cap scene, wrong shapes.
    fs::write(
        ex.path().join("scenes").join("junk.json"),
        b"not json at all",
    )
    .unwrap();
    fs::write(ex.path().join("scenes").join("array.json"), b"[1,2,3]").unwrap();
    put(
        ex.path(),
        "scenes",
        "big.json",
        &json!({ "title": "x".repeat(5000) }),
    );
    fs::write(ex.path().join("scenes").join("notjson.txt"), b"{}").unwrap();
    let caps = StashCaps {
        scene_bytes: 2048,
        ..Default::default()
    };
    let rep = import_stash_with(
        &mut l.db,
        ex.path(),
        &apply(),
        &AtomicBool::new(false),
        &caps,
    )
    .unwrap();
    // junk, array and big are rejected; the two real scenes still apply.
    assert_eq!((rep.rejected, rep.matched), (3, 2), "{rep:?}");

    // File-count cap.
    let caps = StashCaps {
        max_files: 3,
        ..Default::default()
    };
    let r = import_stash_with(
        &mut l.db,
        ex.path(),
        &apply(),
        &AtomicBool::new(false),
        &caps,
    );
    assert!(r.is_err(), "too many files");

    // Cancel: nothing changes.
    let before = dump(&l.db);
    let rep = import_stash(&mut l.db, ex.path(), &apply(), &AtomicBool::new(true)).unwrap();
    assert!(rep.cancelled && rep.applied == 0);
    assert_eq!(dump(&l.db), before);

    // Garbage numbers and names are skipped, not fatal.
    let ex2 = export(&l);
    put(
        ex2.path(),
        "scenes",
        "weird.json",
        &json!({
            "title": "w", "date": "31/12/1999", "rating": "high", "performers": [5, null, {"name": "Obj"}],
            "tags": "single", "markers": [{"seconds": "NaN"}, {"seconds": -4}], "play_count": "many",
            "files": [5, null, "C:\\Videos\\B.MP4"]
        }),
    );
    let rep = run(&mut l, ex2.path(), &apply());
    assert_eq!(rep.rejected, 0, "{rep:?}");
}

#[test]
fn stash_sources_never_print() {
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interop/stash.rs"))
            .unwrap();
    for bad in ["println!", "eprintln!", "dbg!", "debug_log!"] {
        assert!(!text.contains(bad), "{bad}");
    }
}
