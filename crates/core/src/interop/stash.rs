//! Stash "JSON export" folders: one JSON file per object under `files/`,
//! `scenes/`, `performers/`, `studios/` and `tags/`. The format is described
//! by Stash's own manual; this module is written from that description and
//! from observed exports, not from Stash's code.
//!
//! Items are matched by content, never by path: a file's size and `oshash`
//! (computed on the spot, read-only, for library files that lack one), or,
//! when the export has no `oshash`, size plus duration (within half a second)
//! plus file name. Several candidates are ambiguous and skipped. Images,
//! covers, URLs and anything else outside the mapping are never read into the
//! library; a URL in a file is never fetched.

use super::{
    ImportOpts, InteropReport, PersonRec, Record, TagRec, apply_record, clean_text, day_start,
    read_capped, walk_files,
};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::fingerprint::{oshash_hex, parse_oshash, quick_hashes};
use crate::meta;
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Size limits of one import. The defaults are what the app uses; tests
/// shrink them.
#[derive(Debug, Clone, Copy)]
pub struct StashCaps {
    pub max_files: usize,
    pub file_bytes: u64,
    pub scene_bytes: u64,
    pub named_bytes: u64,
    pub tag_bytes: u64,
}

impl Default for StashCaps {
    fn default() -> Self {
        StashCaps {
            max_files: 500_000,
            file_bytes: 256 << 10,
            scene_bytes: 16 << 20,
            named_bytes: 4 << 20,
            tag_bytes: 1 << 20,
        }
    }
}

/// Largest scenes' markers read per scene (the applier caps again).
const MAX_MARKERS: usize = 2000;

/// A JSON number, or a string holding one.
fn num_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

/// An integer, or a string holding one; a float only when it is whole.
fn num_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)
                .map(|f| f as i64)
        }),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<i64>().ok().or_else(|| {
                s.parse::<f64>()
                    .ok()
                    .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)
                    .map(|f| f as i64)
            })
        }
        _ => None,
    }
}

/// A perceptual hash as the 64 bits Stash stores. Integers are read exactly,
/// never through a float; strings are 16 hex digits, else decimal.
fn parse_phash(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_i64().map(|i| i as u64).or_else(|| n.as_u64()),
        Value::String(s) => {
            let s = s.trim();
            if s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
                u64::from_str_radix(s, 16).ok()
            } else {
                s.parse::<i64>()
                    .map(|i| i as u64)
                    .ok()
                    .or_else(|| s.parse::<u64>().ok())
            }
        }
        _ => None,
    }
}

/// A name that may be given as text or as an object with a `name`.
fn name_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(name_of).collect(),
        Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// 1 to 5 stars. A value of 1 to 5 is stars already; anything above is the
/// 1 to 100 scale and becomes `ceil(v / 20)`.
fn stars_of(v: &Value) -> Option<u8> {
    let n = num_f64(v)?;
    if n <= 0.0 {
        None
    } else if n <= 5.0 {
        Some((n.round() as i64).clamp(1, 5) as u8)
    } else {
        Some(((n / 20.0).ceil() as i64).clamp(1, 5) as u8)
    }
}

// ---- file shapes -------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct SFp {
    #[serde(rename = "type")]
    kind: String,
    fingerprint: Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SFile {
    path: String,
    size: Value,
    duration: Value,
    fingerprints: Vec<SFp>,
}

struct FileInfo {
    size: i64,
    duration_ms: Option<i64>,
    oshash: Option<u64>,
    phash: Option<u64>,
    basename_lc: String,
}

