//! Content hashes: `oshash` next to the v1 fingerprint.
use jizzcraft_core::fingerprint::{
    fingerprint_pending, oshash_hex, oshash_of, parse_oshash, quick_fp, quick_hashes,
};
use jizzcraft_core::{Db, Filter, Registry, register_core, scan};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p
}

/// Independent reference: sum of LE words of the first and last `chunk` bytes.
fn reference(bytes: &[u8]) -> Option<u64> {
    let size = bytes.len() as u64;
    if size <= 8 {
        return None;
    }
    let chunk = if size < 65536 {
        (size / 8 * 8) as usize
    } else {
        65536
    };
    let mut sum = size;
    for part in [&bytes[..chunk], &bytes[bytes.len() - chunk..]] {
        for w in part.chunks(8) {
            let mut a = [0u8; 8];
            a.copy_from_slice(w);
            sum = sum.wrapping_add(u64::from_le_bytes(a));
        }
    }
    Some(sum)
}

fn hashes(bytes: &[u8]) -> Option<u64> {
    let t = tempfile::tempdir().unwrap();
    let p = write(t.path(), "f.bin", bytes);
    quick_hashes(&p).unwrap().oshash
}

#[test]
fn known_values() {
    assert_eq!(hashes(&vec![0u8; 200_000]), Some(200_000));
    assert_eq!(hashes(&vec![0u8; 1000]), Some(1000));
    let ones = vec![1u8; 200_000];
    assert_eq!(
        hashes(&ones),
        Some(
            16384u64
                .wrapping_mul(0x0101_0101_0101_0101)
                .wrapping_add(200_000)
        )
    );
}

#[test]
fn counting_bytes_match_the_reference() {
    for n in [
        9usize, 15, 16, 1000, 65_535, 65_536, 131_072, 131_073, 300_000,
    ] {
        let bytes: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        assert_eq!(hashes(&bytes), reference(&bytes), "len {n}");
    }
}

#[test]
fn tiny_files() {
    assert_eq!(hashes(&[1u8; 7]), None);
    assert_eq!(hashes(&[1u8; 8]), None);
    // 9 bytes: an 8-byte chunk at each end.
    let b = [1, 2, 3, 4, 5, 6, 7, 8, 9];
    let head = u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
    let tail = u64::from_le_bytes([2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(hashes(&b), Some(9u64.wrapping_add(head).wrapping_add(tail)));
    assert_eq!(oshash_of(8, &[0; 8], &[0; 8]), None);
}

#[test]
fn hex_round_trip() {
    assert_eq!(oshash_hex(0xab), "00000000000000ab");
    assert_eq!(parse_oshash("00000000000000ab"), Some(0xab));
    assert_eq!(parse_oshash("ab"), None);
    assert_eq!(parse_oshash("0000000000000zab"), None);
    assert_eq!(parse_oshash("+00000000000000ab"), None);
}

#[test]
fn fp1_is_unchanged() {
    // Frozen value: the v1 fingerprint of 200 000 zero bytes.
    let t = tempfile::tempdir().unwrap();
    for n in [0usize, 1, 100, 131_072, 131_073, 200_000] {
        let bytes: Vec<u8> = (0..n).map(|i| (i % 13) as u8).collect();
        let p = write(t.path(), "f.bin", &bytes);
        let mut h = blake3::Hasher::new();
        h.update(b"jzfp1");
        h.update(&(n as u64).to_le_bytes());
        if n <= 131_072 {
            h.update(&bytes);
        } else {
            h.update(&bytes[..65536]);
            h.update(&bytes[n - 65536..]);
        }
        let mut want = [0u8; 16];
        want.copy_from_slice(&h.finalize().as_bytes()[..16]);
        assert_eq!(quick_fp(&p).unwrap(), (n as u64, want), "len {n}");
        assert_eq!(quick_hashes(&p).unwrap().fp, want);
    }
}

fn find(db: &Db, name: &str) -> jizzcraft_core::Media {
    db.query(&Filter {
        include_missing: true,
        ..Default::default()
    })
    .unwrap()
    .into_iter()
    .find(|m| m.name == name)
    .unwrap()
}

fn png(path: &Path, salt: u8) {
    image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8 ^ salt, y as u8, salt]))
        .save(path)
        .unwrap();
}

