//! Visual fingerprints of videos, for duplicate review. Read-only: the file
//! is decoded in memory (25 frames, 160 px wide) and only a 64-bit hash is
//! stored in the library database. No frame, sprite or file name is written
//! anywhere, and nothing here runs on the UI thread.

use crate::thumbs;
use gstreamer as gst;
use gstreamer::prelude::*;
use jizzcraft_core::phash::{FRAME_W, FRAMES, PHASH_ALGO, perception_hash, sprite};
use jizzcraft_core::{Db, HasLibrary, Registry};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Time allowed to decode one frame.
const FRAME_TIMEOUT_S: u64 = 10;

/// Perceptual hash of one video: 25 frames spread from 5 % to 95 % of its
/// length. Any failure (no duration, a frame that will not decode, a
/// cancellation) is an error and nothing is stored by this function.
pub fn video_phash(path: &Path, cancel: &AtomicBool) -> Result<u64, String> {
    let p = thumbs::open_paused_width(path, FRAME_W)?;
    let dur = p
        .pipeline
        .query_duration::<gst::ClockTime>()
        .map(|d| d.nseconds())
        .filter(|d| *d > 0)
        .ok_or("unknown duration")?;
    let start = dur as f64 * 0.05;
    let step = dur as f64 * 0.9 / FRAMES as f64;
    let mut frames = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let at = (start + step * i as f64) as u64;
        frames.push(thumbs::frame_at_accurate(&p, at, FRAME_TIMEOUT_S)?);
    }
    let sprite = sprite(&frames).ok_or("bad frames")?;
    Ok(perception_hash(&sprite))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhashReport {
    pub hashed: u64,
    pub failed: u64,
    pub remaining: u64,
    pub cancelled: bool,
}

impl PhashReport {
    pub fn to_json(&self) -> Value {
        json!({
            "hashed": self.hashed, "failed": self.failed,
            "remaining": self.remaining, "cancelled": self.cancelled,
        })
    }
}

/// Hash videos that have no hash yet (with `all`, also those that failed
/// before), in small steps so a cancel is quick. `progress(done, total)`.
pub fn phash_pending(
    db: &mut Db,
    all: bool,
    limit: Option<u64>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> jizzcraft_core::Result<PhashReport> {
    crate::init().map_err(jizzcraft_core::Error::Invalid)?;
    let mut rep = PhashReport::default();
    let mut tried: HashSet<i64> = HashSet::new();
    let budget = limit.unwrap_or(u64::MAX);
    let total = db.phash_remaining(all)?;
    'outer: loop {
        let batch = db.phash_pending(all, 8 + tried.len() as u64)?;
        let batch: Vec<_> = batch
            .into_iter()
            .filter(|(id, _)| !tried.contains(id))
            .take(8)
            .collect();
        if batch.is_empty() {
            break;
        }
        for (id, path) in batch {
            if cancel.load(Ordering::Relaxed) {
                rep.cancelled = true;
                break 'outer;
            }
            if rep.hashed + rep.failed >= budget {
                break 'outer;
            }
            tried.insert(id);
            match video_phash(&path, cancel) {
                Ok(h) => {
                    db.set_phash(id, Some(h), PHASH_ALGO)?;
                    rep.hashed += 1;
                }
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    rep.cancelled = true;
                    break 'outer;
                }
                Err(e) => {
                    // A missing decoder says nothing about the file: try again
                    // later. Anything else is remembered as "tried and failed".
                    if !e.contains("missing GStreamer") && e != crate::guard::REFUSED {
                        db.set_phash(id, None, PHASH_ALGO)?;
                    }
                    rep.failed += 1;
                }
            }
            progress(rep.hashed + rep.failed, total);
        }
    }
    rep.remaining = db.phash_remaining(all)?;
    Ok(rep)
}

/// Commands that need GStreamer: `library.phash`.
pub fn register_media<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "library.phash",
        "Compute visual fingerprints of videos for duplicate review: {all?, limit?}",
        |c, a| {
            let all = a.get("all").and_then(Value::as_bool).unwrap_or(false);
            let limit = a.get("limit").and_then(Value::as_u64);
            let cancel = AtomicBool::new(false);
            let rep = phash_pending(c.db(), all, limit, &cancel, &mut |_, _| {})?;
            Ok(rep.to_json())
        },
    );
}
