//! Perceptual hash, the metric tree and duplicate groups.
use image::{Rgb, RgbImage};
use jizzcraft_core::phash::{BkTree, FRAMES, hamming, perception_hash, sprite};
use jizzcraft_core::{Db, Filter, Registry, register_core, scan};
use serde_json::json;
use std::fs;

/// A smooth scene with energy at many frequencies (a plain ramp has almost
/// none, so its median-thresholded bits would flip on any noise).
fn gradient(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let (u, v) = (f64::from(x) / f64::from(w), f64::from(y) / f64::from(h));
        let c = |a: f64, b: f64, p: f64| {
            (128.0
                + 50.0 * (a * u * std::f64::consts::TAU + p).sin()
                + 40.0 * (b * v * std::f64::consts::TAU + p * 2.0).cos()
                + 30.0 * ((a + b) * (u + v) * std::f64::consts::PI).sin())
            .clamp(0.0, 255.0) as u8
        };
        Rgb([c(1.0, 2.0, 0.3), c(2.5, 1.5, 1.1), c(3.5, 3.0, 2.0)])
    })
}

fn checker(w: u32, h: u32, cell: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        if (x / cell + y / cell).is_multiple_of(2) {
            Rgb([240, 240, 240])
        } else {
            Rgb([10, 10, 10])
        }
    })
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn same_image_distance_zero_and_resizes_stay_close() {
    let a = gradient(320, 240);
    assert_eq!(hamming(perception_hash(&a), perception_hash(&a)), 0);
    let small = image::imageops::resize(&a, 160, 120, image::imageops::FilterType::Triangle);
    assert!(hamming(perception_hash(&a), perception_hash(&small)) <= 4);
}

#[test]
fn small_noise_stays_within_four_bits() {
    let a = gradient(256, 256);
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let noisy = RgbImage::from_fn(256, 256, |x, y| {
        let p = a.get_pixel(x, y);
        let mut n = |v: u8| (i32::from(v) + (rng.next() % 5) as i32 - 2).clamp(0, 255) as u8;
        Rgb([n(p[0]), n(p[1]), n(p[2])])
    });
    assert!(hamming(perception_hash(&a), perception_hash(&noisy)) <= 4);
}

#[test]
fn different_pictures_are_far_apart() {
    let g = perception_hash(&gradient(256, 256));
    let c = perception_hash(&checker(256, 256, 16));
    assert!(hamming(g, c) > 10, "{}", hamming(g, c));
}

#[test]
fn sprite_needs_exactly_25_frames_and_is_5x5() {
    let f = gradient(160, 90);
    assert!(sprite(&vec![f.clone(); FRAMES - 1]).is_none());
    let s = sprite(&vec![f; FRAMES]).unwrap();
    assert_eq!((s.width(), s.height()), (800, 450));
}

#[test]
fn bk_tree_matches_a_linear_scan() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let hashes: Vec<u64> = (0..2000).map(|_| rng.next()).collect();
    let mut tree = BkTree::new();
    for (i, h) in hashes.iter().enumerate() {
        tree.insert(*h, i as i64);
    }
    // A few near-duplicates so that hits exist at small distances.
    for (i, base) in hashes.iter().take(50).enumerate() {
        let h = base ^ (1 << (i % 64));
        tree.insert(h, 10_000 + i as i64);
    }
    let mut all: Vec<(u64, i64)> = hashes.iter().copied().zip(0..).collect();
    all.extend((0..50usize).map(|i| (hashes[i] ^ (1 << (i % 64)), 10_000 + i as i64)));
    for q in 0..40 {
        let query = if q % 2 == 0 {
            hashes[q * 7]
        } else {
            rng.next()
        };
        for max in [0u32, 4, 8, 16, 24] {
            let mut got: Vec<(i64, u32)> = tree.within(query, max);
            got.sort_unstable();
            let mut want: Vec<(i64, u32)> = all
                .iter()
                .map(|(h, id)| (*id, hamming(*h, query)))
                .filter(|(_, d)| *d <= max)
                .collect();
            want.sort_unstable();
            assert_eq!(got, want, "query {q} max {max}");
        }
    }
}

fn lib(root: &std::path::Path, n: usize) -> Db {
    for i in 0..n {
        fs::write(root.join(format!("v{i}.mp4")), vec![i as u8 + 1; 3000 + i]).unwrap();
    }
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, root).unwrap();
    db
}

fn ids(db: &Db) -> Vec<i64> {
    let mut v: Vec<_> = db
        .query(&Filter::default())
        .unwrap()
        .into_iter()
        .map(|m| (m.name, m.id))
        .collect();
    v.sort();
    v.into_iter().map(|(_, id)| id).collect()
}

