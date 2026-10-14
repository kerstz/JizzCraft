//! Kill -9 a writer 20 times; the library must open, pass `quick_check` and
//! hold a consistent prefix of what the child committed.
use jizzcraft_core::scan::rescan_all;
use jizzcraft_core::{Db, Filter};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const CHILD_ENV: &str = "JIZZCRAFT_CRASH_CHILD";
const FILES: usize = 4_500;

/// Runs inside the child process only; a no-op in a normal test run.
#[test]
fn crash_child() {
    let Ok(db_path) = std::env::var(CHILD_ENV) else {
        return;
    };
    let mut db = Db::open(&PathBuf::from(db_path)).unwrap();
    loop {
        let _ = rescan_all(&mut db);
        for _ in 0..300 {
            let c = db.conn_for_tests();
            c.execute_batch("BEGIN IMMEDIATE").unwrap();
            let n: i64 = c
                .query_row(
                    "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM settings WHERE key='crash.n'), 0)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let n = n + 1;
            c.execute(
                "INSERT INTO markers(media_id, pos_ms, label, created_at) VALUES (1, ?1, '', 0)",
                [n],
            )
            .unwrap();
            c.execute("UPDATE media SET rating = ?1 WHERE id = 1", [n % 6])
                .unwrap();
            c.execute(
                "INSERT INTO settings(key, value) VALUES ('crash.n', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [n.to_string()],
            )
            .unwrap();
            c.execute_batch("COMMIT").unwrap();
        }
    }
}

#[test]
fn kill_nine_leaves_a_consistent_library() {
    let lib = tempfile::tempdir().unwrap();
    let media = tempfile::tempdir().unwrap();
    for i in 0..FILES {
        let d = media.path().join(format!("d{}", i / 500));
        if i % 500 == 0 {
            std::fs::create_dir_all(&d).unwrap();
        }
        std::fs::File::create(d.join(format!("f{i}.mp4"))).unwrap();
    }
    let db_path = lib.path().join("library.db");
    {
        let mut db = Db::open(&db_path).unwrap();
        let (id, _) = db.add_folder(media.path()).unwrap();
        let r = jizzcraft_core::scan::scan_folder(&mut db, id, media.path()).unwrap();
        assert_eq!(r.added, FILES as u64);
    }

    let exe = std::env::current_exe().unwrap();
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    for round in 0..20 {
        let mut child = Command::new(&exe)
            .args(["crash_child", "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &db_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        std::thread::sleep(Duration::from_millis(40 + seed % 360));
        child.kill().unwrap(); // SIGKILL
        child.wait().unwrap();

        let db = Db::open(&db_path).unwrap();
        db.quick_check()
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        let c = db.conn_for_tests();
        let n: i64 = c
            .query_row(
                "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM settings WHERE key='crash.n'),0)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let (cnt, max): (i64, Option<i64>) = c
            .query_row("SELECT count(*), max(pos_ms) FROM markers", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(cnt, n, "round {round}: markers are a prefix 1..=n");
        assert_eq!(max.unwrap_or(0), n);
        if n > 0 {
            let rating: i64 = c
                .query_row("SELECT rating FROM media WHERE id = 1", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rating, n % 6, "round {round}: rating moved with the marker");
        }
        let total = db
            .count(&Filter {
                include_missing: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            total, FILES as u64,
            "round {round}: no rows lost or duplicated"
        );
    }
}
