//! Scan as a job at scale: batching, progress, cancellation, hostile trees.
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]
use jizzcraft_core::scan::{ScanControl, scan_folder, scan_folder_with};
use jizzcraft_core::{Db, Filter};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// `n` empty `.mp4` files, 200 per sub-directory.
fn make_tree(root: &Path, n: usize) {
    for i in 0..n {
        let d = root.join(format!("d{:04}", i / 200));
        if i % 200 == 0 {
            std::fs::create_dir_all(&d).unwrap();
        }
        std::fs::File::create(d.join(format!("f{i:06}.mp4"))).unwrap();
    }
}

fn setup(n: usize) -> (tempfile::TempDir, tempfile::TempDir, Db, i64) {
    let lib = tempfile::tempdir().unwrap();
    let media = tempfile::tempdir().unwrap();
    make_tree(media.path(), n);
    let mut db = Db::open(&lib.path().join("library.db")).unwrap();
    let (id, _) = db.add_folder(media.path()).unwrap();
    (lib, media, db, id)
}

fn count_all(db: &Db) -> u64 {
    db.count(&Filter {
        include_missing: true,
        ..Default::default()
    })
    .unwrap()
}

fn full_scan(n: usize) {
    let (_l, m, mut db, id) = setup(n);
    let ctl = ScanControl::default();
    let t = Instant::now();
    let r = scan_folder_with(&mut db, id, m.path(), &ctl).unwrap();
    eprintln!("scan of {n}: {:?}", t.elapsed());
    assert_eq!(r.added, n as u64);
    assert!(!r.cancelled);
    assert!(ctl.progress.load(Ordering::Relaxed) >= n as u64);
    assert_eq!(count_all(&db), n as u64);
    let r2 = scan_folder(&mut db, id, m.path()).unwrap();
    assert_eq!(
        (r2.unchanged, r2.added, r2.marked_missing),
        (n as u64, 0, 0)
    );
    db.quick_check().unwrap();
}

#[test]
fn scan_5k_files() {
    full_scan(5_000);
}

#[test]
#[ignore = "set JIZZCRAFT_SCALE=1 and pass --ignored"]
fn scan_100k_files() {
    if std::env::var("JIZZCRAFT_SCALE").as_deref() != Ok("1") {
        eprintln!("JIZZCRAFT_SCALE=1 not set: skipped");
        return;
    }
    full_scan(100_000);
}

#[test]
fn cancel_is_prompt_and_keeps_committed_rows() {
    let (_l, m, mut db, id) = setup(30_000);
    let ctl = ScanControl::default();
    let (c2, set_at) = (
        ctl.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(None)),
    );
    let s2 = set_at.clone();
    let watcher = std::thread::spawn(move || {
        while c2.progress.load(Ordering::Relaxed) < 100 {
            std::thread::sleep(Duration::from_millis(1));
        }
        *s2.lock().unwrap() = Some(Instant::now());
        c2.cancel.store(true, Ordering::Relaxed);
    });
    let r = scan_folder_with(&mut db, id, m.path(), &ctl).unwrap();
    let back = Instant::now();
    watcher.join().unwrap();
    let at = set_at.lock().unwrap().unwrap();
    assert!(r.cancelled, "scan finished before the cancel landed");
    assert!(back.duration_since(at) < Duration::from_secs(1));
    let n = count_all(&db);
    assert!(n < 30_000);
    assert_eq!(n, r.added, "everything counted was committed");
    assert_eq!(r.marked_missing, 0);
    db.quick_check().unwrap();
    // A fresh scan finishes the job.
    let r2 = scan_folder(&mut db, id, m.path()).unwrap();
    assert_eq!(r2.added + n, 30_000);
    assert_eq!(count_all(&db), 30_000);
}

#[test]
fn cancel_before_start_does_nothing() {
    let (_l, m, mut db, id) = setup(100);
    let ctl = ScanControl::default();
    ctl.cancel.store(true, Ordering::Relaxed);
    let r = scan_folder_with(&mut db, id, m.path(), &ctl).unwrap();
    assert!(r.cancelled);
    assert_eq!(count_all(&db), 0);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn non_utf8_names_dangling_links_and_unreadable_dirs() {
        let (_l, m, mut db, id) = setup(10);
        let raw = std::ffi::OsStr::from_bytes(b"bad-\xff\xfe.mp4");
        std::fs::File::create(m.path().join(raw)).unwrap();
        std::os::unix::fs::symlink("/definitely/not/here", m.path().join("dangling.mp4")).unwrap();
        let locked = m.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::File::create(locked.join("secret.mp4")).unwrap();

        let r = scan_folder(&mut db, id, m.path()).unwrap();
        assert_eq!(r.added, 12);
        assert_eq!(r.skipped_symlinks, 1);
        assert_eq!(r.errors, 0);
        let raw_path = m.path().join(raw);
        let all = db.query(&Filter::default()).unwrap();
        assert!(all.iter().any(|x| x.path == raw_path));

        // Unreadable directory: its known files must not turn "missing".
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let can_read = std::fs::read_dir(&locked).is_ok(); // running as root
        let r = scan_folder(&mut db, id, m.path()).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !can_read {
            assert!(r.errors >= 1);
        }
        assert_eq!(r.marked_missing, 0);
        assert_eq!(db.query(&Filter::default()).unwrap().len(), 12);
    }

    #[test]
    fn read_only_tree_scans_and_stays_untouched() {
        let (_l, m, mut db, id) = setup(50);
        let mut dirs: Vec<_> = std::fs::read_dir(m.path()).unwrap().flatten().collect();
        dirs.sort_by_key(|e| e.path());
        for d in &dirs {
            for f in std::fs::read_dir(d.path()).unwrap().flatten() {
                std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
            }
            std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let r = scan_folder(&mut db, id, m.path()).unwrap();
        for d in &dirs {
            std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!((r.added, r.errors), (50, 0));
    }
}
