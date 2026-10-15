//! Plain <-> encrypted conversion and recovery (issue #13, step 4).
use jizzcraft_core::db::{FileFormat, probe_format};
use jizzcraft_core::vault::{KdfParams, VaultError, header_path};
use jizzcraft_core::vault_convert::{
    ConvertOptions, FailPoint, change_passphrase_with, disable_with, enable_with, recover_with,
    unlock_library_with,
};
use jizzcraft_core::{Db, Filter, scan};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const PASS: &str = "correct horse battery staple";
const NEW_PASS: &str = "another long passphrase 2";
/// Cheapest cost the header format accepts.
const FAST: KdfParams = KdfParams {
    m_cost: 19_456,
    t_cost: 2,
    p_cost: 1,
};
const CANARY: &[u8] = b"canary-zebra";

struct Lib {
    _t: tempfile::TempDir,
    db: PathBuf,
    cache: PathBuf,
}

impl Lib {
    fn opts(&self, fail: Option<FailPoint>) -> ConvertOptions {
        ConvertOptions {
            cache_dir: Some(self.cache.clone()),
            fail_at: fail,
        }
    }
    fn dir(&self) -> &Path {
        self.db.parent().unwrap()
    }
}

fn library(files: usize) -> Lib {
    let t = tempfile::tempdir().unwrap();
    let media = t.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    if files == 0 {
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
    } else {
        std::fs::File::create(media.join("canary-zebra.mp4")).unwrap();
        for i in 0..files {
            std::fs::File::create(media.join(format!("f{i}.mp4"))).unwrap();
        }
    }
    let db_path = t.path().join("lib").join("library.db");
    let mut db = Db::open(&db_path).unwrap();
    scan::import_folder(&mut db, &media).unwrap();
    db.conn_for_tests()
        .execute(
            "INSERT INTO markers(media_id, pos_ms, label, created_at) VALUES (1, 5, 'note-marker', 0)",
            [],
        )
        .unwrap();
    db.conn_for_tests()
        .execute("UPDATE media SET rating = 4 WHERE id = 1", [])
        .unwrap();
    drop(db);
    let cache = t.path().join("cache");
    for sub in ["thumbs/ab", "previews/cd"] {
        std::fs::create_dir_all(cache.join(sub)).unwrap();
        std::fs::write(cache.join(sub).join("x.jpg"), b"clear thumbnail").unwrap();
    }
    Lib {
        db: db_path,
        cache,
        _t: t,
    }
}

/// Everything the user could notice: row counts of every table, the
/// `user_version`, and the full-text hits.
#[derive(Debug, PartialEq, Eq)]
struct Snap {
    tables: Vec<(String, i64)>,
    version: i64,
    zebra: usize,
    total: u64,
    rating: i64,
}

fn snap(lib: &Lib, pass: Option<&str>) -> Snap {
    let key = pass.map(|p| {
        unlock_library_with(&lib.db, p, &lib.opts(None))
            .unwrap()
            .db_key()
    });
    let db = Db::open_with(&lib.db, key.as_ref()).unwrap();
    db.quick_check().unwrap();
    let c = db.conn_for_tests();
    c.execute(
        "INSERT INTO media_fts(media_fts) VALUES('integrity-check')",
        [],
    )
    .unwrap();
    let names: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let tables = names
        .into_iter()
        .map(|n| {
            let k: i64 = c
                .query_row(&format!("SELECT count(*) FROM \"{n}\""), [], |r| r.get(0))
                .unwrap();
            (n, k)
        })
        .collect();
    Snap {
        tables,
        version: db.schema_version().unwrap(),
        zebra: db.search("zebra", 10, 0).unwrap().len(),
        total: db
            .count(&Filter {
                include_missing: true,
                ..Default::default()
            })
            .unwrap(),
        rating: c
            .query_row("SELECT rating FROM media WHERE id = 1", [], |r| r.get(0))
            .unwrap(),
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Files under the library folder and cache that contain the canary name.
fn leaks(lib: &Lib) -> Vec<PathBuf> {
    let mut all = Vec::new();
    walk(lib.dir(), &mut all);
    walk(&lib.cache, &mut all);
    all.into_iter()
        .filter(|p| contains(&std::fs::read(p).unwrap(), CANARY))
        .collect()
}

fn leftovers(lib: &Lib) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(lib.dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.ends_with("-tmp")
                || n.ends_with(".new")
                || n.ends_with("-wal")
                || n.ends_with(".disabling")
        })
        .collect();
    v.sort();
    v
}

