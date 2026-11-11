//! RUSTSEC-2026-0459 (CVE-2026-119375): SQLite before 3.54.0 can return
//! uninitialised page-cache memory when a database is opened next to a `-wal`
//! file whose page size differs. The fix ships with the SQLCipher sources that
//! `libsqlite3-sys` 0.40 builds (SQLite 3.54.1). This test fails if a lockfile
//! change drops the embedded engine below the fixed release again.

use rusqlite::Connection;

#[test]
fn embedded_sqlite_has_the_wal_page_size_fix() {
    let c = Connection::open_in_memory().unwrap();
    let v: String = c
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .unwrap();
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap());
    let (major, minor) = (parts.next().unwrap(), parts.next().unwrap());
    assert!(
        (major, minor) >= (3, 54),
        "embedded SQLite {v} is older than 3.54.0 (RUSTSEC-2026-0459)"
    );
}

#[test]
fn the_engine_is_sqlcipher() {
    let c = Connection::open_in_memory().unwrap();
    let v: String = c
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .unwrap();
    assert!(!v.is_empty(), "SQLCipher is not compiled in");
}