#[test]
fn backfill_rescan_lookup_and_get() {
    let t = tempfile::tempdir().unwrap();
    png(&t.path().join("a.png"), 1);
    png(&t.path().join("b.png"), 2);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, t.path()).unwrap();
    assert!(find(&db, "a.png").oshash.is_none());

    // A row that already has fp1 but no oshash is picked up too.
    let a = find(&db, "a.png").id;
    let (_, fp) = quick_fp(&t.path().join("a.png")).unwrap();
    db.conn_for_tests()
        .execute(
            "UPDATE media SET fp = ?2 WHERE id = ?1",
            rusqlite::params![a, fp.as_slice()],
        )
        .unwrap();
    let cancel = AtomicBool::new(false);
    let rep = fingerprint_pending(&mut db, false, None, &cancel, &mut |_, _| {}).unwrap();
    assert_eq!((rep.hashed, rep.remaining), (2, 0), "{rep:?}");
    let m = find(&db, "a.png");
    let want = quick_hashes(&t.path().join("a.png")).unwrap();
    assert_eq!(m.oshash, want.oshash.map(oshash_hex));
    assert_eq!(
        m.fp.as_deref(),
        Some(jizzcraft_core::fingerprint::fp_hex(&fp).as_str())
    );

    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let got = reg.run(&mut db, "media.get", json!({ "id": a })).unwrap();
    assert_eq!(got["oshash"], json!(m.oshash));
    let by = reg
        .run(&mut db, "media.by_hash", json!({ "oshash": m.oshash }))
        .unwrap();
    assert_eq!(by["ids"], json!([a]));
    let by = reg
        .run(&mut db, "media.by_hash", json!({ "fp": m.fp }))
        .unwrap();
    assert_eq!(by["ids"], json!([a]));
    assert!(reg.run(&mut db, "media.by_hash", json!({})).is_err());
    assert!(
        reg.run(
            &mut db,
            "media.by_hash",
            json!({ "oshash": "00", "fp": m.fp })
        )
        .is_err()
    );
    assert!(
        reg.run(&mut db, "media.by_hash", json!({ "oshash": "zz" }))
            .is_err()
    );

    // A changed file loses both hashes on rescan.
    std::thread::sleep(std::time::Duration::from_millis(20));
    png(&t.path().join("a.png"), 9);
    scan::rescan_all(&mut db).unwrap();
    let m = find(&db, "a.png");
    assert!(m.oshash.is_none() && m.fp.is_none());
}

#[test]
fn export_v3_matches_by_oshash_when_fp_is_absent() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    png(&t.path().join("a.png"), 3);
    let cancel = AtomicBool::new(false);

    let mut db1 = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db1, t.path()).unwrap();
    fingerprint_pending(&mut db1, false, None, &cancel, &mut |_, _| {}).unwrap();
    let id = find(&db1, "a.png").id;
    db1.set_rating(id, 4).unwrap();
    let dest = work.path().join("e.json");
    jizzcraft_core::portability::export_meta(&db1, &dest, &Default::default()).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&fs::read(&dest).unwrap()).unwrap();
    assert_eq!(doc["version"], 3);
    assert_eq!(doc["items"][0]["oshash"], json!(find(&db1, "a.png").oshash));

    // Second library: only the oshash is known.
    let mut db2 = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db2, t.path()).unwrap();
    fingerprint_pending(&mut db2, false, None, &cancel, &mut |_, _| {}).unwrap();
    db2.conn_for_tests()
        .execute("UPDATE media SET fp = NULL", [])
        .unwrap();
    let rep = jizzcraft_core::portability::import_meta(&mut db2, &dest, false).unwrap();
    assert_eq!(
        (rep.matched, rep.applied, rep.unmatched),
        (1, 1, 0),
        "{rep:?}"
    );
    assert_eq!(find(&db2, "a.png").rating, 4);

    // A version 2 file (no oshash) still imports through the fingerprint.
    let mut v2 = doc.clone();
    v2["version"] = json!(2);
    v2["items"][0].as_object_mut().unwrap().remove("oshash");
    let p2 = work.path().join("v2.json");
    fs::write(&p2, v2.to_string()).unwrap();
    let rep = jizzcraft_core::portability::import_meta(&mut db1, &p2, true).unwrap();
    assert_eq!(rep.matched, 1);
}