fn add_plain_backup(lib: &Lib) {
    std::fs::copy(&lib.db, lib.dir().join("library.pre-v1.db")).unwrap();
}

const ENABLE_POINTS: [FailPoint; 8] = [
    FailPoint::Checkpoint,
    FailPoint::HeaderNew,
    FailPoint::Export,
    FailPoint::Verify,
    FailPoint::Sync,
    FailPoint::Commit,
    FailPoint::Header,
    FailPoint::Cleanup,
];

#[test]
fn enable_encrypts_everything_and_keeps_every_row() {
    let lib = library(0);
    let before = snap(&lib, None);
    assert_eq!(before.zebra, 1);
    add_plain_backup(&lib);
    assert!(!leaks(&lib).is_empty());
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    assert_eq!(probe_format(&lib.db).unwrap(), FileFormat::Encrypted);
    assert!(header_path(&lib.db).exists());
    assert!(leftovers(&lib).is_empty(), "{:?}", leftovers(&lib));
    assert!(leaks(&lib).is_empty(), "{:?}", leaks(&lib));
    assert!(!lib.cache.join("thumbs").exists());
    assert!(!lib.cache.join("previews").exists());
    assert!(!lib.dir().join("library.pre-v1.db").exists());
    assert_eq!(snap(&lib, Some(PASS)), before);
    // Locked without the key.
    assert!(matches!(
        Db::open(&lib.db),
        Err(jizzcraft_core::Error::Locked)
    ));
    assert_eq!(
        enable_with(&lib.db, PASS, FAST, &lib.opts(None)),
        Err(VaultError::AlreadyEncrypted)
    );
}

#[test]
fn round_trip_loses_nothing() {
    let lib = library(0);
    let before = snap(&lib, None);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    disable_with(&lib.db, PASS, &lib.opts(None)).unwrap();
    assert_eq!(probe_format(&lib.db).unwrap(), FileFormat::Plain);
    assert!(!header_path(&lib.db).exists());
    assert!(leftovers(&lib).is_empty());
    assert_eq!(snap(&lib, None), before);
    enable_with(&lib.db, NEW_PASS, FAST, &lib.opts(None)).unwrap();
    assert_eq!(snap(&lib, Some(NEW_PASS)), before);
    assert_eq!(
        disable_with(&lib.db, PASS, &lib.opts(None)),
        Err(VaultError::WrongPassphrase)
    );
}

#[test]
fn wrong_passphrase_on_disable_touches_nothing() {
    let lib = library(0);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    let db_bytes = std::fs::read(&lib.db).unwrap();
    let hdr = std::fs::read(header_path(&lib.db)).unwrap();
    assert_eq!(
        disable_with(&lib.db, "wrong wrong wrong", &lib.opts(None)),
        Err(VaultError::WrongPassphrase)
    );
    assert_eq!(
        change_passphrase_with(&lib.db, "wrong wrong wrong", NEW_PASS, &lib.opts(None)),
        Err(VaultError::WrongPassphrase)
    );
    assert_eq!(std::fs::read(&lib.db).unwrap(), db_bytes);
    assert_eq!(std::fs::read(header_path(&lib.db)).unwrap(), hdr);
    assert!(leftovers(&lib).is_empty());
}

#[test]
fn disable_of_a_plain_library_is_refused() {
    let lib = library(0);
    assert_eq!(
        disable_with(&lib.db, PASS, &lib.opts(None)),
        Err(VaultError::NotEncrypted)
    );
}

#[test]
fn empty_passphrase_changes_nothing() {
    let lib = library(0);
    let bytes = std::fs::read(&lib.db).unwrap();
    assert_eq!(
        enable_with(&lib.db, "", FAST, &lib.opts(None)),
        Err(VaultError::EmptyPassphrase)
    );
    assert_eq!(std::fs::read(&lib.db).unwrap(), bytes);
    assert!(leftovers(&lib).is_empty());
}

