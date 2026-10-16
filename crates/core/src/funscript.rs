//! Funscript sidecars, display only: a `<stem>.funscript` next to a video is
//! read to draw an intensity heatmap on the timeline. Nothing here talks to a
//! device, writes a file or lets a file name leave the module.

use crate::commands::{HasLibrary, Registry};
use crate::error::Error;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Sidecars bigger than this are ignored (real ones are well under 2 MiB).
pub const MAX_BYTES: u64 = 8 << 20;
/// Hard cap on actions kept, against absurd or hostile files.
pub const MAX_ACTIONS: usize = 200_000;
/// Speed (position units per second) drawn as full intensity.
pub const FULL_SPEED: f32 = 400.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub at_ms: u64,
    /// 0..=100.
    pub pos: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// Strictly increasing in `at_ms`; never empty.
    pub actions: Vec<Action>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    actions: Vec<RawAction>,
}

#[derive(Deserialize)]
struct RawAction {
    #[serde(default)]
    at: Value,
    #[serde(default)]
    pos: Value,
}

/// `<stem>.funscript` in the media's folder, ignoring case. Axis variants such
/// as `name.roll.funscript` are not looked at.
pub fn find_sidecar(media: &Path) -> Option<PathBuf> {
    let (dir, stem) = (media.parent()?, media.file_stem()?.to_str()?.to_lowercase());
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.to_lowercase().strip_suffix(".funscript") == Some(stem.as_str()) {
            found.push(entry.path());
        }
    }
    // Several spellings that differ only by case: pick one, deterministically.
    found.sort();
    found.into_iter().next()
}

fn whole(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 9e15)
                .map(|f| f as i64)
        }),
        _ => None,
    }
}

/// Forgiving parse of the `actions` list. Entries with a negative or
/// non-integer time, or a non-numeric position, are dropped; positions are
/// clamped to 0..=100; the rest is sorted, de-duplicated by time (first wins)
/// and truncated to [`MAX_ACTIONS`]. `None` when nothing usable is left.
pub fn parse(bytes: &[u8]) -> Option<Script> {
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let raw: Raw = serde_json::from_slice(bytes).ok()?;
    let mut actions: Vec<Action> = raw
        .actions
        .iter()
        .filter_map(|a| {
            let at = whole(&a.at).filter(|t| *t >= 0)? as u64;
            let pos = match &a.pos {
                Value::Number(n) => n.as_f64().filter(|f| f.is_finite())?,
                _ => return None,
            };
            Some(Action {
                at_ms: at,
                pos: pos.round().clamp(0.0, 100.0) as u8,
            })
        })
        .collect();
    actions.sort_by_key(|a| a.at_ms);
    actions.dedup_by_key(|a| a.at_ms);
    actions.truncate(MAX_ACTIONS);
    (!actions.is_empty()).then_some(Script { actions })
}

/// Read the sidecar of `media`, if any and usable. Blocking file I/O: never
/// call from a UI thread.
pub fn load_for(media: &Path) -> Option<Script> {
    let path = find_sidecar(media)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(&path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    parse(&buf)
}

/// Mean stroke speed per bucket over `duration_ms`, each value in 0..=1
/// (`FULL_SPEED` units per second and above read as 1). Time before the first
/// and after the last action counts as rest.
pub fn heatmap(script: &Script, duration_ms: u64, buckets: usize) -> Vec<f32> {
    if buckets == 0 || duration_ms == 0 {
        return Vec::new();
    }
    let mut sum = vec![0f64; buckets];
    let bucket_ms = duration_ms as f64 / buckets as f64;
    for w in script.actions.windows(2) {
        let (a, b) = (w[0], w[1]);
        let dt = (b.at_ms - a.at_ms) as f64;
        if dt <= 0.0 || a.at_ms >= duration_ms {
            continue;
        }
        let speed = (f64::from(a.pos) - f64::from(b.pos)).abs() / (dt / 1000.0);
        let (s, e) = (a.at_ms as f64, (b.at_ms.min(duration_ms)) as f64);
        let first = ((s / bucket_ms) as usize).min(buckets - 1);
        let last = (((e / bucket_ms).ceil() as usize).max(1) - 1).min(buckets - 1);
        for (i, cell) in sum.iter_mut().enumerate().take(last + 1).skip(first) {
            let lo = (i as f64 * bucket_ms).max(s);
            let hi = ((i + 1) as f64 * bucket_ms).min(e);
            if hi > lo {
                *cell += speed * (hi - lo);
            }
        }
    }
    sum.into_iter()
        .map(|v| ((v / bucket_ms) as f32 / FULL_SPEED).clamp(0.0, 1.0))
        .collect()
}

pub fn register<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "media.funscript",
        "Read the funscript sidecar of an item for display: {id, buckets?=200 (1..=1000)}",
        |c, a| {
            let id = a
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::Invalid("missing integer argument `id`".into()))?;
            let buckets = match a.get("buckets") {
                None | Some(Value::Null) => 200,
                Some(v) => v
                    .as_u64()
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or_else(|| Error::Invalid("`buckets` must be 1 to 1000".into()))?
                    as usize,
            };
            let m = c
                .db()
                .get(id)?
                .ok_or_else(|| Error::NotFound(format!("media id {id}")))?;
            let Some(script) = load_for(&m.path) else {
                return Ok(json!({ "found": false, "actions": 0, "heatmap": [] }));
            };
            let last = script.actions.last().map_or(0, |a| a.at_ms);
            let dur = m.duration_ms.unwrap_or(last).max(1);
            Ok(json!({
                "found": true,
                "actions": script.actions.len(),
                "heatmap": heatmap(&script, dur, buckets),
            }))
        },
    );
}
