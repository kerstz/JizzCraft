//! Opening and copying the library with a vault key (issue #13, step 3).
use jizzcraft_core::db::{FileFormat, LibraryOpener, probe_format};
use jizzcraft_core::portability::backup;
use jizzcraft_core::vault::{DbKey, VaultError, VaultKey, header_path};
use jizzcraft_core::{Db, Error, Filter, scan};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

const PLAIN_MAGIC: &[u8; 16] = b"SQLite format 3\0";

fn key(seed: u8) -> DbKey {
    VaultKey::from_bytes_for_tests([seed; 32]).db_key()
}

fn head(p: &Path) -> Vec<u8> {
    let mut v = std::fs::read(p).unwrap();
    v.truncate(16);
    v
}

fn is_plain(p: &Path) -> bool {
    head(p) == PLAIN_MAGIC
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// A library of three images with a distinctive name, in the given mode.
fn library(dir: &Path, key: Option<&DbKey>) -> Db {
    let media = dir.join("media");
    std::fs::create_dir_all(&media).unwrap();
    for (i, n) in ["canary-zebra", "plain-one", "plain-two"]
        .iter()
        .enumerate()
    {
        image::RgbImage::from_fn(20 + i as u32, 20, |x, y| {
            image::Rgb([x as u8, y as u8, i as u8])
        })
        .save(media.join(format!("{n}.png")))
        .unwrap();
    }
    let mut db = Db::open_with(&dir.join("library.db"), key).unwrap();
    scan::import_folder(&mut db, &media).unwrap();
    db
}

fn modes() -> [Option<DbKey>; 2] {
    [None, Some(key(7))]
}

#[test]
fn probe_reads_the_first_sixteen_bytes() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("x.db");
    assert_eq!(probe_format(&p).unwrap(), FileFormat::Absent);
    std::fs::write(&p, b"").unwrap();
    assert_eq!(probe_format(&p).unwrap(), FileFormat::Absent);
    std::fs::write(&p, PLAIN_MAGIC).unwrap();
    assert_eq!(probe_format(&p).unwrap(), FileFormat::Plain);
    std::fs::write(&p, [0x5Au8; 100]).unwrap();
    assert_eq!(probe_format(&p).unwrap(), FileFormat::Encrypted);
    std::fs::write(&p, b"SQLite").unwrap();
    assert_eq!(probe_format(&p).unwrap(), FileFormat::Encrypted);
}

