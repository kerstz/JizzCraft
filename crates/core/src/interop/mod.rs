//! Reading other tools' metadata files. Imports are read-only (files are
//! opened, never written), bounded in size and count, dry-run by default and
//! report counts only. A URL found in a file is never fetched. Nothing in this
//! module prints or logs: file names and paths never leave it.
//!
//! A dry run is the same code path as an import, inside one transaction that
//! is rolled back, so its counts are exact.

pub mod edl;
pub mod nfo;
pub mod nfo_export;
pub mod stash;

use crate::commands::{HasLibrary, Registry};
use crate::error::{Error, Result};
use crate::meta::{self, Resolved};
use crate::organize;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Files visited per import.
pub const MAX_FILES: usize = 100_000;
/// Directory levels below the starting folder.
pub const MAX_DEPTH: usize = 32;

/// What to change. Everything is "fill": existing values win unless
/// `overwrite` says otherwise; favorite and hidden are never touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOpts {
    /// Count what would change and change nothing. On by default.
    pub dry_run: bool,
    /// Replace an existing title, release date, studio and rating.
    pub overwrite: bool,
    /// Import star ratings (only onto unrated items unless `overwrite`).
    pub ratings: bool,
    /// Import play counts, last played dates and resume points.
    pub history: bool,
    /// Put items into playlists named after their NFO `<set>`.
    pub playlists: bool,
}

impl Default for ImportOpts {
    fn default() -> Self {
        ImportOpts {
            dry_run: true,
            overwrite: false,
            ratings: false,
            history: false,
            playlists: false,
        }
    }
}

/// Counts only: no names, no paths.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct InteropReport {
    pub files_seen: u64,
    pub parsed: u64,
    pub rejected: u64,
    pub matched: u64,
    /// Matched items on which something changed.
    pub applied: u64,
    pub unmatched: u64,
    pub ambiguous: u64,
    pub titles_set: u64,
    pub studios_created: u64,
    pub studios_from_show: u64,
    pub playlists_created: u64,
    pub people_created: u64,
    pub tags_created: u64,
    pub links_added: u64,
    pub markers_added: u64,
    pub ratings_set: u64,
    pub ignored_fields: u64,
    pub dry_run: bool,
    /// Stopped by the user: nothing was changed.
    pub cancelled: bool,
}

/// Single cleaned line of text: control characters dropped, trimmed, at most
/// `max` characters.
pub fn clean_text(s: &str, max: usize) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).collect();
    t.trim().chars().take(max).collect()
}

/// Reads at most `max + 1` bytes of a regular file. The extra byte lets the
/// caller tell "exactly at the cap" from "over it".
pub fn read_capped(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut buf = Vec::new();
    f.take(max + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Regular files below `root` with extension `ext` (case-insensitive), in
/// name order. Symbolic links are not followed, hidden entries are skipped,
/// and the walk stops at `max_files` files or `max_depth` levels.
pub fn walk_files(root: &Path, ext: &str, max_files: usize, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(max_depth)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || !e.file_name().to_str().is_some_and(|n| n.starts_with('.'))
        });
    for e in walker.flatten() {
        if !e.file_type().is_file() {
            continue;
        }
        let matches = e
            .path()
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case(ext));
        if matches {
            out.push(e.into_path());
            if out.len() >= max_files {
                break;
            }
        }
    }
    out
}

/// `[0, 1, 2]` clamp of Kodi's 0-10 `userrating` onto 0-5 stars.
pub(crate) fn stars_from_ten(v: f32) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    (v / 2.0).round().clamp(0.0, 5.0) as u8
}

fn opt_flag(a: &Value, k: &str) -> Result<Option<bool>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| Error::Invalid(format!("`{k}` must be a boolean"))),
    }
}

/// Import options from command arguments; `dry_run` defaults to true.
pub fn opts_from_args(a: &Value) -> Result<ImportOpts> {
    Ok(ImportOpts {
        dry_run: opt_flag(a, "dry_run")?.unwrap_or(true),
        overwrite: opt_flag(a, "overwrite")?.unwrap_or(false),
        ratings: opt_flag(a, "ratings")?.unwrap_or(false),
        history: opt_flag(a, "history")?.unwrap_or(false),
        playlists: opt_flag(a, "playlists")?.unwrap_or(false),
    })
}

