use jizzcraft_core::fingerprint::{fingerprint_pending, quick_fp};
use jizzcraft_core::{Db, Filter, scan};
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn png(path: &Path, w: u32, h: u32, salt: u8) {
    image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8 ^ salt, y as u8, salt]))
        .save(path)
        .unwrap();
}

fn fingerprint(db: &mut Db) {
    let cancel = AtomicBool::new(false);
    fingerprint_pending(db, false, None, &cancel, &mut |_, _| {}).unwrap();
}

fn find(db: &Db, name: &str) -> jizzcraft_core::Media {
    db.query(&Filter {
        include_missing: true,
        ..Default::default()
    })
    .unwrap()
    .into_iter()
    .find(|m| m.name == name)
    .unwrap_or_else(|| panic!("no item named {name}"))
}

fn rescan_all(db: &mut Db) -> scan::ScanReport {
    scan::rescan_all(db).unwrap()
}

#[test]
fn rename_in_place_keeps_user_data() {
    let t = tempfile::tempdir().unwrap();
    png(&t.path().join("old.png"), 64, 64, 1);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    let id = find(&db, "old.png").id;
    db.set_favorite(id, true).unwrap();
    db.set_rating(id, 4).unwrap();
    db.add_marker(id, 1234, "kept").unwrap();
    fingerprint(&mut db);
    assert!(
        find(&db, "old.png")
            .fp
            .as_deref()
            .unwrap()
            .starts_with("fp1:")
    );

    fs::rename(t.path().join("old.png"), t.path().join("new name.png")).unwrap();
    let rep = rescan_all(&mut db);
    assert_eq!((rep.relinked, rep.added), (1, 0), "{rep:?}");

    let m = find(&db, "new name.png");
    assert_eq!(m.id, id, "same row");
    assert!(m.favorite && m.rating == 4 && !m.missing);
    assert_eq!(db.markers(id).unwrap().len(), 1);
    assert_eq!(db.stats().unwrap().total, 1);
}

#[test]
fn move_between_imported_folders() {
    let t = tempfile::tempdir().unwrap();
    let (a, b) = (t.path().join("A"), t.path().join("B"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    png(&a.join("x.png"), 48, 48, 2);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, &a).unwrap();
    scan::import_folder(&mut db, &b).unwrap();
    let id = find(&db, "x.png").id;
    db.set_rating(id, 5).unwrap();
    fingerprint(&mut db);

    fs::rename(a.join("x.png"), b.join("x.png")).unwrap();
    let rep = rescan_all(&mut db);
    assert_eq!(rep.relinked, 1, "{rep:?}");
    let m = find(&db, "x.png");
    assert_eq!((m.id, m.rating), (id, 5));
    assert!(
        m.path
            .starts_with(jizzcraft_core::pathbytes::canonical(&b).unwrap())
    );
}

#[test]
fn copy_is_not_a_move() {
    let t = tempfile::tempdir().unwrap();
    png(&t.path().join("orig.png"), 32, 32, 3);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    fingerprint(&mut db);
    fs::copy(t.path().join("orig.png"), t.path().join("copy.png")).unwrap();
    let rep = rescan_all(&mut db);
    assert_eq!((rep.relinked, rep.added), (0, 1));
    assert_eq!(db.stats().unwrap().total, 2);
}

#[test]
fn same_size_different_bytes_is_not_a_move() {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("a.png"), vec![1u8; 2000]).unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    fingerprint(&mut db);
    fs::remove_file(t.path().join("a.png")).unwrap();
    fs::write(t.path().join("b.png"), vec![2u8; 2000]).unwrap();
    let rep = rescan_all(&mut db);
    assert_eq!(
        (rep.relinked, rep.added, rep.marked_missing),
        (0, 1, 1),
        "{rep:?}"
    );
}

#[test]
fn unmounted_root_does_not_lend_its_rows() {
    let t = tempfile::tempdir().unwrap();
    let (a, b) = (t.path().join("usb"), t.path().join("home"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    png(&a.join("p.png"), 40, 40, 4);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, &a).unwrap();
    scan::import_folder(&mut db, &b).unwrap();
    fingerprint(&mut db);

    // The drive disappears, and an identical file shows up elsewhere.
    // New address, same fingerprint: the file cannot hide from us.
    let bytes = fs::read(a.join("p.png")).unwrap();
    fs::remove_dir_all(&a).unwrap();
    fs::write(b.join("p.png"), bytes).unwrap();
    let rep = rescan_all(&mut db);
    assert_eq!(rep.relinked, 0, "{rep:?}");
    assert_eq!(db.stats().unwrap().total, 2);
}

#[test]
fn two_identical_vanished_files_pair_deterministically() {
    let t = tempfile::tempdir().unwrap();
    fs::create_dir_all(t.path().join("o")).unwrap();
    let bytes = vec![9u8; 4000];
    fs::write(t.path().join("o/one.png"), &bytes).unwrap();
    fs::write(t.path().join("o/two.png"), &bytes).unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    let (id1, id2) = (find(&db, "one.png").id, find(&db, "two.png").id);
    fingerprint(&mut db);
    fs::rename(t.path().join("o/one.png"), t.path().join("one.png")).unwrap();
    fs::rename(t.path().join("o/two.png"), t.path().join("2.png")).unwrap();
    let rep = rescan_all(&mut db);
    assert_eq!((rep.relinked, rep.added), (2, 0), "{rep:?}");
    assert_eq!(find(&db, "one.png").id, id1, "same name wins");
    assert_eq!(find(&db, "2.png").id, id2);
}

#[test]
fn large_file_fingerprint_ignores_the_middle() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("big.bin");
    let mut data = vec![7u8; 300_000];
    fs::write(&p, &data).unwrap();
    let (s1, f1) = quick_fp(&p).unwrap();
    data[150_000] = 99; // middle only: known limitation, size is the guard
    fs::write(&p, &data).unwrap();
    let (s2, f2) = quick_fp(&p).unwrap();
    assert_eq!((s1, f1), (s2, f2));
    data[10] = 1; // head change is caught
    fs::write(&p, &data).unwrap();
    assert_ne!(quick_fp(&p).unwrap().1, f1);
}

#[test]
fn merge_combines_user_data() {
    let t = tempfile::tempdir().unwrap();
    png(&t.path().join("a.png"), 20, 20, 5);
    png(&t.path().join("b.png"), 21, 21, 6);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    let (a, b) = (find(&db, "a.png").id, find(&db, "b.png").id);
    db.set_favorite(a, true).unwrap();
    db.set_rating(a, 5).unwrap();
    db.set_rating(b, 2).unwrap();
    db.add_marker(a, 100, "from").unwrap();
    db.add_marker(b, 100, "to wins").unwrap();
    db.add_marker(a, 200, "moved over").unwrap();

    db.merge_into(a, b).unwrap();
    assert!(db.get(a).unwrap().is_none());
    let m = db.get(b).unwrap().unwrap();
    assert!(m.favorite);
    assert_eq!(m.rating, 5);
    let marks = db.markers(b).unwrap();
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0].label, "to wins");
    assert!(db.merge_into(b, b).is_err());
}
