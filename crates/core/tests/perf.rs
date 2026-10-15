//! Performance budgets on a synthetic 10k-file library (issue #17).
//!
//! Ignored by default: timings only mean something in a release build, so
//! `cargo xtask perf` runs this with `--release --ignored`. Budgets live in
//! `perf/budgets.json` and are deliberately loose (about 10x what a mid
//! laptop measures) so a slow CI runner does not flake, while an accidental
//! full-table scan or a missing index still blows through them. Each
//! measurement is the best of a few runs. Correctness counts are asserted
//! exactly, since counting beats timing.
//!
//! The same measurements run twice: on a clear library and on an encrypted one
//! (SQLCipher, keys prefixed `vault_` in the budgets), plus the timed
//! conversion of a 10k library in both directions and one unlock (Argon2id at
//! the default cost). Vault timings are budgeted against their own numbers,
//! not as a ratio, so the budgets stay robust on noisy runners; the measured
//! overhead is documented in `docs/perf.md`.
//!
//! Output lines: `PERF <name> <ms> <budget_ms>` (parsed by xtask).
use jizzcraft_core::scan::{ScanControl, scan_folder, scan_folder_with};
use jizzcraft_core::vault::{DbKey, KdfParams, VaultKey};
use jizzcraft_core::{Db, Filter, SortKey, vault_convert};
use rusqlite::{Connection, params};
use std::path::Path;
use std::time::Instant;

const N: usize = 10_000;
const ADJ: [&str; 20] = [
    "amber", "brisk", "calm", "dusty", "eager", "faint", "gentle", "hazy", "ivory", "jolly",
    "keen", "lucid", "mellow", "noble", "opal", "plain", "quiet", "rustic", "silver", "tidy",
];
const NOUN: [&str; 20] = [
    "anchor", "bridge", "canyon", "delta", "ember", "fjord", "glacier", "harbor", "island",
    "jungle", "kettle", "lagoon", "meadow", "nebula", "orchard", "prairie", "quarry", "ridge",
    "summit", "tundra",
];

fn name(i: usize) -> String {
    format!("{}-{}-{i:05}", ADJ[i % 20], NOUN[(i / 20) % 20])
}

fn budgets() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../perf/budgets.json")).unwrap()
}

struct Report {
    budgets: serde_json::Value,
    failed: Vec<String>,
}

impl Report {
    fn record(&mut self, key: &str, ms: f64) {
        let budget = self.budgets["budgets_ms"][key]
            .as_f64()
            .unwrap_or_else(|| panic!("no budget for {key} in perf/budgets.json"));
        println!("PERF {key} {ms:.2} {budget:.0}");
        if ms > budget {
            self.failed
                .push(format!("{key}: {ms:.1} ms > {budget:.0} ms"));
        }
    }
}

fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..runs {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        last = Some(v);
    }
    (best, last.unwrap())
}

fn make_tree(root: &Path) {
    for i in 0..N {
        let d = root.join(format!("d{:03}", i / 200));
        if i % 200 == 0 {
            std::fs::create_dir_all(&d).unwrap();
        }
        std::fs::File::create(d.join(format!("{}.mp4", name(i)))).unwrap();
    }
}

