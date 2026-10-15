//! Forgetting an imported root as a job: backup first, bounded chunks,
//! clean cancellation, same registry command.
use jizzcraft_core::scan::{ScanControl, scan_folder_with};
use jizzcraft_core::{Db, Filter, Registry, register_core};
use serde_json::json;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn make_tree(root: &Path, n: usize) {
    for i in 0..n {
        let d = root.join(format!("d{:03}", i / 200));
        if i % 200 == 0 {
            std::fs::create_dir_all(&d).unwrap();
        }
        std::fs::File::create(d.join(format!("f{i:06}.mp4"))).unwrap();
    }
}

fn setup(n: usize) -> (tempfile::TempDir, tempfile::TempDir, Db) {
    let lib = tempfile::tempdir().unwrap();
    let media = tempfile::tempdir().unwrap();
    make_tree(media.path(), n);
    let mut db = Db::open(&lib.path().join("library.db")).unwrap();
    let (id, _) = db.add_folder(media.path()).unwrap();
    scan_folder_with(&mut db, id, media.path(), &ScanControl::default()).unwrap();
    (lib, media, db)
}

fn total(db: &Db) -> u64 {
    db.count(&Filter {
        include_missing: true,
        ..Default::default()
    })
    .unwrap()
}

fn backups(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".pre-remove-")
        })
        .collect();
    v.sort();
    v
}

#[test]
fn backup_holds_the_rows_and_is_written_before_deleting() {
    let (lib, media, mut db) = setup(300);
    let root = db.folders().unwrap()[0].1.clone();
    let cancel = AtomicBool::new(false);
    let mut calls = 0;
    let r = db
        .remove_folder_safely(&root, &cancel, &mut |_| calls += 1)
        .unwrap()
        .unwrap();
    assert_eq!((r.removed, r.cancelled, r.backed_up), (300, false, true));
    assert!(calls >= 1);
    assert_eq!(total(&db), 0);
    assert!(db.folders().unwrap().is_empty());
    let b = backups(lib.path());
    assert_eq!(b.len(), 1);
    let old = Db::open_read_only(&b[0]).unwrap();
    assert_eq!(total(&old), 300);
    assert_eq!(old.folders().unwrap().len(), 1);
    // Files on disk are untouched.
    assert!(media.path().join("d000/f000000.mp4").exists());
}

#[test]
fn unknown_root_makes_no_backup() {
    let (lib, _m, mut db) = setup(3);
    let cancel = AtomicBool::new(false);
    let r = db
        .remove_folder_safely(Path::new("/nope"), &cancel, &mut |_| {})
        .unwrap();
    assert!(r.is_none());
    assert!(backups(lib.path()).is_empty());
}

#[test]
fn backups_rotate_to_three() {
    let (lib, _m, db) = setup(2);
    for _ in 0..6 {
        db.backup_before_remove().unwrap();
    }
    assert_eq!(backups(lib.path()).len(), 3);
}

/// 10 000 items: work is cut into bounded transactions (many progress ticks),
/// and a cancel between two chunks stops cleanly, keeping the root and a
/// consistent database; a second run finishes.
#[test]
fn ten_thousand_items_chunked_and_cancellable() {
    let (_lib, _m, mut db) = setup(10_000);
    let root = db.folders().unwrap()[0].1.clone();
    let cancel = AtomicBool::new(false);
    let mut ticks = 0;
    let r = db
        .remove_folder_safely(&root, &cancel, &mut |n| {
            ticks += 1;
            if n >= 1_000 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap()
        .unwrap();
    assert!(r.cancelled);
    assert!((2..=4).contains(&ticks), "ticks {ticks}");
    assert_eq!(total(&db), 10_000 - r.removed);
    assert_eq!(db.folders().unwrap().len(), 1);
    db.quick_check().unwrap();

    cancel.store(false, Ordering::Relaxed);
    let mut ticks = 0;
    let r2 = db
        .remove_folder_safely(&root, &cancel, &mut |_| ticks += 1)
        .unwrap()
        .unwrap();
    assert!(!r2.cancelled);
    assert!(ticks >= 15, "ticks {ticks}");
    assert_eq!(r.removed + r2.removed, 10_000);
    assert_eq!(total(&db), 0);
    assert!(db.folders().unwrap().is_empty());
}

#[test]
fn registry_command_backs_up() {
    let (lib, _m, mut db) = setup(5);
    let root = db.folders().unwrap()[0].1.clone();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let v = reg
        .run(&mut db, "library.remove", json!({ "path": root }))
        .unwrap();
    assert_eq!(v["removed_media"], 5);
    assert_eq!(backups(lib.path()).len(), 1);
}

#[test]
fn tagged_backup_works_from_a_read_only_connection() {
    let (lib, _m, db) = setup(7);
    drop(db);
    let ro = Db::open_read_only(&lib.path().join("library.db")).unwrap();
    let p = ro.backup_tagged("pre-check").unwrap().unwrap();
    assert!(p.to_string_lossy().contains(".pre-check-"));
    assert_eq!(total(&Db::open_read_only(&p).unwrap()), 7);
}
