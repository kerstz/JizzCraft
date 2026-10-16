//! Backup and metadata export/import. Writes only to a path the user picks,
//! never overwrites, and never writes next to media (media extensions are
//! refused). Exports are keyed by content fingerprint, not by path.

use crate::db::{Db, now_secs};
use crate::error::{Error, Result};
use crate::fingerprint::parse_fp;
use crate::model::classify;
use crate::organize::LabelKind;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

const FORMAT: &str = "jizzcraft-meta";
/// Written version. Versions 1 and 2 still import (no tags and playlists in 1; no `oshash` before 3).
const VERSION: u64 = 3;

pub(crate) fn check_dest(dest: &Path) -> Result<()> {
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
///
/// From an encrypted library the copy is encrypted with the same key (checked
/// by `tests/vault_db.rs`), and the vault header is copied next to it as
/// `dest` with the extension `vault` (so `copy.db` gets `copy.vault`, which is
/// where `Db::open_with` and the vault tools look for it). The size returned
/// is the database only.
pub fn backup(db: &Db, dest: &Path) -> Result<u64> {
    use crate::vault::{VaultError, header_path};
    check_dest(dest)?;
    let d = dest
        .to_str()
        .ok_or_else(|| Error::Invalid("destination path is not valid UTF-8".into()))?;
    let header = if db.encrypted {
        let src = db
            .path
            .as_deref()
            .map(header_path)
            .ok_or(Error::Vault(VaultError::HeaderMissing))?;
        let dst = header_path(dest);
        if dst.exists() {
            return Err(Error::Invalid("destination already exists".into()));
        }
        // Read it first: a backup nobody can open is worse than none.
        let bytes = std::fs::read(&src).map_err(|_| Error::Vault(VaultError::HeaderMissing))?;
        crate::vault::validate_header(&bytes)?;
        Some((dst, bytes))
    } else {
        None
    };
    // Private from the start: an empty 0600 file is a valid VACUUM INTO target.
    crate::fsutil::precreate_private(dest);
    db.conn.execute("VACUUM INTO ?1", [d])?;
    let len = std::fs::metadata(dest)?.len();
    if let Some((dst, bytes)) = header {
        let write = || -> std::io::Result<()> {
            let mut f = crate::fsutil::private_file_options()
                .write(true)
                .create_new(true)
                .open(&dst)?;
            f.write_all(&bytes)?;
            f.sync_all()
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(dest);
            let _ = std::fs::remove_file(&dst);
            return Err(e.into());
        }
    }
    Ok(len)
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
    pub playlists: u64,
    /// Items with user data but no fingerprint yet: not exportable.
    pub skipped_no_fp: u64,
}

pub fn export_meta(db: &Db, dest: &Path, o: &ExportOpts) -> Result<ExportReport> {
    export_meta_ext(db, dest, o, false)
}

/// `include_private` also exports playlists, tags and categories flagged
/// private (a category is private when any ancestor is).
pub fn export_meta_ext(
    db: &Db,
    dest: &Path,
    o: &ExportOpts,
    include_private: bool,
) -> Result<ExportReport> {
    use crate::organize::{LabelInfo, LabelKind};
    use std::collections::HashMap;
    check_dest(dest)?;
    let mut rep = ExportReport::default();
    // Labels visible in this export, keyed by id.
    let mut vis_tags: HashMap<i64, LabelInfo> = HashMap::new();
    let mut vis_cats: HashMap<i64, LabelInfo> = HashMap::new();
    let mut label_docs: Vec<Value> = Vec::new();
    for t in db.labels(LabelKind::Tag)? {
        if include_private || !t.private {
            label_docs.push(json!({
                "kind": "tag", "path": t.name, "color": t.color, "private": t.private
            }));
            vis_tags.insert(t.id, t);
        }
    }
    {
        let all = db.labels(LabelKind::Category)?;
        let private_ids: std::collections::HashSet<i64> =
            all.iter().filter(|l| l.private).map(|l| l.id).collect();
        let parent: HashMap<i64, Option<i64>> = all.iter().map(|l| (l.id, l.parent_id)).collect();
        for l in all {
            let mut hidden = false;
            let mut cur = Some(l.id);
            for _ in 0..8 {
                match cur {
                    Some(id) if private_ids.contains(&id) => {
                        hidden = true;
                        break;
                    }
                    Some(id) => cur = parent.get(&id).copied().flatten(),
                    None => break,
                }
            }
            if include_private || !hidden {
                label_docs.push(json!({
                    "kind": "category", "path": l.path, "color": l.color, "private": l.private
                }));
                vis_cats.insert(l.id, l);
            }
        }
    }
    let mut item_tags: HashMap<i64, Vec<String>> = HashMap::new();
    let mut item_cats: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut st = db
            .conn
            .prepare("SELECT media_id, label_id FROM media_labels ORDER BY media_id, label_id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (m, l) = row?;
            if let Some(t) = vis_tags.get(&l) {
                item_tags.entry(m).or_default().push(t.name.clone());
            } else if let Some(c) = vis_cats.get(&l) {
                item_cats.entry(m).or_default().push(c.path.clone());
            }
        }
    }
    let mut items: Vec<Value> = Vec::new();
    let mut st = db.conn.prepare(
        "SELECT id, fp, size, duration_ms, name, favorite, rating, hidden,
                resume_ms, play_count, last_played_at, oshash
         FROM media
         WHERE favorite = 1 OR rating > 0 OR hidden = 1 OR play_count > 0
            OR resume_ms IS NOT NULL
            OR EXISTS (SELECT 1 FROM markers k WHERE k.media_id = media.id)
            OR EXISTS (SELECT 1 FROM media_labels ml WHERE ml.media_id = media.id)
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
            r.get::<_, Option<i64>>(11)?,
        ))
    })?;
    for row in rows {
        let (id, fp, size, dur, name, fav, rating, hidden, resume, plays, last, oshash) = row?;
        let tags = item_tags.remove(&id).unwrap_or_default();
        let cats = item_cats.remove(&id).unwrap_or_default();
        let markers: Vec<Value> = db
            .markers(id)?
            .into_iter()
            .map(|m| json!({ "pos_ms": m.pos_ms, "label": m.label }))
            .collect();
        let has_data = fav != 0
            || rating > 0
            || hidden != 0
            || plays > 0
            || resume.is_some()
            || !markers.is_empty();
        if !has_data && tags.is_empty() && cats.is_empty() {
            // Only private labels: nothing to say about this item.
            continue;
        }
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
            "markers": markers,
            "tags": tags,
            "categories": cats,
        });
        if let Some(h) = oshash {
            it["oshash"] = json!(crate::fingerprint::oshash_hex(h as u64));
        }
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
    let mut playlists: Vec<Value> = Vec::new();
    for p in db.playlists()? {
        if p.private && !include_private {
            continue;
        }
        let mut st = db.conn.prepare(
            "SELECT m.fp FROM playlist_items i JOIN media m ON m.id = i.media_id
             WHERE i.playlist_id = ?1 ORDER BY i.pos, i.added_at, i.media_id",
        )?;
        let fps: Vec<String> = st
            .query_map([p.id], |r| r.get::<_, Option<Vec<u8>>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .map(|b| crate::db::fp_string(&b))
            .collect();
        playlists.push(json!({
            "name": p.name, "color": p.color, "private": p.private, "items": fps,
        }));
        rep.playlists += 1;
    }
    let doc = json!({
        "format": FORMAT,
        "version": VERSION,
        "exported_at": now_secs(),
        "items": items,
        "views": views,
        "labels": label_docs,
        "playlists": playlists,
    });
    let mut f = crate::fsutil::private_file_options()
        .write(true)
        .create_new(true)
        .open(dest)?;
    f.write_all(serde_json::to_string_pretty(&doc).expect("json").as_bytes())?;
    Ok(rep)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ImportReport {
    pub matched: u64,
    pub applied: u64,
    pub unmatched: u64,
    /// Fingerprint matched several rows: skipped rather than guessed.
    /// Identical twins get no presents rather than the wrong ones.
    pub ambiguous: u64,
    pub views: u64,
    /// Playlists created or merged into.
    pub playlists: u64,
    /// Tags and categories created.
    pub labels: u64,
}

pub fn import_meta(db: &mut Db, src: &Path, dry_run: bool) -> Result<ImportReport> {
    let text = std::fs::read_to_string(src)?;
    let doc: Value =
        serde_json::from_str(&text).map_err(|e| Error::Invalid(format!("not a JSON file: {e}")))?;
    if doc["format"] != FORMAT {
        return Err(Error::Invalid("not a JizzCraft metadata export".into()));
    }
    if !matches!(doc["version"].as_u64(), Some(1..=3)) {
        return Err(Error::Invalid("unsupported export version".into()));
    }
    let mut rep = ImportReport::default();
    let tx = db.conn.transaction()?;
    if !dry_run {
        // Label metadata first, so private flags and colours carry over.
        for l in doc["labels"].as_array().into_iter().flatten() {
            let color = l["color"]
                .as_u64()
                .filter(|c| *c <= 0xFF_FFFF)
                .map(|c| c as u32);
            let private = l["private"].as_bool().unwrap_or(false);
            let Some(path) = l["path"].as_str() else {
                continue;
            };
            match l["kind"].as_str() {
                Some("tag") => {
                    ensure_label(&tx, LabelKind::Tag, path, Some((color, private)), &mut rep);
                }
                Some("category") => {
                    ensure_label(
                        &tx,
                        LabelKind::Category,
                        path,
                        Some((color, private)),
                        &mut rep,
                    );
                }
                _ => {}
            }
        }
    }
    for it in doc["items"].as_array().into_iter().flatten() {
        let fp = it["fp"].as_str().and_then(parse_fp);
        let oshash = it["oshash"]
            .as_str()
            .and_then(crate::fingerprint::parse_oshash);
        let Some(size) = it["size"].as_i64() else {
            rep.unmatched += 1;
            continue;
        };
        // The fingerprint first; the oshash (with the size) only when the
        // fingerprint finds nothing.
        let mut ids: Vec<i64> = Vec::new();
        if let Some(fp) = fp {
            let mut st = tx.prepare("SELECT id FROM media WHERE fp = ?1 AND size = ?2")?;
            let rows = st.query_map(params![fp.as_slice(), size], |r| r.get(0))?;
            ids = rows.collect::<std::result::Result<_, _>>()?;
        }
        if ids.is_empty()
            && let Some(h) = oshash
        {
            let mut st = tx.prepare("SELECT id FROM media WHERE oshash = ?1 AND size = ?2")?;
            let rows = st.query_map(params![h as i64, size], |r| r.get(0))?;
            ids = rows.collect::<std::result::Result<_, _>>()?;
        }
        if fp.is_none() && oshash.is_none() {
            rep.unmatched += 1;
            continue;
        }
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
                for (key, kind) in [
                    ("tags", LabelKind::Tag),
                    ("categories", LabelKind::Category),
                ] {
                    for name in it[key].as_array().into_iter().flatten() {
                        let Some(name) = name.as_str() else { continue };
                        if let Some(l) = ensure_label(&tx, kind, name, None, &mut rep) {
                            let _ = crate::organize::tx_label_assign(&tx, kind, l, *id);
                        }
                    }
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
        for p in doc["playlists"].as_array().into_iter().flatten() {
            let Some(name) = p["name"].as_str() else {
                continue;
            };
            let Ok((name, lc)) = crate::organize::clean_name(name) else {
                continue;
            };
            let existing: Option<i64> = tx
                .query_row("SELECT id FROM playlists WHERE name_lc = ?1", [&lc], |r| {
                    r.get(0)
                })
                .optional()?;
            let id = match existing {
                Some(id) => id,
                None => {
                    let n: i64 =
                        tx.query_row("SELECT COUNT(*) FROM playlists", [], |r| r.get(0))?;
                    if n >= crate::organize::MAX_PLAYLISTS {
                        continue;
                    }
                    let color = p["color"].as_i64().filter(|c| (0..=0xFF_FFFF).contains(c));
                    let now = now_secs();
                    tx.execute(
                        "INSERT INTO playlists(name, name_lc, color, private, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![name, lc, color, p["private"].as_bool().unwrap_or(false), now],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            // Same order as the file; unknown or ambiguous fingerprints are skipped.
            let mut ids: Vec<i64> = Vec::new();
            for fp in p["items"].as_array().into_iter().flatten() {
                let Some(fp) = fp.as_str().and_then(parse_fp) else {
                    continue;
                };
                let found: Vec<i64> = {
                    let mut st = tx.prepare_cached("SELECT id FROM media WHERE fp = ?1 LIMIT 2")?;
                    let rows = st.query_map([fp.as_slice()], |r| r.get(0))?;
                    rows.collect::<std::result::Result<_, _>>()?
                };
                if let [one] = found.as_slice() {
                    ids.push(*one);
                }
            }
            for chunk in ids.chunks(crate::organize::MAX_IDS) {
                // Already-present items are skipped; the rest go to the end.
                if crate::organize::tx_playlist_add(&tx, id, chunk).is_err() {
                    break;
                }
            }
            rep.playlists += 1;
        }
        tx.commit()?;
    }
    Ok(rep)
}

/// Find or create a tag or a category path (`A/B`). Returns the leaf id.
/// `meta` (colour, private) applies only to a label created by this call.
fn ensure_label(
    tx: &rusqlite::Connection,
    kind: LabelKind,
    path: &str,
    meta: Option<(Option<u32>, bool)>,
    rep: &mut ImportReport,
) -> Option<i64> {
    let parts: Vec<&str> = match kind {
        LabelKind::Tag => vec![path],
        LabelKind::Category => path.split('/').collect(),
    };
    let mut parent: Option<i64> = None;
    let last = parts.len() - 1;
    for (i, part) in parts.into_iter().enumerate() {
        let (name, lc) = crate::organize::clean_name(part).ok()?;
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM labels WHERE kind = ?1 AND IFNULL(parent_id, 0) = ?2
                 AND name_lc = ?3",
                params![
                    if kind == LabelKind::Tag { 2 } else { 1 },
                    parent.unwrap_or(0),
                    lc
                ],
                |r| r.get(0),
            )
            .optional()
            .ok()?;
        let id = match found {
            Some(id) => id,
            None => {
                let (color, private) = match meta {
                    Some(m) if i == last => m,
                    _ => (None, false),
                };
                let id = crate::organize::tx_label_create(tx, kind, &name, parent, color).ok()?;
                if private {
                    crate::organize::tx_label_set_private(tx, id, true).ok()?;
                }
                rep.labels += 1;
                id
            }
        };
        parent = Some(id);
    }
    parent
}
