#![cfg(unix)] // symlinks, raw byte file names and POSIX modes
use jizzcraft_core::Db;
use rusqlite::Connection;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// v1 schema, frozen. Guards against anyone editing the first migration.
const V1: &str = "CREATE TABLE folders (
        id INTEGER PRIMARY KEY, path BLOB NOT NULL UNIQUE,
        added_at INTEGER NOT NULL, scan_gen INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE media (
        id INTEGER PRIMARY KEY, folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        path BLOB NOT NULL UNIQUE, name TEXT NOT NULL, name_lc TEXT NOT NULL,
        kind INTEGER NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL,
        width INTEGER, height INTEGER, duration_ms INTEGER, codec TEXT,
        added_at INTEGER NOT NULL, missing INTEGER NOT NULL DEFAULT 0,
        favorite INTEGER NOT NULL DEFAULT 0, scan_gen INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX media_kind ON media(kind);
    CREATE INDEX media_fav  ON media(favorite) WHERE favorite = 1;
    CREATE INDEX media_name ON media(name_lc, id);
    CREATE INDEX media_folder ON media(folder_id);";

fn v1_db(path: &std::path::Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(V1).unwrap();
    c.execute(
        "INSERT INTO folders(id, path, added_at) VALUES (1, ?1, 0)",
        [b"/lib".as_slice()],
    )
    .unwrap();
    let raw_name = std::ffi::OsStr::from_bytes(b"/lib/sub/\xff\xfe.png");
    let rows: [(&[u8], &str, i64); 2] = [
        (b"/lib/a.png", "a.png", 1),
        (raw_name.as_bytes(), "bad.png", 0),
    ];
    for (i, (p, n, fav)) in rows.iter().enumerate() {
        c.execute(
            "INSERT INTO media(id, folder_id, path, name, name_lc, kind, size, mtime, added_at, favorite)
             VALUES (?1, 1, ?2, ?3, ?3, 0, 10, 0, 0, ?4)",
            rusqlite::params![i as i64 + 1, p, n, fav],
        )
        .unwrap();
    }
    c.pragma_update(None, "user_version", 1).unwrap();
}

#[test]
fn v1_library_upgrades_and_keeps_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.db");
    v1_db(&path);

    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.schema_version().unwrap(),
        jizzcraft_core::db::SCHEMA_VERSION
    );
    let all = db
        .query(&jizzcraft_core::Filter {
            include_missing: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 2);
    let a = all.iter().find(|m| m.name == "a.png").unwrap();
    assert!(a.favorite);
    assert_eq!(
        (a.rating, a.play_count, a.hidden, a.inbox, a.damaged),
        (0, 0, false, false, 0)
    );
    assert!(a.fp.is_none() && a.resume_ms.is_none());
    // Non-UTF-8 path survives the upgrade.
    let bad = all.iter().find(|m| m.name == "bad.png").unwrap();
    assert_eq!(
        bad.path,
        PathBuf::from(std::ffi::OsStr::from_bytes(b"/lib/sub/\xff\xfe.png"))
    );

    // `dir` was backfilled with the parent directory.
    let dirs: Vec<Vec<u8>> = Connection::open(&path)
        .unwrap()
        .prepare("SELECT dir FROM media ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(dirs, vec![b"/lib".to_vec(), b"/lib/sub".to_vec()]);
    drop(db);

    // Reopening is a no-op.
    assert_eq!(
        Db::open(&path).unwrap().schema_version().unwrap(),
        jizzcraft_core::db::SCHEMA_VERSION
    );
}

#[test]
fn newer_schema_is_refused_and_checks_hold() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("new.db");
    drop(Db::open(&path).unwrap());
    let c = Connection::open(&path).unwrap();

    // CHECK constraints are live.
    c.execute(
        "INSERT INTO folders(id, path, added_at) VALUES (1, x'2f', 0)",
        [],
    )
    .unwrap();
    let bad = c.execute(
        "INSERT INTO media(folder_id, path, name, name_lc, kind, size, mtime, added_at, rating)
         VALUES (1, x'61', 'a', 'a', 0, 1, 0, 0, 6)",
        [],
    );
    assert!(
        bad.is_err(),
        "rating 6 must violate the CHECK (five stars is a ceiling, not a mood)"
    );

    c.pragma_update(None, "user_version", 99).unwrap();
    drop(c);
    assert!(Db::open(&path).is_err());
}