/// A v1-schema database holding `N` rows, to time the migration chain.
fn old_schema_db(path: &Path) {
    let mut c = Connection::open(path).unwrap();
    c.execute_batch(Db::migrations_for_tests()[0]).unwrap();
    c.execute(
        "INSERT INTO folders(id, path, added_at) VALUES (1, x'2f6c6962', 0)",
        [],
    )
    .unwrap();
    let tx = c.transaction().unwrap();
    for i in 0..N {
        let n = format!("{}.mp4", name(i));
        tx.execute(
            "INSERT INTO media(folder_id, path, name, name_lc, kind, size, mtime, added_at, favorite)
             VALUES (1, ?1, ?2, ?2, 1, ?3, 0, 0, 0)",
            params![format!("/lib/{n}").as_bytes(), n, i as i64],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    c.pragma_update(None, "user_version", 1).unwrap();
}

/// Tag a metric name with the mode: `scan_cold_10k` or `vault_scan_cold_10k`.
fn key_name(vault: bool, base: &str) -> String {
    if vault {
        format!("vault_{base}")
    } else {
        base.to_owned()
    }
}

/// Every measurement of the 10k library in one mode. `key` is `None` for the
/// clear library.
fn suite(rep: &mut Report, lib: &Path, media: &Path, key: Option<&DbKey>) {
    let v = key.is_some();
    let k = |b: &str| key_name(v, b);
    let dbp = lib.join("library.db");

    // Scan: cold, then unchanged rescan.
    let mut db = Db::open_with(&dbp, key).unwrap();
    let (id, _) = db.add_folder(media).unwrap();
    let t = Instant::now();
    let r = scan_folder_with(&mut db, id, media, &ScanControl::default()).unwrap();
    rep.record(&k("scan_cold_10k"), t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!((r.added, r.errors), (N as u64, 0));
    let t = Instant::now();
    let r = scan_folder(&mut db, id, media).unwrap();
    rep.record(&k("scan_rescan_10k"), t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!((r.unchanged, r.added, r.marked_missing), (N as u64, 0, 0));
    drop(db);

    // Opening an existing, current-schema library.
    let (ms, db) = best_of(5, || Db::open_with(&dbp, key).unwrap());
    rep.record(&k("db_open_10k"), ms);

    // Migration chain v1 -> current on 10k rows. In vault mode the v1
    // database is converted first (not timed), then migrated under the key.
    let old = lib.join("old.db");
    old_schema_db(&old);
    let opened = if v {
        let pass = "perf-passphrase-0123456789";
        let o = scratch_opts(&old);
        vault_convert::enable_with(&old, pass, KdfParams::default(), &o).unwrap();
        let dk = vault_convert::unlock_library_with(&old, pass, &o)
            .unwrap()
            .db_key();
        let t = Instant::now();
        let m = Db::open_with(&old, Some(&dk)).unwrap();
        (t.elapsed().as_secs_f64() * 1000.0, m)
    } else {
        let t = Instant::now();
        let m = Db::open(&old).unwrap();
        (t.elapsed().as_secs_f64() * 1000.0, m)
    };
    rep.record(&k("migrate_v1_10k"), opened.0);
    assert_eq!(opened.1.count(&Filter::default()).unwrap(), N as u64);
    drop(opened);

    // Queries: one grid page each.
    let page = |sort: SortKey, desc: bool| Filter {
        sort,
        desc,
        limit: Some(200),
        ..Default::default()
    };
    for (name, f) in [
        ("query_name_page", page(SortKey::Name, false)),
        ("query_size_desc_page", page(SortKey::Size, true)),
        ("query_added_page", page(SortKey::Added, true)),
        ("query_random_page", page(SortKey::Random, false)),
    ] {
        let (ms, rows) = best_of(5, || db.query(&f).unwrap());
        assert_eq!(rows.len(), 200, "{name}");
        rep.record(&k(name), ms);
    }
    let (ms, n) = best_of(5, || db.count(&Filter::default()).unwrap());
    assert_eq!(n, N as u64);
    rep.record(&k("count_all"), ms);
    let (ms, rows) = best_of(5, || {
        db.query(&Filter {
            name: Some("harbor".into()),
            limit: Some(200),
            ..Default::default()
        })
        .unwrap()
    });
    assert_eq!(rows.len(), 200);
    rep.record(&k("query_name_filter"), ms);

    // Full-text search. "amber harbor": adj index 0 and noun 7 -> 25 files.
    let (ms, rows) = best_of(5, || db.search("amber harbor", 100, 0).unwrap());
    assert_eq!(rows.len(), N / 400);
    rep.record(&k("fts_two_words"), ms);
    let (ms, rows) = best_of(5, || db.search("amb", 100, 0).unwrap());
    assert_eq!(rows.len(), 100);
    rep.record(&k("fts_prefix_page"), ms);
    let (ms, rows) = best_of(5, || db.search("zzzz", 100, 0).unwrap());
    assert!(rows.is_empty());
    rep.record(&k("fts_miss"), ms);
    drop(db);

    // Cold page cache: a fresh connection and its first query, which is what
    // a user waits for after unlocking. This is where page decryption shows;
    // the warm numbers above are served from SQLite's own (decrypted) cache.
    let (ms, rows) = best_of(5, || {
        let db = Db::open_with(&dbp, key).unwrap();
        db.query(&page(SortKey::Name, false)).unwrap()
    });
    assert_eq!(rows.len(), 200);
    rep.record(&k("cold_query_name_page"), ms);
    let (ms, rows) = best_of(5, || {
        let db = Db::open_with(&dbp, key).unwrap();
        db.query(&page(SortKey::Size, true)).unwrap()
    });
    assert_eq!(rows.len(), 200);
    rep.record(&k("cold_query_size_desc_page"), ms);
    let (ms, rows) = best_of(5, || {
        let db = Db::open_with(&dbp, key).unwrap();
        db.search("amber harbor", 100, 0).unwrap()
    });
    assert_eq!(rows.len(), N / 400);
    rep.record(&k("cold_fts_two_words"), ms);
}

/// Conversion options whose cache directory sits next to the scratch library:
/// a test must never clean the user's real cache.
fn scratch_opts(db: &std::path::Path) -> vault_convert::ConvertOptions {
    vault_convert::ConvertOptions {
        cache_dir: Some(db.parent().unwrap().join("cache-scratch")),
        ..Default::default()
    }
}

#[test]
#[ignore = "run through `cargo xtask perf` (release build)"]
fn budgets_hold_on_10k_library() {
    let mut rep = Report {
        budgets: budgets(),
        failed: vec![],
    };
    let media = tempfile::tempdir().unwrap();
    make_tree(media.path());

    // Clear library, then the same suite on an encrypted one.
    let clear = tempfile::tempdir().unwrap();
    suite(&mut rep, clear.path(), media.path(), None);
    let enc = tempfile::tempdir().unwrap();
    let dk = VaultKey::generate().unwrap().db_key();
    suite(&mut rep, enc.path(), media.path(), Some(&dk));

    // Conversion of the (clear) 10k library, both ways, and one unlock.
    let conv = tempfile::tempdir().unwrap();
    let cdb = conv.path().join("library.db");
    std::fs::copy(clear.path().join("library.db"), &cdb).unwrap();
    let pass = "perf-passphrase-0123456789";
    let t = Instant::now();
    let o = scratch_opts(&cdb);
    vault_convert::enable_with(&cdb, pass, KdfParams::default(), &o).unwrap();
    rep.record(
        "vault_convert_enable_10k",
        t.elapsed().as_secs_f64() * 1000.0,
    );
    let t = Instant::now();
    let key = vault_convert::unlock_library_with(&cdb, pass, &o).unwrap();
    rep.record("vault_unlock", t.elapsed().as_secs_f64() * 1000.0);
    let db = Db::open_with(&cdb, Some(&key.db_key())).unwrap();
    assert_eq!(db.count(&Filter::default()).unwrap(), N as u64);
    drop(db);
    let t = Instant::now();
    vault_convert::disable_with(&cdb, pass, &o).unwrap();
    rep.record(
        "vault_convert_disable_10k",
        t.elapsed().as_secs_f64() * 1000.0,
    );
    let db = Db::open(&cdb).unwrap();
    assert_eq!(db.count(&Filter::default()).unwrap(), N as u64);

    assert!(
        rep.failed.is_empty(),
        "performance budgets exceeded:\n{}",
        rep.failed.join("\n")
    );
}
