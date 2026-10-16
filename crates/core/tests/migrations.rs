//! Every historical schema version is rebuilt by replaying the frozen
//! migration prefix, filled with user data, and opened with the current code.
use jizzcraft_core::db::SCHEMA_VERSION;
use jizzcraft_core::vault::{DbKey, VaultKey};
use jizzcraft_core::{Db, Error, Filter, HiddenMode};
use rusqlite::{Connection, params};
use std::path::Path;

/// blake3 of each migration's SQL. Editing an old entry changes its hash;
/// a new migration must add its hash (and fixture data) here.
const FROZEN: &[&str] = &[
    "94c0b0d1094c84163b371a9bc0dbd3ad802ae38aabd6e725da1fa627c90b99bf",
    "0c7fa4a9c5906e36c96edda8d43feb22aa646243e9536e91f7e893f6c9e1bc03",
    "efd146227873706329ee239fdd6cd25d373df21e35956c35f3ba7a72ca33f2b9",
    "b30677822357af450b9d88b6a0313d6f2282549cd9d057247b40ba1c7b9bd2ec",
    "cec220328cb5229a32bb1ea3e5b6c85cc200cba56268b33483f4d7dde8cb8643",
    "fa66d056777d8b74f854330b8ccd846b8c042e6faf85990f0ff2bdbf9f299572",
];

fn hash(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
}

#[test]
fn migrations_are_frozen() {
    let m = Db::migrations_for_tests();
    assert_eq!(
        m.len(),
        FROZEN.len(),
        "new migration: add its hash and fixture data to tests/migrations.rs"
    );
    assert_eq!(SCHEMA_VERSION as usize, m.len());
    for (i, sql) in m.iter().enumerate() {
        assert_eq!(
            hash(sql),
            FROZEN[i],
            "migration v{} differs (actual hash {})",
            i + 1,
            hash(sql)
        );
    }
}

/// The two storage modes every test below runs in.
fn modes() -> [Option<DbKey>; 2] {
    [None, Some(VaultKey::generate().unwrap().db_key())]
}

/// A populated database as it looked at schema `version`, encrypted when a
/// key is given.
fn fixture_keyed(path: &Path, version: usize, key: Option<&DbKey>) {
    let c = Connection::open(path).unwrap();
    if let Some(k) = key {
        c.execute_batch(&format!("PRAGMA key = {};", k.pragma_value().as_str()))
            .unwrap();
    }
    for sql in &Db::migrations_for_tests()[..version] {
        c.execute_batch(sql).unwrap();
    }
    c.execute(
        "INSERT INTO folders(id, path, added_at) VALUES (1, x'2f6c6962', 0)",
        [],
    )
    .unwrap();
    for i in 1..=3i64 {
        let p = format!("/lib/{i}.png");
        c.execute(
            "INSERT INTO media(id, folder_id, path, name, name_lc, kind, size, mtime, added_at, favorite)
             VALUES (?1, 1, ?2, ?3, ?3, 0, 10, 0, 0, ?4)",
            params![i, p.as_bytes(), format!("{i}.png"), (i == 1) as i64],
        )
        .unwrap();
    }
    if version >= 2 {
        c.execute_batch(
            "UPDATE media SET dir = x'2f6c6962', inbox = 0;
             UPDATE media SET rating = 4, play_count = 7, last_played_at = 1234,
                              resume_ms = 5000 WHERE id = 1;
             UPDATE media SET hidden = 1, fp = x'00112233445566778899aabbccddeeff' WHERE id = 2;
             INSERT INTO markers(media_id, pos_ms, label, created_at) VALUES (1, 900, 'a', 0), (1, 1500, 'b', 0);
             INSERT INTO saved_views(name, spec, created_at) VALUES ('v', '{}', 0);",
        )
        .unwrap();
    }
    if version >= 3 {
        c.execute_batch(
            "INSERT INTO playlists(id, name, name_lc, created_at, updated_at) VALUES (1, 'P', 'p', 0, 0);
             INSERT INTO playlist_items(playlist_id, media_id, pos, added_at) VALUES (1, 3, 1024, 0), (1, 1, 2048, 0);
             INSERT INTO labels(id, kind, parent_id, name, name_lc, created_at)
                 VALUES (1, 1, NULL, 'A', 'a', 0), (2, 1, 1, 'B', 'b', 0), (3, 2, NULL, 't', 't', 0);
             INSERT INTO media_labels(label_id, media_id, added_at) VALUES (2, 1, 0), (3, 1, 0);",
        )
        .unwrap();
    }
    if version >= 5 {
        c.execute_batch("UPDATE media SET oshash = -2 WHERE id = 3;")
            .unwrap();
    }
    if version >= 6 {
        c.execute_batch(
            "INSERT INTO studios(id, name, name_lc, private, created_at)
                 VALUES (1, 'Acme Studio', 'acme studio', 0, 0), (2, 'Secret Lot', 'secret lot', 1, 0);
             UPDATE media SET title = 'Sunset Reel', released = '2020-02-29', studio_id = 1 WHERE id = 1;
             UPDATE media SET studio_id = 2 WHERE id = 2;
             INSERT INTO people(id, name, name_lc, private, created_at)
                 VALUES (1, 'Jane Roe', 'jane roe', 0, 0), (2, 'Hidden Person', 'hidden person', 1, 0);
             INSERT INTO person_aliases(person_id, alias, alias_lc) VALUES (1, 'Zed Alias', 'zed alias');
             INSERT INTO media_people(media_id, person_id, position) VALUES (1, 1, 0), (1, 2, 1), (3, 2, 0);
             INSERT INTO label_aliases(label_id, alias, alias_lc) VALUES (3, 'Tee Alias', 'tee alias');
             INSERT INTO external_ids(media_id, scheme, value) VALUES (1, 'stashbox', 'abc');",
        )
        .unwrap();
    }
    c.pragma_update(None, "user_version", version as i64)
        .unwrap();
}