#[test]
fn duplicates_group_by_distance_and_use_imported_hashes() {
    let t = tempfile::tempdir().unwrap();
    let db = lib(t.path(), 5);
    let id = ids(&db);
    // 0 and 1 are one bit apart; 2 only has an imported hash close to 0; 3 is far; 4 has none.
    let base = 0x0f0f_f0f0_1234_5678u64;
    db.set_phash(id[0], Some(base), 1).unwrap();
    db.set_phash(id[1], Some(base ^ 1), 1).unwrap();
    db.set_phash(id[3], Some(!base), 1).unwrap();
    db.set_phash(id[4], None, 1).unwrap();
    db.conn_for_tests()
        .execute(
            "INSERT INTO external_ids(media_id, scheme, value) VALUES (?1, 'phash.stash', ?2)",
            rusqlite::params![id[2], format!("{:016x}", base ^ 0b110)],
        )
        .unwrap();

    let none = db.duplicates(0, None, 100).unwrap();
    assert!(none.groups.is_empty());
    assert_eq!(none.hashed, 4);

    let one = db.duplicates(1, None, 100).unwrap();
    assert_eq!(one.groups.len(), 1);
    assert_eq!(one.groups[0].items.len(), 2);
    assert_eq!(
        db.duplicates(2, None, 100).unwrap().groups[0].items.len(),
        3
    );

    let rep = db.duplicates(8, None, 100).unwrap();
    assert_eq!(rep.groups.len(), 1);
    let got: Vec<i64> = rep.groups[0].items.iter().map(|i| i.id).collect();
    assert_eq!(got, vec![id[0], id[1], id[2]]);
    assert_eq!(rep.groups[0].distance, 3);
    assert_eq!(db.phash_distance(id[0], id[2]).unwrap(), Some(2));
    assert_eq!(db.phash_distance(id[0], id[4]).unwrap(), None);
    assert!(db.duplicates(17, None, 10).is_err());
}

#[test]
fn duration_difference_blocks_a_link() {
    let t = tempfile::tempdir().unwrap();
    let db = lib(t.path(), 2);
    let id = ids(&db);
    for i in &id {
        db.set_phash(*i, Some(7), 1).unwrap();
    }
    db.conn_for_tests()
        .execute_batch(&format!(
            "UPDATE media SET duration_ms = 60000 WHERE id = {};
             UPDATE media SET duration_ms = 90000 WHERE id = {};",
            id[0], id[1]
        ))
        .unwrap();
    assert_eq!(db.duplicates(0, None, 10).unwrap().groups.len(), 1);
    assert_eq!(db.duplicates(0, Some(5000), 10).unwrap().groups.len(), 0);
    assert_eq!(db.duplicates(0, Some(40_000), 10).unwrap().groups.len(), 1);
}

#[test]
fn pending_failed_and_rescan_rules() {
    let t = tempfile::tempdir().unwrap();
    let mut db = lib(t.path(), 3);
    let id = ids(&db);
    assert_eq!(db.phash_pending(false, 100).unwrap().len(), 3);
    db.set_phash(id[0], Some(1), 1).unwrap();
    db.set_phash(id[1], None, 1).unwrap(); // tried and failed
    let p: Vec<i64> = db
        .phash_pending(false, 100)
        .unwrap()
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(p, vec![id[2]]);
    let all: Vec<i64> = db
        .phash_pending(true, 100)
        .unwrap()
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(all, vec![id[1], id[2]]);
    assert_eq!(db.phash_remaining(false).unwrap(), 1);

    // A changed file loses both columns.
    fs::write(t.path().join("v0.mp4"), vec![9u8; 4321]).unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    let (h, a): (Option<i64>, Option<i64>) = db
        .conn_for_tests()
        .query_row(
            "SELECT phash, phash_algo FROM media WHERE id = ?1",
            [id[0]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((h, a), (None, None));
    assert!(
        db.set_phash(id[0], Some(1), 0).is_err(),
        "algo 0 is refused by CHECK"
    );
}

#[test]
fn commands_work_and_validate() {
    let t = tempfile::tempdir().unwrap();
    let mut db = lib(t.path(), 2);
    let id = ids(&db);
    db.set_phash(id[0], Some(5), 1).unwrap();
    db.set_phash(id[1], Some(5), 1).unwrap();
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let r = reg.run(&mut db, "media.duplicates", json!({})).unwrap();
    assert_eq!(r["groups"].as_array().unwrap().len(), 1);
    assert!(
        reg.run(&mut db, "media.duplicates", json!({ "distance": 17 }))
            .is_err()
    );
    let d = reg
        .run(
            &mut db,
            "media.phash.distance",
            json!({ "a": id[0], "b": id[1] }),
        )
        .unwrap();
    assert_eq!(d["distance"], 0);
    assert!(
        reg.run(
            &mut db,
            "media.phash.distance",
            json!({ "a": 99999, "b": id[1] })
        )
        .is_err()
    );
}
