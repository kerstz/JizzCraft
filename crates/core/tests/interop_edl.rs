//! Kodi `.edl` import: a tiny line parser and an importer that is read-only,
//! bounded, dry-run by default, idempotent and exact.
use jizzcraft_core::fingerprint::fingerprint_pending;
use jizzcraft_core::interop::edl::{EdlReject, import_edl, parse_edl};
use jizzcraft_core::interop::{ImportOpts, InteropReport};
use jizzcraft_core::{Db, scan};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn apply() -> ImportOpts {
    ImportOpts {
        dry_run: false,
        ..Default::default()
    }
}

fn run(db: &mut Db, dir: Option<&Path>, o: &ImportOpts) -> InteropReport {
    import_edl(db, dir, o, &AtomicBool::new(false)).unwrap()
}

fn m(ms: i64, l: &str) -> (i64, String) {
    (ms, l.to_string())
}

#[test]
fn parse_maps_actions() {
    let (v, ig) = parse_edl(b"10 20 0\n30.5 31 1\n40 41 2\n50 60 3\n70 80 9\n").unwrap();
    assert_eq!(
        v,
        vec![m(10_000, "Cut"), m(30_500, "Mute"), m(40_000, "Scene")]
    );
    assert_eq!(ig, 2, "actions 3 and 9");
}

#[test]
fn parse_times_and_invalid_lines() {
    let (v, ig) = parse_edl(
        b"\xEF\xBB\xBF00:01:02.5 00:01:03 0\n\
          #100 #200 0\n\
          20 10 0\n\
          abc 5 0\n\
          5 6\n\
          -1 5 0\n\
          1e3 5000 0\n\
          99999999 99999999 0\n\
          \n\
          0.0004 1 2\n",
    )
    .unwrap();
    assert_eq!(v, vec![m(62_500, "Cut"), m(0, "Scene")]);
    assert_eq!(ig, 7);
}

#[test]
fn parse_limits() {
    let big = vec![b'#'; 64 * 1024 + 1];
    assert_eq!(parse_edl(&big), Err(EdlReject::TooBig));
    let many = "1 2 0\n".repeat(2_001);
    assert_eq!(parse_edl(many.as_bytes()), Err(EdlReject::TooMany));
    let ok = "1 2 0\n".repeat(2_000);
    assert_eq!(parse_edl(ok.as_bytes()).unwrap().0.len(), 2_000);
    assert_eq!(parse_edl(b"\xff\xfe"), Err(EdlReject::Malformed));
}

struct Lib {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: Db,
}

fn lib(names: &[&str]) -> Lib {
    let t = tempfile::tempdir().unwrap();
    let root = jizzcraft_core::pathbytes::canonical(t.path()).unwrap();
    for (i, n) in names.iter().enumerate() {
        fs::write(
            root.join(n),
            format!("not really a video number {i} {}", "x".repeat(40 + i)),
        )
        .unwrap();
    }
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, &root).unwrap();
    fingerprint_pending(
        &mut db,
        false,
        None,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    Lib { _t: t, root, db }
}

fn markers(db: &Db) -> Vec<(i64, String)> {
    let c = db.conn_for_tests();
    let mut st = c
        .prepare("SELECT pos_ms, label FROM markers ORDER BY media_id, pos_ms")
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn import_is_idempotent_and_dry_run_is_exact() {
    let mut l = lib(&["a.mp4", "b.mp4"]);
    fs::write(l.root.join("a.edl"), "10 20 0\n30 31 2\n").unwrap();
    fs::write(l.root.join("zzz.edl"), "1 2 0\n").unwrap();
    fs::write(l.root.join("bad.edl"), vec![b'#'; 70_000]).unwrap();
    let dry = run(&mut l.db, None, &ImportOpts::default());
    assert!(dry.dry_run, "{dry:?}");
    assert_eq!(
        (
            dry.files_seen,
            dry.parsed,
            dry.rejected,
            dry.matched,
            dry.unmatched
        ),
        (3, 2, 1, 1, 1),
        "{dry:?}"
    );
    assert_eq!((dry.markers_added, dry.applied), (2, 1));
    assert!(markers(&l.db).is_empty(), "dry run writes nothing");

    let done = run(&mut l.db, None, &apply());
    assert_eq!(
        InteropReport {
            dry_run: true,
            ..done.clone()
        },
        dry
    );
    assert_eq!(markers(&l.db), vec![m(10_000, "Cut"), m(30_000, "Scene")]);

    let again = run(&mut l.db, None, &apply());
    assert_eq!((again.markers_added, again.applied), (0, 0), "{again:?}");
    assert_eq!(markers(&l.db).len(), 2);
}

#[test]
fn manual_marker_at_the_same_ms_is_kept() {
    let mut l = lib(&["a.mp4"]);
    let id =
        l.db.conn_for_tests()
            .query_row("SELECT id FROM media", [], |r| r.get::<_, i64>(0))
            .unwrap();
    l.db.conn_for_tests()
        .execute(
            "INSERT INTO markers(media_id, pos_ms, label, created_at) VALUES (?1, 10000, 'mine', 0)",
            [id],
        )
        .unwrap();
    fs::write(l.root.join("a.edl"), "10 20 0\n").unwrap();
    let rep = run(&mut l.db, None, &apply());
    assert_eq!(rep.markers_added, 0, "{rep:?}");
    assert_eq!(markers(&l.db), vec![m(10_000, "mine")]);
}

#[test]
fn ambiguous_names_are_not_guessed() {
    let mut l = lib(&["a.mp4", "a.mkv"]);
    fs::write(l.root.join("a.edl"), "10 20 0\n").unwrap();
    let rep = run(&mut l.db, None, &apply());
    assert_eq!(
        (rep.ambiguous, rep.matched, rep.markers_added),
        (1, 0, 0),
        "{rep:?}"
    );
    assert!(markers(&l.db).is_empty());
}

#[test]
fn cancel_changes_nothing() {
    let mut l = lib(&["a.mp4"]);
    fs::write(l.root.join("a.edl"), "10 20 0\n").unwrap();
    let rep = import_edl(&mut l.db, None, &apply(), &AtomicBool::new(true)).unwrap();
    assert!(rep.cancelled && rep.markers_added == 0, "{rep:?}");
    assert!(markers(&l.db).is_empty());
}

#[test]
fn missing_folder_is_an_error() {
    let mut l = lib(&["a.mp4"]);
    let r = import_edl(
        &mut l.db,
        Some(&l.root.join("nope")),
        &apply(),
        &AtomicBool::new(false),
    );
    assert!(r.is_err());
}