impl SFile {
    fn info(&self) -> Option<FileInfo> {
        let size = num_i64(&self.size).filter(|s| *s > 0)?;
        let mut oshash = None;
        let mut phash = None;
        for f in &self.fingerprints {
            match f.kind.as_str() {
                "oshash" => {
                    oshash = f.fingerprint.as_str().and_then(parse_oshash).or(oshash);
                }
                "phash" => phash = parse_phash(&f.fingerprint).or(phash),
                _ => {}
            }
        }
        let base = self
            .path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        Some(FileInfo {
            size,
            duration_ms: num_f64(&self.duration)
                .filter(|d| *d >= 0.0)
                .map(|d| (d * 1000.0).round() as i64),
            oshash,
            phash,
            basename_lc: base,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SMarker {
    title: Option<String>,
    seconds: Value,
    primary_tag: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SStashId {
    stash_id: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SScene {
    title: Option<String>,
    date: Option<String>,
    rating: Value,
    studio: Value,
    performers: Value,
    tags: Value,
    markers: Vec<SMarker>,
    files: Vec<Value>,
    resume_time: Value,
    play_count: Value,
    play_history: Vec<String>,
    stash_ids: Vec<SStashId>,
}

/// Fields that exist in a scene but are never imported.
const IGNORED_SCENE_KEYS: &[&str] = &[
    "o_history",
    "organized",
    "code",
    "details",
    "director",
    "urls",
    "url",
    "groups",
    "movies",
    "galleries",
    "custom_fields",
    "cover",
    "production_date",
    "play_duration",
];

#[derive(Default)]
struct Perf {
    disambiguation: String,
    aliases: Vec<String>,
    favorite: bool,
}

// ---- matching ----------------------------------------------------------------

enum Found {
    Items(Vec<i64>),
    Ambiguous,
    None,
}

struct Candidate {
    id: i64,
    size: i64,
    oshash: Option<i64>,
    duration_ms: Option<i64>,
    name_lc: String,
    path: std::path::PathBuf,
}

/// Library candidates with this size, present videos only.
fn candidates(c: &Connection, size: i64) -> Result<Vec<Candidate>> {
    let mut st = c.prepare_cached(
        "SELECT id, size, oshash, duration_ms, name_lc, path
         FROM media WHERE size = ?1 AND kind = 1 AND missing = 0 ORDER BY id",
    )?;
    let rows = st.query_map([size], |r| {
        Ok(Candidate {
            id: r.get(0)?,
            size: r.get(1)?,
            oshash: r.get(2)?,
            duration_ms: r.get(3)?,
            name_lc: r.get(4)?,
            path: crate::db::blob_to_path(r.get(5)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn match_file(c: &Connection, f: &FileInfo) -> Result<Found> {
    let mut cands = candidates(c, f.size)?;
    let ids = if let Some(h) = f.oshash {
        // Library files without an oshash get one now (a read-only look at
        // 128 KiB), kept with the rest of the import.
        for cand in cands.iter_mut().filter(|x| x.oshash.is_none()) {
            if let Ok(hs) = quick_hashes(&cand.path)
                && hs.size as i64 == cand.size
                && let Some(o) = hs.oshash
            {
                c.execute(
                    "UPDATE media SET oshash = ?2 WHERE id = ?1",
                    params![cand.id, o as i64],
                )?;
                cand.oshash = Some(o as i64);
            }
        }
        cands
            .iter()
            .filter(|x| x.oshash == Some(h as i64))
            .map(|x| x.id)
            .collect::<Vec<_>>()
    } else {
        let Some(d) = f.duration_ms else {
            return Ok(Found::None);
        };
        if f.basename_lc.is_empty() {
            return Ok(Found::None);
        }
        cands
            .iter()
            .filter(|x| x.duration_ms.is_some_and(|xd| (xd - d).abs() <= 500))
            .filter(|x| x.name_lc == f.basename_lc)
            .map(|x| x.id)
            .collect()
    };
    Ok(match ids.len() {
        0 => Found::None,
        1 => Found::Items(ids),
        _ => Found::Ambiguous,
    })
}

// ---- reading the folder --------------------------------------------------------

fn read_json(path: &Path, cap: u64) -> Option<Value> {
    let bytes = read_capped(path, cap).ok()?;
    if bytes.len() as u64 > cap {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Import a Stash export folder. Nothing is written outside the database;
/// with `dry_run` nothing is written at all.
pub fn import_stash(
    db: &mut Db,
    dir: &Path,
    o: &ImportOpts,
    cancel: &AtomicBool,
) -> Result<InteropReport> {
    import_stash_with(db, dir, o, cancel, &StashCaps::default())
}

pub fn import_stash_with(
    db: &mut Db,
    dir: &Path,
    o: &ImportOpts,
    cancel: &AtomicBool,
    caps: &StashCaps,
) -> Result<InteropReport> {
    if !dir.join("scenes").is_dir() {
        return Err(Error::Invalid("not a Stash export folder".into()));
    }
    let list = |sub: &str| -> Vec<std::path::PathBuf> {
        let d = dir.join(sub);
        if d.is_dir() {
            walk_files(&d, "json", caps.max_files + 1, 2)
        } else {
            Vec::new()
        }
    };
    let (files, scenes, performers, studios, tags) = (
        list("files"),
        list("scenes"),
        list("performers"),
        list("studios"),
        list("tags"),
    );
    let total = files.len() + scenes.len() + performers.len() + studios.len() + tags.len();
    if total > caps.max_files {
        return Err(Error::Invalid("too many files in the export folder".into()));
    }
    let mut rep = InteropReport {
        dry_run: o.dry_run,
        files_seen: total as u64,
        ..Default::default()
    };

    // The small side files are held in memory; scenes are streamed.
    let mut file_map: HashMap<String, FileInfo> = HashMap::new();
    for p in &files {
        match read_json(p, caps.file_bytes).and_then(|v| serde_json::from_value::<SFile>(v).ok()) {
            Some(f) => {
                rep.parsed += 1;
                if let Some(info) = f.info() {
                    file_map.insert(f.path.trim().to_owned(), info);
                }
            }
            None => rep.rejected += 1,
        }
    }
    let mut perf_map: HashMap<String, Perf> = HashMap::new();
    for p in &performers {
        let Some(v) = read_json(p, caps.named_bytes) else {
            rep.rejected += 1;
            continue;
        };
        rep.parsed += 1;
        if let Some(name) = v.get("name").and_then(Value::as_str) {
            perf_map.insert(
                name.trim().to_lowercase(),
                Perf {
                    disambiguation: clean_text(
                        v.get("disambiguation")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        60,
                    ),
                    aliases: strings(v.get("aliases").unwrap_or(&Value::Null)),
                    favorite: v.get("favorite").and_then(Value::as_bool).unwrap_or(false),
                },
            );
        }
    }
    for p in &studios {
        // Studio files hold nothing we keep beyond the name the scene carries.
        match read_json(p, caps.named_bytes) {
            Some(v) => {
                rep.parsed += 1;
                if v.get("parent_studio").is_some_and(|x| !x.is_null()) {
                    rep.ignored_fields += 1;
                }
            }
            None => rep.rejected += 1,
        }
    }
    let mut tag_aliases: HashMap<String, Vec<String>> = HashMap::new();
    for p in &tags {
        let Some(v) = read_json(p, caps.tag_bytes) else {
            rep.rejected += 1;
            continue;
        };
        rep.parsed += 1;
        if v.get("parents")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            // Flattened: the tree is not imported.
            rep.ignored_fields += 1;
        }
        if let Some(name) = v.get("name").and_then(Value::as_str) {
            tag_aliases.insert(
                name.trim().to_lowercase(),
                strings(v.get("aliases").unwrap_or(&Value::Null)),
            );
        }
    }

    let tx = db.conn.transaction()?;
    for p in &scenes {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        let Some(raw) = read_json(p, caps.scene_bytes) else {
            rep.rejected += 1;
            continue;
        };
        let ignored = IGNORED_SCENE_KEYS
            .iter()
            .filter(|k| {
                raw.get(**k).is_some_and(|v| match v {
                    Value::Null => false,
                    Value::Array(a) => !a.is_empty(),
                    Value::String(s) => !s.is_empty(),
                    Value::Object(m) => !m.is_empty(),
                    Value::Bool(b) => *b,
                    Value::Number(_) => true,
                })
            })
            .count() as u64;
        let Ok(scene) = serde_json::from_value::<SScene>(raw) else {
            rep.rejected += 1;
            continue;
        };
        rep.parsed += 1;
        rep.ignored_fields += ignored;

        let mut infos: Vec<&FileInfo> = Vec::new();
        let mut unresolved = 0u64;
        for f in &scene.files {
            let path = match f {
                Value::String(s) => Some(s.trim().to_owned()),
                Value::Object(m) => m
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_owned()),
                _ => None,
            };
            match path.as_deref().and_then(|p| file_map.get(p)) {
                Some(i) => infos.push(i),
                None => unresolved += 1,
            }
        }
        if scene.files.is_empty() {
            unresolved += 1;
        }
        rep.unmatched += unresolved;

        let record = record_of(&scene, &perf_map, &tag_aliases);
        for info in infos {
            let ids = match match_file(&tx, info)? {
                Found::None => {
                    rep.unmatched += 1;
                    continue;
                }
                Found::Ambiguous => {
                    rep.ambiguous += 1;
                    continue;
                }
                Found::Items(ids) => ids,
            };
            for id in ids {
                rep.matched += 1;
                // The file's own perceptual hash rides along with its own
                // item, as 16 hex digits of the 64 bits.
                let mut rec = record.clone();
                if let Some(h) = info.phash {
                    rec.external_ids.push(("phash.stash".into(), oshash_hex(h)));
                }
                if apply_record(&tx, id, &rec, o, &mut rep)? {
                    rep.applied += 1;
                }
            }
        }
    }
    if o.dry_run || rep.cancelled {
        tx.rollback()?;
    } else {
        tx.commit()?;
    }
    if rep.cancelled {
        rep = InteropReport {
            files_seen: rep.files_seen,
            dry_run: o.dry_run,
            cancelled: true,
            ..Default::default()
        };
    }
    Ok(rep)
}

fn record_of(
    s: &SScene,
    perfs: &HashMap<String, Perf>,
    tag_aliases: &HashMap<String, Vec<String>>,
) -> Record {
    let people: Vec<PersonRec> = strings(&s.performers)
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let p = perfs.get(&name.trim().to_lowercase());
            PersonRec {
                disambiguation: p.map(|p| p.disambiguation.clone()).unwrap_or_default(),
                aliases: p.map(|p| p.aliases.clone()).unwrap_or_default(),
                favorite: p.is_some_and(|p| p.favorite),
                position: Some(i as i64),
                name,
            }
        })
        .collect();
    let tags: Vec<TagRec> = strings(&s.tags)
        .into_iter()
        .map(|name| TagRec {
            aliases: tag_aliases
                .get(&name.trim().to_lowercase())
                .cloned()
                .unwrap_or_default(),
            name,
        })
        .collect();
    let mut external_ids = Vec::new();
    if let Some(id) = s
        .stash_ids
        .first()
        .map(|x| x.stash_id.trim())
        .filter(|x| !x.is_empty())
    {
        external_ids.push(("stashbox".to_owned(), id.to_owned()));
    }
    let markers: Vec<(i64, String)> = s
        .markers
        .iter()
        .take(MAX_MARKERS)
        .filter_map(|m| {
            let sec = num_f64(&m.seconds).filter(|s| *s >= 0.0 && *s < 1.0e7)?;
            let label = [m.title.as_deref(), m.primary_tag.as_deref()]
                .into_iter()
                .flatten()
                .map(|t| clean_text(t, 200))
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            Some(((sec * 1000.0).round() as i64, label))
        })
        .collect();
    let last_played = s.play_history.iter().filter_map(|t| day_start(t)).max();
    let history_count = s.play_history.len().min(u32::MAX as usize) as u32;
    let play_count = num_i64(&s.play_count)
        .map(|n| n.clamp(0, u32::MAX as i64) as u32)
        .unwrap_or(0)
        .max(history_count);
    Record {
        title: s.title.clone(),
        released: s.date.as_deref().and_then(|d| meta::clean_date(d).ok()),
        studios: name_of(&s.studio).into_iter().collect(),
        people,
        tags,
        external_ids,
        stars: stars_of(&s.rating),
        play_count: (play_count > 0).then_some(play_count),
        last_played,
        resume_ms: num_f64(&s.resume_time)
            .filter(|t| *t >= 0.0 && *t < 1.0e7)
            .map(|t| (t * 1000.0).round() as i64),
        markers,
        playlists: Vec::new(),
        ignored: 0,
    }
}
