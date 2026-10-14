//! Backup and metadata export/import. Writes only to a path the user picks,
//! never overwrites, and never writes next to media (media extensions are
//! refused). Exports are keyed by content fingerprint, not by path.

use crate::db::{Db, now_secs};
use crate::error::{Error, Result};
use crate::fingerprint::parse_fp;
use crate::model::classify;
use rusqlite::params;
use serde::Serialize;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

const FORMAT: &str = "jizzcraft-meta";
const VERSION: u64 = 1;

fn check_dest(dest: &Path) -> Result<()> {
    if dest.as_os_str().is_empty() {
        return Err(Error::Invalid("empty destination".into()));
    }
    if dest.exists() {
        return Err(Error::Invalid("destination already exists".into()));
    }
    if classify(dest).is_some() {
        return Err(Error::Invalid(
            "destination looks like a media file; pick another name".into(),
        ));
    }
    match dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(p) if !p.is_dir() => Err(Error::Invalid("destination folder does not exist".into())),
        _ => Ok(()),
    }
}

/// Consistent online copy of the library (`VACUUM INTO`). Returns bytes written.
pub fn backup(db: &Db, dest: &Path) -> Result<u64> {
    check_dest(dest)?;
    let d = dest
        .to_str()
        .ok_or_else(|| Error::Invalid("destination path is not valid UTF-8".into()))?;
    db.conn.execute("VACUUM INTO ?1", [d])?;
    Ok(std::fs::metadata(dest)?.len())
}

#[derive(Debug, Clone, Default)]
pub struct ExportOpts {
    /// Include play counts, last played and resume points.
    pub include_history: bool,
    /// Include file names (they can be sensitive).
    pub include_names: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ExportReport {
    pub items: u64,
    /// Items with user data but no fingerprint yet: not exportable.
    pub skipped_no_fp: u64,
}

pub fn export_meta(db: &Db, dest: &Path, o: &ExportOpts) -> Result<ExportReport> {
    check_dest(dest)?;
    let mut rep = ExportReport::default();
    let mut items: Vec<Value> = Vec::new();
    let mut st = db.conn.prepare(
        "SELECT id, fp, size, duration_ms, name, favorite, rating, hidden,
                resume_ms, play_count, last_played_at
         FROM media
         WHERE favorite = 1 OR rating > 0 OR hidden = 1 OR play_count > 0
            OR resume_ms IS NOT NULL
            OR EXISTS (SELECT 1 FROM markers k WHERE k.media_id = media.id)
         ORDER BY id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<Vec<u8>>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, Option<i64>>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, Option<i64>>(10)?,
        ))
    })?;
    for row in rows {
        let (id, fp, size, dur, name, fav, rating, hidden, resume, plays, last) = row?;
        let Some(fp) = fp else {
            rep.skipped_no_fp += 1;
            continue;
        };
        let mut it = json!({
            "fp": crate::db::fp_string(&fp),
            "size": size,
            "favorite": fav != 0,
            "rating": rating,
            "hidden": hidden != 0,
            "markers": db.markers(id)?.into_iter()
                .map(|m| json!({ "pos_ms": m.pos_ms, "label": m.label }))
                .collect::<Vec<_>>(),
        });
        if let Some(d) = dur {
            it["duration_ms"] = json!(d);
        }
        if o.include_names {
            it["name"] = json!(name);
        }
        if o.include_history {
            it["resume_ms"] = json!(resume);
            it["play_count"] = json!(plays);
            it["last_played_at"] = json!(last);
        }
        items.push(it);
        rep.items += 1;
    }
    let views: Vec<Value> = db
        .views()?
        .into_iter()
        .map(|v| json!({ "name": v.name, "spec": v.spec }))
        .collect();
    let doc = json!({
        "format": FORMAT,
        "version": VERSION,
        "exported_at": now_secs(),
        "items": items,
        "views": views,
    });
    let mut f = OpenOptions::new().write(true).create_new(true).open(dest)?;
    f.write_all(serde_json::to_string_pretty(&doc).expect("json").as_bytes())?;
    Ok(rep)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ImportReport {
    pub matched: u64,
    pub applied: u64,
    pub unmatched: u64,
    /// Fingerprint matched several rows: skipped rather than guessed.
    pub ambiguous: u64,
    pub views: u64,
}

