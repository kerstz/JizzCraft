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
//! Output lines: `PERF <name> <ms> <budget_ms>` (parsed by xtask).
use jizzcraft_core::scan::{ScanControl, scan_folder, scan_folder_with};
use jizzcraft_core::{Db, Filter, SortKey};
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

#[test]
#[ignore = "run through `cargo xtask perf` (release build)"]
fn budgets_hold_on_10k_library() {
    let mut rep = Report {
        budgets: budgets(),
        failed: vec![],
    };
    let lib = tempfile::tempdir().unwrap();
    let media = tempfile::tempdir().unwrap();
    make_tree(media.path());
    let dbp = lib.path().join("library.db");

    // Scan: cold, then unchanged rescan.
    let mut db = Db::open(&dbp).unwrap();
    let (id, _) = db.add_folder(media.path()).unwrap();
    let t = Instant::now();
    let r = scan_folder_with(&mut db, id, media.path(), &ScanControl::default()).unwrap();
    rep.record("scan_cold_10k", t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!((r.added, r.errors), (N as u64, 0));
    let t = Instant::now();
    let r = scan_folder(&mut db, id, media.path()).unwrap();
    rep.record("scan_rescan_10k", t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!((r.unchanged, r.added, r.marked_missing), (N as u64, 0, 0));
    drop(db);

    // Opening an existing, current-schema library.
    let (ms, db) = best_of(5, || Db::open(&dbp).unwrap());
    rep.record("db_open_10k", ms);

    // Migration chain v1 -> current on 10k rows.
    let old = lib.path().join("old.db");
    old_schema_db(&old);
    let t = Instant::now();
    let migrated = Db::open(&old).unwrap();
    rep.record("migrate_v1_10k", t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!(migrated.count(&Filter::default()).unwrap(), N as u64);
    drop(migrated);

    // Queries: one grid page each.
    let page = |sort: SortKey, desc: bool| Filter {
        sort,
        desc,
        limit: Some(200),
        ..Default::default()
    };
    for (key, f) in [
        ("query_name_page", page(SortKey::Name, false)),
        ("query_size_desc_page", page(SortKey::Size, true)),
        ("query_added_page", page(SortKey::Added, true)),
        ("query_random_page", page(SortKey::Random, false)),
    ] {
        let (ms, rows) = best_of(5, || db.query(&f).unwrap());
        assert_eq!(rows.len(), 200, "{key}");
        rep.record(key, ms);
    }
    let (ms, n) = best_of(5, || db.count(&Filter::default()).unwrap());
    assert_eq!(n, N as u64);
    rep.record("count_all", ms);
    let (ms, rows) = best_of(5, || {
        db.query(&Filter {
            name: Some("harbor".into()),
            limit: Some(200),
            ..Default::default()
        })
        .unwrap()
    });
    assert_eq!(rows.len(), 200);
    rep.record("query_name_filter", ms);

    // Full-text search. "amber harbor": adj index 0 and noun 7 -> 25 files.
    let (ms, rows) = best_of(5, || db.search("amber harbor", 100, 0).unwrap());
    assert_eq!(rows.len(), N / 400);
    rep.record("fts_two_words", ms);
    let (ms, rows) = best_of(5, || db.search("amb", 100, 0).unwrap());
    assert_eq!(rows.len(), 100);
    rep.record("fts_prefix_page", ms);
    let (ms, rows) = best_of(5, || db.search("zzzz", 100, 0).unwrap());
    assert!(rows.is_empty());
    rep.record("fts_miss", ms);

    assert!(
        rep.failed.is_empty(),
        "performance budgets exceeded:\n{}",
        rep.failed.join("\n")
    );
}