pub fn register<C: HasLibrary>(reg: &mut Registry<C>) {
    nfo_export::register(reg);
    reg.register(
        "meta.import_stash",
        "Read a Stash JSON export folder onto matching items (dry run unless dry_run is false): {dir, dry_run?=true, overwrite?, ratings?, history?}",
        |c, a| {
            let o = opts_from_args(&a)?;
            let dir = a
                .get("dir")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| Error::Invalid("missing string argument `dir`".into()))?;
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let rep = stash::import_stash(c.db(), &dir, &o, &cancel)?;
            Ok(json!(rep))
        },
    );
    reg.register(
        "meta.import_nfo",
        "Read Kodi-style .nfo files onto matching items (dry run unless dry_run is false): {dir?, dry_run?=true, overwrite?, ratings?, history?, playlists?}",
        |c, a| {
            let o = opts_from_args(&a)?;
            let dir = a.get("dir").and_then(Value::as_str).map(PathBuf::from);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let rep = nfo::import_nfo(c.db(), dir.as_deref(), &o, &cancel)?;
            Ok(json!(rep))
        },
    );
    reg.register(
        "meta.import_edl",
        "Read Kodi .edl files as markers (cut, mute, scene) on the same-named video (dry run unless dry_run is false): {dir?, dry_run?=true}",
        |c, a| {
            let o = ImportOpts {
                dry_run: opt_flag(&a, "dry_run")?.unwrap_or(true),
                ..ImportOpts::default()
            };
            let dir = a.get("dir").and_then(Value::as_str).map(PathBuf::from);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let rep = edl::import_edl(c.db(), dir.as_deref(), &o, &cancel)?;
            Ok(json!(rep))
        },
    );
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Midnight UTC of the `YYYY-MM-DD` at the start of `s`.
pub(crate) fn day_start(s: &str) -> Option<i64> {
    let day = s.trim().get(..10)?;
    let day = meta::clean_date(day).ok()?;
    let (y, m, d) = (
        day[..4].parse::<i64>().ok()?,
        day[5..7].parse::<i64>().ok()?,
        day[8..].parse::<i64>().ok()?,
    );
    Some(days_from_civil(y, m, d) * 86_400)
}

// ---- what every source boils down to ----------------------------------------

#[derive(Default, Clone)]
pub(crate) struct PersonRec {
    pub name: String,
    pub disambiguation: String,
    pub aliases: Vec<String>,
    /// Applied only when the performer is created by the import.
    pub favorite: bool,
    pub position: Option<i64>,
}

#[derive(Default, Clone)]
pub(crate) struct TagRec {
    pub name: String,
    pub aliases: Vec<String>,
}

/// One item's worth of metadata, whichever format it came from.
#[derive(Default, Clone)]
pub(crate) struct Record {
    pub title: Option<String>,
    /// A valid `YYYY-MM-DD`.
    pub released: Option<String>,
    pub studios: Vec<String>,
    pub people: Vec<PersonRec>,
    pub tags: Vec<TagRec>,
    pub external_ids: Vec<(String, String)>,
    /// 1 to 5.
    pub stars: Option<u8>,
    pub play_count: Option<u32>,
    pub last_played: Option<i64>,
    pub resume_ms: Option<i64>,
    pub markers: Vec<(i64, String)>,
    /// Playlist names (applied only with `ImportOpts::playlists`).
    pub playlists: Vec<String>,
    pub ignored: u64,
}

/// Performers applied to one item (the model's own cap is 64).
const MAX_PEOPLE_APPLIED: usize = 64;
const MAX_MARKERS_APPLIED: usize = 500;