pub fn import_meta(db: &mut Db, src: &Path, dry_run: bool) -> Result<ImportReport> {
    let text = std::fs::read_to_string(src)?;
    let doc: Value =
        serde_json::from_str(&text).map_err(|e| Error::Invalid(format!("not a JSON file: {e}")))?;
    if doc["format"] != FORMAT {
        return Err(Error::Invalid("not a JizzCraft metadata export".into()));
    }
    if doc["version"].as_u64() != Some(VERSION) {
        return Err(Error::Invalid("unsupported export version".into()));
    }
    let mut rep = ImportReport::default();
    let tx = db.conn.transaction()?;
    for it in doc["items"].as_array().into_iter().flatten() {
        let fp = it["fp"].as_str().and_then(parse_fp);
        let size = it["size"].as_i64();
        let (Some(fp), Some(size)) = (fp, size) else {
            rep.unmatched += 1;
            continue;
        };
        let ids: Vec<i64> = {
            let mut st = tx.prepare("SELECT id FROM media WHERE fp = ?1 AND size = ?2")?;
            let rows = st.query_map(params![fp.as_slice(), size], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        match ids.as_slice() {
            [] => rep.unmatched += 1,
            [id] => {
                rep.matched += 1;
                if dry_run {
                    continue;
                }
                let rating = it["rating"].as_u64().unwrap_or(0).min(5) as i64;
                tx.execute(
                    "UPDATE media SET
                        favorite = MAX(favorite, ?2), hidden = MAX(hidden, ?3),
                        rating = MAX(rating, ?4),
                        play_count = MAX(play_count, ?5),
                        resume_ms = COALESCE(resume_ms, ?6),
                        last_played_at = MAX(COALESCE(last_played_at, 0), ?7)
                     WHERE id = ?1",
                    params![
                        id,
                        it["favorite"].as_bool().unwrap_or(false) as i64,
                        it["hidden"].as_bool().unwrap_or(false) as i64,
                        rating,
                        it["play_count"].as_i64().unwrap_or(0).max(0),
                        it["resume_ms"].as_i64().filter(|v| *v >= 0),
                        it["last_played_at"].as_i64().unwrap_or(0).max(0),
                    ],
                )?;
                tx.execute(
                    "UPDATE media SET last_played_at = NULL WHERE id = ?1 AND last_played_at = 0",
                    [id],
                )?;
                for m in it["markers"].as_array().into_iter().flatten() {
                    let (Some(pos), label) = (
                        m["pos_ms"].as_i64().filter(|v| *v >= 0),
                        m["label"].as_str(),
                    ) else {
                        continue;
                    };
                    let label: String = label
                        .unwrap_or("")
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(200)
                        .collect();
                    tx.execute(
                        "INSERT OR IGNORE INTO markers(media_id, pos_ms, label, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, pos, label, now_secs()],
                    )?;
                }
                rep.applied += 1;
            }
            _ => rep.ambiguous += 1,
        }
    }
    if !dry_run {
        for v in doc["views"].as_array().into_iter().flatten() {
            let (Some(name), Some(spec)) = (v["name"].as_str(), v.get("spec")) else {
                continue;
            };
            // Validate through the same whitelist as every other entry point.
            if let Ok(f) = crate::db::Filter::from_json(spec) {
                let spec = f.to_json().to_string();
                let n = name.trim();
                if !n.is_empty() && n.chars().count() <= 60 && !n.chars().any(char::is_control) {
                    tx.execute(
                        "INSERT INTO saved_views(name, spec, created_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(name) DO NOTHING",
                        params![n, spec, now_secs()],
                    )?;
                    rep.views += 1;
                }
            }
        }
        tx.commit()?;
    }
    Ok(rep)
}