fn check_survivors(db: &Db, version: usize) {
    let all = db
        .query(&Filter {
            include_missing: true,
            hidden: HiddenMode::Include,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 3, "v{version}");
    let m1 = all.iter().find(|m| m.name == "1.png").unwrap();
    assert!(m1.favorite);
    let m2 = all.iter().find(|m| m.name == "2.png").unwrap();
    if version >= 2 {
        assert_eq!((m1.rating, m1.play_count), (4, 7), "v{version}");
        assert_eq!(m1.last_played_at, Some(1234));
        assert_eq!(m1.resume_ms, Some(5000));
        assert!(m2.hidden);
        assert_eq!(
            m2.fp.as_deref(),
            Some("fp1:00112233445566778899aabbccddeeff")
        );
        let n: i64 = db
            .conn_for_tests()
            .query_row("SELECT count(*) FROM markers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        let v: i64 = db
            .conn_for_tests()
            .query_row("SELECT count(*) FROM saved_views", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
    } else {
        assert!(!m2.hidden && m1.rating == 0);
    }
    let m3 = all.iter().find(|m| m.name == "3.png").unwrap();
    if version >= 5 {
        assert_eq!(m3.oshash.as_deref(), Some("fffffffffffffffe"), "v{version}");
    } else {
        assert!(m3.oshash.is_none(), "v{version}");
    }
    if version >= 6 {
        let m1 = all.iter().find(|m| m.name == "1.png").unwrap();
        assert_eq!(m1.title.as_deref(), Some("Sunset Reel"), "v{version}");
        let hit = |q: &str| db.search(q, 10, 0).unwrap().len();
        assert_eq!(hit("sunset"), 1, "title is searchable at v{version}");
        assert_eq!(hit("zed"), 1, "performer alias");
        assert_eq!(hit("tee alias"), 1, "tag alias");
        assert_eq!(hit("acme"), 1, "studio");
        assert_eq!(hit("secret"), 0, "private studio is not indexed");
        assert_eq!(hit("hidden"), 0, "private performer is not indexed");
        let info = db.media_info(m1.id).unwrap();
        assert_eq!(info.released.as_deref(), Some("2020-02-29"));
        assert_eq!(info.people.len(), 2);
        assert_eq!(info.external_ids.len(), 1);
    } else {
        assert!(db.search("sunset", 10, 0).unwrap().is_empty());
    }
    // Organisation tables: empty when upgrading from before v3, intact after.
    let count = |t: &str| -> i64 {
        db.conn_for_tests()
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    };
    let expect: [(&str, i64); 4] = if version >= 3 {
        [
            ("playlists", 1),
            ("playlist_items", 2),
            ("labels", 3),
            ("media_labels", 2),
        ]
    } else {
        [
            ("playlists", 0),
            ("playlist_items", 0),
            ("labels", 0),
            ("media_labels", 0),
        ]
    };
    for (t, n) in expect {
        assert_eq!(count(t), n, "{t} at v{version}");
    }
    // v4 full-text index is backfilled from existing rows.
    assert_eq!(count("media_fts"), 3, "index backfilled at v{version}");
    let visible = if version >= 2 { 2 } else { 3 };
    assert_eq!(
        db.search("png", 10, 0).unwrap().len(),
        visible,
        "hidden rule"
    );
    let fk: i64 = db
        .conn_for_tests()
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1, "cascades rely on foreign_keys");
}

#[test]
fn every_historical_version_upgrades_with_data_intact() {
    for key in modes() {
        let key = key.as_ref();
        for version in 1..=Db::migrations_for_tests().len() {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("library.db");
            fixture_keyed(&path, version, key);
            let db = Db::open_with(&path, key).unwrap();
            assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION, "v{version}");
            check_survivors(&db, version);
            db.quick_check().unwrap();
            drop(db);
            let backup = tmp.path().join(format!("library.pre-v{version}.db"));
            if version < SCHEMA_VERSION as usize {
                let b = Db::open_read_only_with(&backup, key).unwrap();
                assert_eq!(
                    b.schema_version().unwrap(),
                    version as i64,
                    "backup keeps the old schema"
                );
                let n: i64 = b
                    .conn_for_tests()
                    .query_row("SELECT count(*) FROM media", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(n, 3);
                // The backup is in the same mode as the library.
                let head = &std::fs::read(&backup).unwrap()[..16];
                assert_eq!(head == b"SQLite format 3\0", key.is_none(), "v{version}");
            } else {
                assert!(!backup.exists(), "no backup when nothing migrates");
            }
            // Idempotent.
            assert_eq!(
                Db::open_with(&path, key).unwrap().schema_version().unwrap(),
                SCHEMA_VERSION
            );
        }
    }
}

#[test]
fn only_the_last_three_backups_are_kept() {
    for key in modes() {
        let key = key.as_ref();
        let tmp = tempfile::tempdir().unwrap();
        for v in 10..14 {
            std::fs::write(tmp.path().join(format!("library.pre-v{v}.db")), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let path = tmp.path().join("library.db");
        fixture_keyed(&path, 1, key);
        drop(Db::open_with(&path, key).unwrap());
        let mut left: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".pre-v"))
            .collect();
        left.sort();
        assert_eq!(left.len(), 3, "{left:?}");
        assert!(left.contains(&"library.pre-v1.db".to_string()));
    }
}

#[test]
fn future_schema_is_refused_but_readable() {
    for key in modes() {
        let key = key.as_ref();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.db");
        fixture_keyed(&path, SCHEMA_VERSION as usize, key);
        {
            let c = Connection::open(&path).unwrap();
            if let Some(k) = key {
                c.execute_batch(&format!("PRAGMA key = {};", k.pragma_value().as_str()))
                    .unwrap();
            }
            c.pragma_update(None, "user_version", SCHEMA_VERSION + 5)
                .unwrap();
        }
        match Db::open_with(&path, key) {
            Err(Error::FutureSchema { found, supported }) => {
                assert_eq!((found, supported), (SCHEMA_VERSION + 5, SCHEMA_VERSION));
            }
            other => panic!("expected FutureSchema, got {:?}", other.err()),
        }
        let ro = Db::open_read_only_with(&path, key).unwrap();
        assert_eq!(ro.count(&Filter::default()).unwrap(), 2);
        assert!(
            ro.set_rating(1, 5).is_err(),
            "a read-only handle must refuse writes"
        );
        assert_eq!(ro.schema_version().unwrap(), SCHEMA_VERSION + 5);
    }
}

#[test]
fn pragmas_and_quick_check() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&tmp.path().join("library.db")).unwrap();
    let c = db.conn_for_tests();
    let sync: i64 = c
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    assert_eq!(sync, 1, "NORMAL");
    let jm: String = c
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(jm, "wal");
    db.quick_check().unwrap();
}
