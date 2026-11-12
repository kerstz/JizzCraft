//! The frame a video's thumbnail is made from (`media.thumb`, schema v8).

use jizzcraft_core::{Db, Error, Filter, MediaMeta, Registry, register_core, scan};
use serde_json::json;
use std::fs;
use std::path::Path;

fn library(dir: &Path) -> (Db, i64, i64) {
    image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]))
        .save(dir.join("a.png"))
        .unwrap();
    fs::write(dir.join("v.webm"), b"only the name matters here").unwrap();
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, dir).unwrap();
    let items = db.query(&Filter::default()).unwrap();
    let id = |n: &str| items.iter().find(|m| m.name == n).unwrap().id;
    (db, id("a.png"), id("v.webm"))
}

#[test]
fn a_video_keeps_the_frame_it_was_given() {
    let t = tempfile::tempdir().unwrap();
    let (db, _, v) = library(t.path());
    assert_eq!(
        db.get(v).unwrap().unwrap().thumb_ms,
        None,
        "automatic by default"
    );
    db.set_thumb_ms(v, Some(83_500)).unwrap();
    assert_eq!(db.get(v).unwrap().unwrap().thumb_ms, Some(83_500));
    db.set_thumb_ms(v, None).unwrap();
    assert_eq!(db.get(v).unwrap().unwrap().thumb_ms, None);
}

#[test]
fn images_unknown_ids_and_positions_past_the_end_are_refused() {
    let t = tempfile::tempdir().unwrap();
    let (db, a, v) = library(t.path());
    assert!(matches!(
        db.set_thumb_ms(a, Some(0)),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        db.set_thumb_ms(999, Some(0)),
        Err(Error::NotFound(_))
    ));
    db.set_meta(
        v,
        &MediaMeta {
            duration_ms: Some(10_000),
            ..MediaMeta::default()
        },
    )
    .unwrap();
    assert!(
        db.set_thumb_ms(v, Some(10_000)).is_ok(),
        "the end itself is fine"
    );
    assert!(matches!(
        db.set_thumb_ms(v, Some(10_001)),
        Err(Error::Invalid(_))
    ));
    assert!(db.set_thumb_ms(v, Some(u64::MAX)).is_err());
    assert_eq!(db.get(v).unwrap().unwrap().thumb_ms, Some(10_000));
}

#[test]
fn the_command_sets_and_clears_and_checks_its_argument() {
    let t = tempfile::tempdir().unwrap();
    let (mut db, a, v) = library(t.path());
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let out = reg
        .run(&mut db, "media.thumb", json!({ "id": v, "at_ms": 1500 }))
        .unwrap();
    assert_eq!(out, json!({ "id": v, "thumb_ms": 1500 }));
    let got = reg.run(&mut db, "media.get", json!({ "id": v })).unwrap();
    assert_eq!(got["thumb_ms"], 1500);
    for bad in [json!("1500"), json!(-1), json!(1.5)] {
        assert!(
            reg.run(&mut db, "media.thumb", json!({ "id": v, "at_ms": bad }))
                .is_err(),
            "{bad} was accepted"
        );
    }
    assert!(
        reg.run(&mut db, "media.thumb", json!({ "id": a, "at_ms": 0 }))
            .is_err()
    );
    let out = reg
        .run(&mut db, "media.thumb", json!({ "id": v, "at_ms": null }))
        .unwrap();
    assert_eq!(out["thumb_ms"], serde_json::Value::Null);
    reg.run(&mut db, "media.thumb", json!({ "id": v })).unwrap();
    assert_eq!(db.get(v).unwrap().unwrap().thumb_ms, None);
}

/// A library from before frames could be chosen opens with every thumbnail
/// automatic, and its rows deserialise without the field.
#[test]
fn older_rows_and_json_mean_automatic() {
    let t = tempfile::tempdir().unwrap();
    let (db, _, v) = library(t.path());
    let mut m = serde_json::to_value(db.get(v).unwrap().unwrap()).unwrap();
    m.as_object_mut().unwrap().remove("thumb_ms");
    let back: jizzcraft_core::Media = serde_json::from_value(m).unwrap();
    assert_eq!(back.thumb_ms, None);
}