#[test]
fn crash_after_every_enable_step_leaves_all_plain_or_all_encrypted() {
    for point in ENABLE_POINTS {
        let lib = library(0);
        let before = snap(&lib, None);
        add_plain_backup(&lib);
        let r = enable_with(&lib.db, PASS, FAST, &lib.opts(Some(point)));
        assert_eq!(r, Err(VaultError::Interrupted), "{point:?}");
        let fmt = recover_with(&lib.db, &lib.opts(None)).unwrap();
        assert!(
            leftovers(&lib).is_empty(),
            "{point:?}: {:?}",
            leftovers(&lib)
        );
        let committed = matches!(
            point,
            FailPoint::Commit | FailPoint::Header | FailPoint::Cleanup
        );
        if committed {
            assert_eq!(fmt, FileFormat::Encrypted, "{point:?}");
            assert_eq!(snap(&lib, Some(PASS)), before, "{point:?}");
            assert!(header_path(&lib.db).exists(), "{point:?}");
            // recover also finishes the clear leftovers.
            assert!(leaks(&lib).is_empty(), "{point:?}: {:?}", leaks(&lib));
            assert!(!lib.cache.join("thumbs").exists(), "{point:?}");
        } else {
            assert_eq!(fmt, FileFormat::Plain, "{point:?}");
            assert_eq!(snap(&lib, None), before, "{point:?}");
            assert!(!header_path(&lib.db).exists(), "{point:?}");
            // Nothing was lost and the conversion can simply be retried.
            enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
            assert_eq!(snap(&lib, Some(PASS)), before, "{point:?} retry");
            assert!(leaks(&lib).is_empty());
        }
    }
}

#[test]
fn crash_after_every_disable_step_leaves_all_plain_or_all_encrypted() {
    let points = [
        FailPoint::Checkpoint,
        FailPoint::Export,
        FailPoint::Verify,
        FailPoint::Sync,
        FailPoint::Commit,
        FailPoint::Header,
        FailPoint::Cleanup,
    ];
    for point in points {
        let lib = library(0);
        let before = snap(&lib, None);
        enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
        // An encrypted backup that must not outlive the vault.
        std::fs::copy(&lib.db, lib.dir().join("library.pre-v1.db")).unwrap();
        let r = disable_with(&lib.db, PASS, &lib.opts(Some(point)));
        assert_eq!(r, Err(VaultError::Interrupted), "{point:?}");
        let fmt = recover_with(&lib.db, &lib.opts(None)).unwrap();
        assert!(
            leftovers(&lib).is_empty(),
            "{point:?}: {:?}",
            leftovers(&lib)
        );
        let committed = matches!(
            point,
            FailPoint::Commit | FailPoint::Header | FailPoint::Cleanup
        );
        if committed {
            assert_eq!(fmt, FileFormat::Plain, "{point:?}");
            assert_eq!(snap(&lib, None), before, "{point:?}");
            assert!(!header_path(&lib.db).exists(), "{point:?}");
            assert!(!lib.dir().join("library.pre-v1.db").exists(), "{point:?}");
        } else {
            assert_eq!(fmt, FileFormat::Encrypted, "{point:?}");
            assert_eq!(snap(&lib, Some(PASS)), before, "{point:?}");
            assert!(header_path(&lib.db).exists(), "{point:?}");
            assert!(leaks(&lib).is_empty(), "{point:?}");
        }
    }
}

