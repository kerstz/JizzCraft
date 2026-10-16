mod common;

use jizzcraft_core::{Db, Filter, Registry, register_core, scan};
use jizzcraft_media::phash::{phash_pending, video_phash};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
        }
    }
    out
}

fn ids_by_name(db: &Db) -> BTreeMap<String, i64> {
    db.query(&Filter::default())
        .unwrap()
        .into_iter()
        .map(|m| (m.name, m.id))
        .collect()
}

#[test]
fn same_pattern_at_two_sizes_is_close_and_another_is_far() {
    if !common::clips_ready() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        t.path().join("a.webm"),
        t.path().join("b.webm"),
        t.path().join("c.webm"),
    );
    common::make_video_pattern(&a, 90, 320, 180, "ball");
    common::make_video_pattern(&b, 90, 640, 360, "ball");
    common::make_video_pattern(&c, 90, 320, 180, "pinwheel");
    let cancel = AtomicBool::new(false);
    let (ha, hb, hc) = (
        video_phash(&a, &cancel).unwrap(),
        video_phash(&b, &cancel).unwrap(),
        video_phash(&c, &cancel).unwrap(),
    );
    let near = jizzcraft_core::phash::hamming(ha, hb);
    let far = jizzcraft_core::phash::hamming(ha, hc);
    eprintln!("near {near} far {far}");
    assert!(near <= 8, "same pattern, two sizes: {near}");
    assert!(far > 10, "different pattern: {far}");
}

#[test]
fn library_phash_stores_hashes_without_touching_files_or_cache() {
    if !common::clips_ready() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    common::make_video_pattern(&t.path().join("a.webm"), 90, 320, 180, "ball");
    common::make_video_pattern(&t.path().join("b.webm"), 90, 640, 360, "ball");
    common::make_video_pattern(&t.path().join("c.webm"), 90, 320, 180, "pinwheel");
    std::fs::write(t.path().join("bad.mp4"), vec![0u8; 4096]).unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();

    let cache = jizzcraft_core::paths::cache_dir();
    let (before, cache_before) = (tree(t.path()), tree(&cache));

    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    jizzcraft_media::integrity::register_media(&mut reg);
    let r = reg.run(&mut db, "library.phash", json!({})).unwrap();
    assert_eq!(
        (r["hashed"].as_u64(), r["failed"].as_u64()),
        (Some(3), Some(1))
    );
    assert_eq!(r["remaining"], 0, "a failed file is not retried");
    let again = reg.run(&mut db, "library.phash", json!({})).unwrap();
    assert_eq!(
        (again["hashed"].as_u64(), again["failed"].as_u64()),
        (Some(0), Some(0))
    );

    assert_eq!(tree(t.path()), before, "media tree unchanged");
    assert_eq!(tree(&cache), cache_before, "nothing written to the cache");

    let id = ids_by_name(&db);
    let near = reg
        .run(
            &mut db,
            "media.phash.distance",
            json!({ "a": id["a.webm"], "b": id["b.webm"] }),
        )
        .unwrap();
    assert!(near["distance"].as_u64().unwrap() <= 8);
    let d = reg.run(&mut db, "media.duplicates", json!({})).unwrap();
    let groups = d["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["items"].as_array().unwrap().len(), 2);
}

#[test]
fn cancel_stops_the_job_before_any_work() {
    if !common::clips_ready() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    common::make_video_pattern(&t.path().join("a.webm"), 90, 320, 180, "ball");
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    let stop = AtomicBool::new(true);
    let r = phash_pending(&mut db, false, None, &stop, &mut |_, _| {}).unwrap();
    assert!(r.cancelled);
    assert_eq!((r.hashed, r.failed, r.remaining), (0, 0, 1));
}