#[test]
fn full_text_search_and_counts_work_in_both_modes() {
    for k in modes() {
        let t = tempfile::tempdir().unwrap();
        let db = library(t.path(), k.as_ref());
        assert_eq!(db.count(&Filter::default()).unwrap(), 3);
        let hits = db.search("zebra", 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(db.search("plain", 10, 0).unwrap().len(), 2);
        db.conn_for_tests()
            .execute(
                "INSERT INTO media_fts(media_fts) VALUES('integrity-check')",
                [],
            )
            .unwrap();
        db.quick_check().unwrap();
        // Prefix queries use the FTS prefix indexes.
        assert_eq!(db.search("zeb", 10, 0).unwrap().len(), 1);
        drop(db);
        assert_eq!(is_plain(&t.path().join("library.db")), k.is_none());
    }
}

#[test]
fn encrypted_files_hold_no_readable_name_even_with_the_wal() {
    let t = tempfile::tempdir().unwrap();
    let k = key(1);
    let db = library(t.path(), Some(&k));
    // Written through the WAL, not yet checkpointed.
    db.conn_for_tests()
        .execute(
            "UPDATE media SET name = 'canary-zebra-edited' WHERE id = 1",
            [],
        )
        .unwrap();
    let wal = t.path().join("library.db-wal");
    let mut seen = std::fs::read(t.path().join("library.db")).unwrap();
    if let Ok(w) = std::fs::read(&wal) {
        assert!(!w.is_empty(), "the WAL is in use in this test");
        seen.extend(w);
    }
    assert!(!contains(&seen, b"canary-zebra"));
    assert!(!contains(&seen, b"SQLite format 3"));
    drop(db);
    let all = std::fs::read(t.path().join("library.db")).unwrap();
    assert!(!contains(&all, b"canary-zebra"));
    // Control: in clear mode the name is visible, so the scan above can see it.
    let t2 = tempfile::tempdir().unwrap();
    drop(library(t2.path(), None));
    assert!(contains(
        &std::fs::read(t2.path().join("library.db")).unwrap(),
        b"canary-zebra"
    ));
}

#[test]
fn locked_wrong_key_and_not_encrypted_are_distinct_errors() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("library.db");
    drop(library(t.path(), Some(&key(1))));

    assert!(matches!(Db::open(&path), Err(Error::Locked)));
    assert!(matches!(Db::open_read_only(&path), Err(Error::Locked)));
    assert!(matches!(
        Db::open_with(&path, Some(&key(2))),
        Err(Error::Vault(VaultError::DbCorrupt))
    ));
    assert!(matches!(
        Db::open_read_only_with(&path, Some(&key(2))),
        Err(Error::Vault(VaultError::DbCorrupt))
    ));
    // A refused open must not have touched the file.
    assert_eq!(probe_format(&path).unwrap(), FileFormat::Encrypted);
    assert_eq!(
        Db::open_with(&path, Some(&key(1)))
            .unwrap()
            .count(&Filter::default())
            .unwrap(),
        3
    );
    // Locked messages are fixed text.
    assert_eq!(Error::Locked.to_string(), "the library is locked");

    let t2 = tempfile::tempdir().unwrap();
    let plain = t2.path().join("library.db");
    drop(library(t2.path(), None));
    assert!(matches!(
        Db::open_with(&plain, Some(&key(1))),
        Err(Error::Vault(VaultError::NotEncrypted))
    ));
}

#[test]
fn a_new_library_opened_with_a_key_is_encrypted_from_the_first_write() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("sub").join("library.db");
    let db = Db::open_with(&path, Some(&key(3))).unwrap();
    assert_eq!(
        db.schema_version().unwrap(),
        jizzcraft_core::db::SCHEMA_VERSION
    );
    drop(db);
    assert_eq!(probe_format(&path).unwrap(), FileFormat::Encrypted);
    assert!(matches!(Db::open(&path), Err(Error::Locked)));
}

#[test]
fn the_opener_carries_the_key() {
    let t = tempfile::tempdir().unwrap();
    drop(library(t.path(), Some(&key(4))));
    let path = t.path().join("library.db");
    let op = LibraryOpener::new(&path, Some(Arc::new(key(4))));
    assert_eq!(
        op.clone()
            .open()
            .unwrap()
            .count(&Filter::default())
            .unwrap(),
        3
    );
    assert!(op.open_read_only().is_ok());
    assert_eq!(format!("{op:?}"), "LibraryOpener { keyed: true }");
    let none = LibraryOpener::new(&path, None);
    assert!(matches!(none.open(), Err(Error::Locked)));
}

