//! A library folder whose files moved: `library.move_root`.

use jizzcraft_core::fingerprint::fingerprint_pending;
use jizzcraft_core::relocate::{MoveReport, move_root};
use jizzcraft_core::{Db, Error, Filter, HiddenMode, Registry, register_core, scan};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn png(path: &Path, w: u32, salt: u8) {
    image::RgbImage::from_fn(w, w, |x, y| image::Rgb([x as u8 ^ salt, y as u8, salt]))
        .save(path)
        .unwrap();
}

/// `old/a.png`, `old/b.png`, `old/sub/v.mp4` (300 KiB, so the fingerprint
/// reads a head and a tail) in a library stored on disk.
fn library(t: &Path) -> (Db, PathBuf) {
    let old = t.join("old");
    fs::create_dir_all(old.join("sub")).unwrap();
    png(&old.join("a.png"), 33, 1);
    png(&old.join("b.png"), 34, 2);
    let video: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    fs::write(old.join("sub/v.mp4"), video).unwrap();
    let mut db = Db::open(&t.join("library.db")).unwrap();
    scan::import_folder(&mut db, &old).unwrap();
    (db, jizzcraft_core::pathbytes::canonical(&old).unwrap())
}

fn all(db: &Db) -> Vec<jizzcraft_core::Media> {
    db.query(&Filter {
        include_missing: true,
        hidden: HiddenMode::Include,
        ..Filter::default()
    })
    .unwrap()
}

fn by_name(db: &Db, name: &str) -> jizzcraft_core::Media {
    all(db).into_iter().find(|m| m.name == name).unwrap()
}

fn hash_tree(root: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    let mut out = BTreeMap::new();
    for e in walkdir::WalkDir::new(root) {
        let e = e.unwrap();
        if e.file_type().is_file() {
            out.insert(
                e.path().to_path_buf(),
                *blake3::hash(&fs::read(e.path()).unwrap()).as_bytes(),
            );
        }
    }
    out
}

fn run(db: &mut Db, from: &Path, to: &Path, dry_run: bool) -> jizzcraft_core::Result<MoveReport> {
    move_root(db, from, to, dry_run, &AtomicBool::new(false))
}

#[test]
fn a_moved_folder_keeps_its_items_and_their_data() {
    let t = tempfile::tempdir().unwrap();
    let (mut db, old) = library(t.path());
    // Only the rated images are fingerprinted (user data only), the video is
    // not: both kinds of check are used.
    let a = by_name(&db, "a.png").id;
    let v = by_name(&db, "v.mp4").id;
    db.set_rating(a, 4).unwrap();
    db.set_rating(by_name(&db, "b.png").id, 1).unwrap();
    fingerprint_pending(&mut db, true, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert!(by_name(&db, "v.mp4").fp.is_none());
    db.toggle_favorite(v).unwrap();

    // The drive now mounts elsewhere: everything is missing after a rescan.
    let new = t.path().join("new");
    fs::rename(&old, &new).unwrap();
    scan::rescan_all(&mut db).unwrap();
    assert!(all(&db).iter().all(|m| m.missing));
    let before = hash_tree(&new);

    let dry = run(&mut db, &old, &new, true).unwrap();
    assert_eq!(
        (
            dry.items,
            dry.found,
            dry.verified,
            dry.different,
            dry.not_found
        ),
        (3, 3, 2, 0, 0)
    );
    assert!(dry.dry_run && !dry.backed_up);
    assert!(
        all(&db).iter().all(|m| m.missing),
        "a dry run changes nothing"
    );
    assert_eq!(db.folders().unwrap()[0].1, old);

    let rep = run(&mut db, &old, &new, false).unwrap();
    assert_eq!((rep.found, rep.different, rep.not_found), (3, 0, 0));
    assert!(rep.backed_up && !rep.dry_run);
    let canon = jizzcraft_core::pathbytes::canonical(&new).unwrap();
    assert_eq!(db.folders().unwrap(), vec![(1, canon.clone())]);
    let items = all(&db);
    assert_eq!(items.len(), 3);
    assert!(
        items
            .iter()
            .all(|m| !m.missing && m.path.starts_with(&canon))
    );
    assert_eq!(by_name(&db, "a.png").id, a);
    assert_eq!(by_name(&db, "a.png").rating, 4);
    assert!(by_name(&db, "v.mp4").favorite);
    assert_eq!(by_name(&db, "v.mp4").path, canon.join("sub").join("v.mp4"));

    // A rescan agrees: nothing new, nothing missing.
    let scan = scan::rescan_all(&mut db).unwrap();
    assert_eq!((scan.added, scan.marked_missing), (0, 0));
    assert_eq!(all(&db).len(), 3);
    assert_eq!(hash_tree(&new), before, "no file was touched");
    // The backup sits beside the library.
    let backups = fs::read_dir(t.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("library.pre-move-")
        })
        .count();
    assert_eq!(backups, 1);
}