/// Apply a record to one item by the "fill" policy. `Ok(true)` when anything
/// changed.
pub(crate) fn apply_record(
    c: &Connection,
    id: i64,
    r: &Record,
    o: &ImportOpts,
    rep: &mut InteropReport,
) -> Result<bool> {
    let mut changed = false;
    // Title.
    if let Some(t) = r.title.as_deref().and_then(|t| meta::clean_title(t).ok()) {
        let cur: Option<String> =
            c.query_row("SELECT title FROM media WHERE id = ?1", [id], |r| r.get(0))?;
        if cur.as_deref() != Some(t.as_str()) && (cur.is_none() || o.overwrite) {
            c.execute("UPDATE media SET title = ?2 WHERE id = ?1", params![id, t])?;
            rep.titles_set += 1;
            changed = true;
        }
    }
    // Release date.
    if let Some(d) = &r.released {
        let n = if o.overwrite {
            c.execute(
                "UPDATE media SET released = ?2 WHERE id = ?1 AND released IS NOT ?2",
                params![id, d],
            )?
        } else {
            c.execute(
                "UPDATE media SET released = ?2 WHERE id = ?1 AND released IS NULL",
                params![id, d],
            )?
        };
        changed |= n > 0;
    }
    // Studio: the first one; the rest are ignored.
    if let Some(name) = r.studios.first() {
        let cur: Option<i64> =
            c.query_row("SELECT studio_id FROM media WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        if (cur.is_none() || o.overwrite)
            && let Ok((sid, created)) = meta::tx_resolve_studio(c, name)
        {
            rep.studios_created += created as u64;
            if cur != Some(sid) {
                c.execute(
                    "UPDATE media SET studio_id = ?2 WHERE id = ?1",
                    params![id, sid],
                )?;
                changed = true;
            }
        }
    }
    rep.ignored_fields += r.studios.len().saturating_sub(1) as u64;
    // Performers.
    for p in r.people.iter().take(MAX_PEOPLE_APPLIED) {
        let pid = match meta::tx_resolve_person(c, &p.name, &p.disambiguation) {
            Ok(Resolved::Found(pid)) => pid,
            Ok(Resolved::Created(pid)) => {
                rep.people_created += 1;
                if p.favorite {
                    c.execute("UPDATE people SET favorite = 1 WHERE id = ?1", [pid])?;
                }
                pid
            }
            Ok(Resolved::Ambiguous) => {
                rep.ignored_fields += 1;
                continue;
            }
            Err(_) => continue,
        };
        for a in p.aliases.iter().take(32) {
            // Over the cap or already used: skipped.
            let _ = meta::person_alias_add_tx(c, pid, a);
        }
        if meta::tx_person_link(c, id, pid, p.position).unwrap_or(false) {
            rep.links_added += 1;
            changed = true;
        }
    }
    // Tags, through aliases.
    for t in &r.tags {
        let Ok((lid, created)) = meta::tx_resolve_tag(c, &t.name) else {
            continue;
        };
        rep.tags_created += created as u64;
        for a in t.aliases.iter().take(32) {
            let _ = meta::tag_alias_add(c, lid, a);
        }
        let n = c.execute(
            "INSERT OR IGNORE INTO media_labels(label_id, media_id, added_at)
             SELECT ?1, id, ?3 FROM media WHERE id = ?2",
            params![lid, id, crate::db::now_secs()],
        )?;
        if n > 0 {
            rep.links_added += 1;
            changed = true;
        }
    }
    for (kind, value) in &r.external_ids {
        if meta::tx_external_id_add(c, id, kind, value)? {
            changed = true;
        }
    }
    for (pos_ms, label) in r.markers.iter().take(MAX_MARKERS_APPLIED) {
        let n = c.execute(
            "INSERT OR IGNORE INTO markers(media_id, pos_ms, label, created_at)
             SELECT id, ?2, ?3, ?4 FROM media WHERE id = ?1",
            params![id, pos_ms, label, crate::db::now_secs()],
        )?;
        if n > 0 {
            rep.markers_added += 1;
            changed = true;
        }
    }
    if o.playlists {
        for name in &r.playlists {
            match organize::tx_resolve_playlist(c, name) {
                Ok(Some((pl, created))) => {
                    rep.playlists_created += created as u64;
                    match organize::tx_playlist_add(c, pl, &[id]) {
                        Ok(n) if n > 0 => {
                            rep.links_added += 1;
                            changed = true;
                        }
                        Ok(_) => {}
                        Err(_) => rep.ignored_fields += 1,
                    }
                }
                Ok(None) | Err(_) => rep.ignored_fields += 1,
            }
        }
    } else {
        rep.ignored_fields += r.playlists.len() as u64;
    }
    // Rating, 1 to 5 stars.
    if o.ratings
        && let Some(stars) = r.stars
    {
        let n = if o.overwrite {
            c.execute(
                "UPDATE media SET rating = ?2 WHERE id = ?1 AND rating <> ?2",
                params![id, stars],
            )?
        } else {
            c.execute(
                "UPDATE media SET rating = ?2 WHERE id = ?1 AND rating = 0",
                params![id, stars],
            )?
        };
        if n > 0 {
            rep.ratings_set += 1;
            changed = true;
        }
    }
    // History: the larger count and the later date win; a resume point only
    // fills an empty one.
    if o.history {
        if let Some(pc) = r.play_count {
            changed |= c.execute(
                "UPDATE media SET play_count = ?2 WHERE id = ?1 AND play_count < ?2",
                params![id, pc],
            )? > 0;
        }
        if let Some(lp) = r.last_played {
            changed |= c.execute(
                "UPDATE media SET last_played_at = ?2
                 WHERE id = ?1 AND COALESCE(last_played_at, 0) < ?2",
                params![id, lp],
            )? > 0;
        }
        if let Some(ms) = r.resume_ms.filter(|ms| *ms >= 0) {
            changed |= c.execute(
                "UPDATE media SET resume_ms = ?2 WHERE id = ?1 AND resume_ms IS NULL",
                params![id, ms],
            )? > 0;
        }
    }
    rep.ignored_fields += r.ignored;
    Ok(changed)
}