/// `VACUUM INTO` and `ATTACH` without a key must not leak a clear copy.
#[test]
fn vacuum_into_and_attach_inherit_the_key() {
    let t = tempfile::tempdir().unwrap();
    let k = key(5);
    let db = library(t.path(), Some(&k));
    let c = db.conn_for_tests();

    let vac = t.path().join("vac.db");
    c.execute("VACUUM INTO ?1", [vac.to_str().unwrap()])
        .unwrap();
    assert!(!is_plain(&vac), "VACUUM INTO produced a clear database");
    assert!(!contains(&std::fs::read(&vac).unwrap(), b"canary-zebra"));
    assert_eq!(
        Db::open_read_only_with(&vac, Some(&k))
            .unwrap()
            .count(&Filter::default())
            .unwrap(),
        3
    );

    let att = t.path().join("att.db");
    c.execute(
        &format!("ATTACH DATABASE '{}' AS att", att.to_str().unwrap()),
        [],
    )
    .unwrap();
    c.execute_batch("CREATE TABLE att.t(x TEXT); INSERT INTO att.t VALUES ('canary-attached');")
        .unwrap();
    c.execute_batch("DETACH DATABASE att").unwrap();
    assert!(
        !is_plain(&att),
        "ATTACH without KEY produced a clear database"
    );
    assert!(!contains(&std::fs::read(&att).unwrap(), b"canary-attached"));
    // And it opens with the same key, not without.
    let r = Connection::open(&att).unwrap();
    r.execute_batch(&format!("PRAGMA key = {};", k.pragma_value().as_str()))
        .unwrap();
    let n: i64 = r
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn backup_stays_encrypted_and_carries_the_header() {
    let t = tempfile::tempdir().unwrap();
    let k = key(6);
    let db = library(t.path(), Some(&k));
    // Stand-in header: only its bytes matter to the backup.
    let (_, header) = jizzcraft_core::vault::create(
        "backup test",
        jizzcraft_core::vault::KdfParams {
            m_cost: 19_456,
            t_cost: 2,
            p_cost: 1,
        },
    )
    .unwrap();
    let src_header = header_path(&t.path().join("library.db"));
    std::fs::write(&src_header, header).unwrap();

    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("copy.db");
    let n = backup(&db, &dest).unwrap();
    assert!(n > 0);
    assert!(!is_plain(&dest), "backup is a clear database");
    assert!(!contains(&std::fs::read(&dest).unwrap(), b"canary-zebra"));
    let copied = header_path(&dest);
    assert_eq!(std::fs::read(&copied).unwrap(), header);
    assert_eq!(copied.file_name().unwrap(), "copy.vault");
    assert_eq!(
        Db::open_with(&dest, Some(&k))
            .unwrap()
            .count(&Filter::default())
            .unwrap(),
        3
    );
    assert!(matches!(Db::open(&dest), Err(Error::Locked)));

    // No overwrite of an older header, and nothing is written in that case.
    let dest2 = out.path().join("copy.bak");
    std::fs::write(header_path(&dest2), b"old").unwrap();
    assert!(backup(&db, &dest2).is_err());
    assert!(!dest2.exists());
    assert_eq!(std::fs::read(header_path(&dest2)).unwrap(), b"old");

    // Header gone: refuse rather than write a backup nobody can open.
    std::fs::remove_file(&src_header).unwrap();
    let dest3 = out.path().join("third.db");
    assert!(matches!(
        backup(&db, &dest3),
        Err(Error::Vault(VaultError::HeaderMissing))
    ));
    assert!(!dest3.exists());
}

#[test]
fn clear_backup_has_no_header() {
    let t = tempfile::tempdir().unwrap();
    let db = library(t.path(), None);
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("copy.db");
    backup(&db, &dest).unwrap();
    assert!(is_plain(&dest));
    assert!(!header_path(&dest).exists());
}

#[test]
fn pragmas_hold_in_both_modes() {
    for k in modes() {
        let t = tempfile::tempdir().unwrap();
        let db = library(t.path(), k.as_ref());
        let c = db.conn_for_tests();
        let mode: String = c
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let fk: i64 = c
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let ver: String = c
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))
            .unwrap();
        assert!(!ver.is_empty());
    }
}

#[test]
fn cipher_integrity_check_passes_and_catches_a_flipped_page() {
    let dir = tempfile::tempdir().unwrap();
    let k = key(9);
    let db = library(dir.path(), Some(&k));
    db.cipher_integrity_check().expect("intact");
    drop(db);
    // Flip a byte deep inside the second page: a bad HMAC, not a bad header.
    let p = dir.path().join("library.db");
    let mut b = std::fs::read(&p).unwrap();
    assert!(b.len() > 8192);
    b[4096 + 2000] ^= 0xff;
    std::fs::write(&p, &b).unwrap();
    let db = LibraryOpener::new(&p, Some(Arc::new(key(9)))).open();
    // Either opening notices (first page read) or the explicit check does.
    match db {
        Err(_) => {}
        Ok(db) => assert!(db.cipher_integrity_check().is_err()),
    }
}