#[test]
fn other_and_absent_files_stay_behind_as_missing() {
    let t = tempfile::tempdir().unwrap();
    let (mut db, old) = library(t.path());
    fingerprint_pending(
        &mut db,
        false,
        None,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    db.set_rating(by_name(&db, "b.png").id, 2).unwrap();
    let new = t.path().join("new");
    fs::rename(&old, &new).unwrap();
    // Same size, other content: a different file under the same name.
    let mut bytes = fs::read(new.join("sub/v.mp4")).unwrap();
    bytes[10] ^= 0xff;
    fs::write(new.join("sub/v.mp4"), bytes).unwrap();
    fs::remove_file(new.join("a.png")).unwrap();

    let rep = run(&mut db, &old, &new, false).unwrap();
    assert_eq!(
        (rep.items, rep.found, rep.different, rep.not_found),
        (3, 1, 1, 1)
    );
    let canon = jizzcraft_core::pathbytes::canonical(&new).unwrap();
    let b = by_name(&db, "b.png");
    assert!(!b.missing && b.path.starts_with(&canon) && b.rating == 2);
    for name in ["a.png", "v.mp4"] {
        let m = by_name(&db, name);
        assert!(m.missing, "{name} is missing");
        assert!(m.path.starts_with(&old), "{name} keeps its old path");
    }
    // The changed video is a new item for the scanner; its data is not
    // carried over to the wrong file.
    let scan = scan::rescan_all(&mut db).unwrap();
    assert_eq!(scan.added, 1);
    let fresh: Vec<_> = all(&db)
        .into_iter()
        .filter(|m| m.name == "v.mp4" && !m.missing)
        .collect();
    assert_eq!(fresh.len(), 1);
    assert!(fresh[0].path.starts_with(&canon));
}

#[test]
fn wrong_destinations_are_refused_and_change_nothing() {
    let t = tempfile::tempdir().unwrap();
    let (mut db, old) = library(t.path());
    let other = t.path().join("other");
    fs::create_dir_all(other.join("inner")).unwrap();
    png(&other.join("x.png"), 20, 9);
    scan::import_folder(&mut db, &other).unwrap();
    let empty = t.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let snapshot = |db: &Db| {
        let items: Vec<_> = all(db)
            .into_iter()
            .map(|m| (m.id, m.path, m.missing))
            .collect();
        (db.folders().unwrap(), items)
    };
    let start = snapshot(&db);

    // Not a library folder.
    assert!(matches!(
        run(&mut db, &empty, &old, true),
        Err(Error::NotFound(_))
    ));
    // Into, onto or above a library folder.
    for to in [
        other.clone(),
        other.join("inner"),
        old.join("sub"),
        t.path().to_path_buf(),
    ] {
        assert!(
            matches!(run(&mut db, &old, &to, false), Err(Error::Invalid(_))),
            "{to:?} was accepted"
        );
    }
    // Not a folder, or not there.
    assert!(run(&mut db, &old, &other.join("x.png"), false).is_err());
    assert!(run(&mut db, &old, &t.path().join("nowhere"), false).is_err());
    // An unrelated folder holds none of the files: refused, even for real.
    assert!(matches!(
        run(&mut db, &old, &empty, false),
        Err(Error::Invalid(_))
    ));
    assert_eq!(snapshot(&db), start);
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_at_the_new_place_is_not_the_file() {
    let t = tempfile::tempdir().unwrap();
    let (mut db, old) = library(t.path());
    let new = t.path().join("new");
    fs::rename(&old, &new).unwrap();
    let keep = t.path().join("kept.png");
    fs::rename(new.join("a.png"), &keep).unwrap();
    std::os::unix::fs::symlink(&keep, new.join("a.png")).unwrap();
    let rep = run(&mut db, &old, &new, true).unwrap();
    assert_eq!((rep.found, rep.different), (2, 1));
}

#[test]
fn the_command_is_a_dry_run_unless_told_otherwise() {
    let t = tempfile::tempdir().unwrap();
    let (mut db, old) = library(t.path());
    let new = t.path().join("new");
    fs::rename(&old, &new).unwrap();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let args = json!({ "from": old, "to": new });
    let v = reg.run(&mut db, "library.move_root", args.clone()).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["found"], 3);
    assert_eq!(db.folders().unwrap()[0].1, old);
    // Counts only: no path or name in the answer.
    let text = v.to_string();
    assert!(!text.contains("new") && !text.contains(".png"), "{text}");

    let mut go = args;
    go["dry_run"] = json!(false);
    let v = reg.run(&mut db, "library.move_root", go).unwrap();
    assert_eq!(
        (v["dry_run"].clone(), v["found"].clone()),
        (json!(false), json!(3))
    );
    assert_ne!(db.folders().unwrap()[0].1, old);
    assert!(
        reg.run(
            &mut db,
            "library.move_root",
            json!({ "from": old, "to": new, "dry_run": "yes" })
        )
        .is_err()
    );
}