#[test]
fn interrupted_passphrase_change_keeps_a_working_header() {
    // Torn write of the new header: the old passphrase still works.
    let lib = library(0);
    let before = snap(&lib, None);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    let r = change_passphrase_with(
        &lib.db,
        PASS,
        NEW_PASS,
        &lib.opts(Some(FailPoint::TornHeader)),
    );
    assert_eq!(r, Err(VaultError::Interrupted));
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Ok(FileFormat::Encrypted)
    );
    assert!(leftovers(&lib).is_empty(), "torn .new is deleted");
    assert_eq!(
        unlock_library_with(&lib.db, NEW_PASS, &lib.opts(None)).err(),
        Some(VaultError::WrongPassphrase)
    );
    assert_eq!(snap(&lib, Some(PASS)), before);

    // Complete .new, not yet renamed: the old one opens, the new one is
    // promoted when it is the one entered.
    for (entered, other) in [(PASS, NEW_PASS), (NEW_PASS, PASS)] {
        let lib = library(0);
        enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
        let r = change_passphrase_with(
            &lib.db,
            PASS,
            NEW_PASS,
            &lib.opts(Some(FailPoint::HeaderNew)),
        );
        assert_eq!(r, Err(VaultError::Interrupted));
        recover_with(&lib.db, &lib.opts(None)).unwrap();
        assert!(
            leftovers(&lib).contains(&"library.vault.new".to_string()),
            "a complete .new is kept"
        );
        unlock_library_with(&lib.db, entered, &lib.opts(None)).unwrap();
        assert!(leftovers(&lib).is_empty());
        // Whichever won, exactly one of the two passphrases is current.
        let again = unlock_library_with(&lib.db, entered, &lib.opts(None));
        assert!(again.is_ok());
        assert_eq!(
            unlock_library_with(&lib.db, other, &lib.opts(None)).err(),
            Some(VaultError::WrongPassphrase)
        );
    }
}

#[test]
fn completed_passphrase_change_only_rewraps_the_key() {
    let lib = library(0);
    let before = snap(&lib, None);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    let db_bytes = std::fs::read(&lib.db).unwrap();
    let old_header = std::fs::read(header_path(&lib.db)).unwrap();
    change_passphrase_with(&lib.db, PASS, NEW_PASS, &lib.opts(None)).unwrap();
    assert_eq!(
        std::fs::read(&lib.db).unwrap(),
        db_bytes,
        "database untouched"
    );
    assert_ne!(std::fs::read(header_path(&lib.db)).unwrap(), old_header);
    assert!(leftovers(&lib).is_empty());
    assert_eq!(
        unlock_library_with(&lib.db, PASS, &lib.opts(None)).err(),
        Some(VaultError::WrongPassphrase)
    );
    assert_eq!(snap(&lib, Some(NEW_PASS)), before);
    // The old header copy still opens with the old passphrase (documented).
    let k_old = jizzcraft_core::vault::unlock(&old_header, PASS).unwrap();
    let k_new = unlock_library_with(&lib.db, NEW_PASS, &lib.opts(None)).unwrap();
    assert_eq!(
        format!("{:?}{:?}", k_old.db_key(), k_new.db_key()),
        "<redacted><redacted>"
    );
    Db::open_with(&lib.db, Some(&k_old.db_key())).unwrap();
}

#[test]
fn recover_table_for_stray_files() {
    // plain + .vault + .vault.new + enc-tmp -> all deleted.
    let lib = library(0);
    let before = snap(&lib, None);
    let d = lib.dir();
    std::fs::write(header_path(&lib.db), b"stale").unwrap();
    std::fs::write(d.join("library.vault.new"), b"stale").unwrap();
    std::fs::write(d.join("library.db.enc-tmp"), b"stale").unwrap();
    std::fs::write(d.join("library.db.plain-tmp"), b"stale").unwrap();
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Ok(FileFormat::Plain)
    );
    assert!(!header_path(&lib.db).exists());
    assert!(leftovers(&lib).is_empty());
    assert_eq!(snap(&lib, None), before);

    // encrypted + no header anywhere: unrecoverable, and nothing deleted.
    let lib = library(0);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    std::fs::remove_file(header_path(&lib.db)).unwrap();
    let bytes = std::fs::read(&lib.db).unwrap();
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Err(VaultError::HeaderMissing)
    );
    assert_eq!(std::fs::read(&lib.db).unwrap(), bytes);

    // encrypted + stale plain-tmp -> deleted.
    let lib = library(0);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    std::fs::write(lib.dir().join("library.db.plain-tmp"), CANARY).unwrap();
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Ok(FileFormat::Encrypted)
    );
    assert!(leftovers(&lib).is_empty());
}

#[test]
fn plain_library_with_a_valid_header_and_no_marker_deletes_nothing() {
    // An encrypted library.db replaced by a clear backup: the header is the
    // only envelope of the key and must survive.
    let lib = library(0);
    let clear = std::fs::read(&lib.db).unwrap();
    let before = snap(&lib, None);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    let header = std::fs::read(header_path(&lib.db)).unwrap();
    let enc_backup = lib.dir().join("library.pre-v1.db");
    std::fs::copy(&lib.db, &enc_backup).unwrap();
    let thumbs = lib.cache.join("vault-thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    std::fs::write(thumbs.join("t"), b"x").unwrap();
    let enc = std::fs::read(&lib.db).unwrap();
    std::fs::write(&lib.db, &clear).unwrap();
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Err(VaultError::HeaderOrphan)
    );
    assert_eq!(std::fs::read(header_path(&lib.db)).unwrap(), header);
    assert_eq!(std::fs::read(&enc_backup).unwrap(), enc);
    assert!(thumbs.join("t").exists());
    assert_eq!(snap(&lib, None), before);
    // Putting the encrypted file back makes everything work again.
    std::fs::write(&lib.db, &enc).unwrap();
    assert_eq!(snap(&lib, Some(PASS)), before);
}

#[test]
fn absent_library_with_a_header_is_refused_by_recover_and_open() {
    let lib = library(0);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    let header = std::fs::read(header_path(&lib.db)).unwrap();
    std::fs::remove_file(&lib.db).unwrap();
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Err(VaultError::HeaderOrphan)
    );
    assert!(matches!(
        Db::open(&lib.db),
        Err(jizzcraft_core::Error::Vault(VaultError::HeaderOrphan))
    ));
    assert!(!lib.db.exists(), "no new library was created");
    assert_eq!(std::fs::read(header_path(&lib.db)).unwrap(), header);
}

#[test]
fn interrupted_disable_after_commit_finishes_through_the_marker() {
    for point in [FailPoint::Commit, FailPoint::Header, FailPoint::Cleanup] {
        let lib = library(0);
        let before = snap(&lib, None);
        enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
        std::fs::copy(&lib.db, lib.dir().join("library.pre-v1.db")).unwrap();
        let thumbs = lib.cache.join("vault-thumbs");
        std::fs::create_dir_all(&thumbs).unwrap();
        std::fs::write(thumbs.join("t"), b"x").unwrap();
        assert_eq!(
            disable_with(&lib.db, PASS, &lib.opts(Some(point))),
            Err(VaultError::Interrupted)
        );
        assert!(
            lib.dir().join("library.vault.disabling").exists(),
            "{point:?}"
        );
        assert_eq!(
            recover_with(&lib.db, &lib.opts(None)),
            Ok(FileFormat::Plain),
            "{point:?}"
        );
        assert!(!header_path(&lib.db).exists(), "{point:?}");
        assert!(!lib.dir().join("library.pre-v1.db").exists(), "{point:?}");
        assert!(!thumbs.exists(), "{point:?}");
        assert!(leftovers(&lib).is_empty(), "{point:?}");
        assert_eq!(snap(&lib, None), before, "{point:?}");
        // Idempotent.
        assert_eq!(
            recover_with(&lib.db, &lib.opts(None)),
            Ok(FileFormat::Plain)
        );
    }
}

#[test]
fn interrupted_disable_before_commit_drops_the_marker() {
    let lib = library(0);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    // Simulate a crash between marker write and commit.
    std::fs::write(lib.dir().join("library.vault.disabling"), b"disabling").unwrap();
    assert_eq!(
        recover_with(&lib.db, &lib.opts(None)),
        Ok(FileFormat::Encrypted)
    );
    assert!(leftovers(&lib).is_empty());
    assert!(header_path(&lib.db).exists());
}

// ---- other process holds the library -------------------------------------

const HOLD_ENV: &str = "JIZZCRAFT_CONVERT_HOLD";

/// Child only: open the library, keep a write transaction open, say so.
#[test]
fn hold_child() {
    let Ok(p) = std::env::var(HOLD_ENV) else {
        return;
    };
    let db = Db::open(Path::new(&p)).unwrap();
    db.conn_for_tests()
        .execute_batch("BEGIN IMMEDIATE")
        .unwrap();
    db.conn_for_tests()
        .execute("UPDATE media SET rating = 5 WHERE id = 1", [])
        .unwrap();
    println!("HOLDING-READY");
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn busy_library_fails_without_corruption() {
    let lib = library(0);
    let before = snap(&lib, None);
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["hold_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(HOLD_ENV, &lib.db)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let ready = std::io::BufReader::new(out)
        .lines()
        .map_while(Result::ok)
        .any(|l| l.contains("HOLDING-READY"));
    assert!(ready, "child did not start");
    let r = enable_with(&lib.db, PASS, FAST, &lib.opts(None));
    assert_eq!(r, Err(VaultError::Busy));
    assert_eq!(probe_format(&lib.db).unwrap(), FileFormat::Plain);
    assert!(!header_path(&lib.db).exists());
    assert!(
        leftovers(&lib).iter().all(|n| n.ends_with("-wal")),
        "{:?}",
        leftovers(&lib)
    );
    child.kill().unwrap();
    child.wait().unwrap();
    // The child's uncommitted write is gone; nothing else changed.
    assert_eq!(snap(&lib, None), before);
    enable_with(&lib.db, PASS, FAST, &lib.opts(None)).unwrap();
    assert_eq!(snap(&lib, Some(PASS)), before);
}

// ---- kill -9 ---------------------------------------------------------------

const KILL_ENV: &str = "JIZZCRAFT_CONVERT_CHILD";
const KILL_CACHE: &str = "JIZZCRAFT_CONVERT_CACHE";

/// Child only: flip the library between modes until killed.
#[test]
fn convert_child() {
    let Ok(p) = std::env::var(KILL_ENV) else {
        return;
    };
    let db = PathBuf::from(p);
    let opts = ConvertOptions {
        cache_dir: std::env::var_os(KILL_CACHE).map(PathBuf::from),
        fail_at: None,
    };
    loop {
        match recover_with(&db, &opts) {
            Ok(FileFormat::Plain) => {
                let _ = enable_with(&db, PASS, FAST, &opts);
            }
            Ok(FileFormat::Encrypted) => {
                let _ = disable_with(&db, PASS, &opts);
            }
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[test]
fn kill_nine_during_conversion_leaves_one_consistent_mode() {
    const FILES: usize = 2_000;
    let lib = library(FILES);
    let before = snap(&lib, None);
    assert_eq!(before.total, FILES as u64 + 1);
    let exe = std::env::current_exe().unwrap();
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    let mut seen = [0usize; 2];
    for round in 0..20 {
        let mut child = Command::new(&exe)
            .args([
                "convert_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(KILL_ENV, &lib.db)
            .env(KILL_CACHE, &lib.cache)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        std::thread::sleep(Duration::from_millis(30 + seed % 400));
        child.kill().unwrap();
        child.wait().unwrap();

        let fmt = recover_with(&lib.db, &lib.opts(None)).unwrap();
        // A `-wal` may remain from a killed connection; SQLite replays it.
        let stray: Vec<_> = leftovers(&lib)
            .into_iter()
            .filter(|n| !n.ends_with("-wal"))
            .collect();
        assert!(stray.is_empty(), "round {round}: {stray:?}");
        match fmt {
            FileFormat::Plain => {
                seen[0] += 1;
                assert!(!header_path(&lib.db).exists(), "round {round}");
                assert_eq!(snap(&lib, None), before, "round {round}");
            }
            FileFormat::Encrypted => {
                seen[1] += 1;
                assert!(header_path(&lib.db).exists(), "round {round}");
                assert_eq!(snap(&lib, Some(PASS)), before, "round {round}");
                assert!(leaks(&lib).is_empty(), "round {round}");
            }
            FileFormat::Absent => panic!("round {round}: library vanished"),
        }
    }
    eprintln!("kill -9 outcomes: plain {} encrypted {}", seen[0], seen[1]);
}
