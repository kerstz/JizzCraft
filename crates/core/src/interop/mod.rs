//! Reading other tools' metadata files. Imports are read-only (files are
//! opened, never written), bounded in size and count, dry-run by default and
//! report counts only. A URL found in a file is never fetched. Nothing in this
//! module prints or logs: file names and paths never leave it.
//!
//! A dry run is the same code path as an import, inside one transaction that
//! is rolled back, so its counts are exact.

pub mod nfo;

use crate::commands::{HasLibrary, Registry};
use crate::error::{Error, Result};
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
}

impl Default for ImportOpts {
    fn default() -> Self {
        ImportOpts {
            dry_run: true,
            overwrite: false,
            ratings: false,
            history: false,
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
    })
}

pub fn register<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "meta.import_nfo",
        "Read Kodi-style .nfo files onto matching items (dry run unless dry_run is false): {dir?, dry_run?=true, overwrite?, ratings?, history?}",
        |c, a| {
            let o = opts_from_args(&a)?;
            let dir = a.get("dir").and_then(Value::as_str).map(PathBuf::from);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let rep = nfo::import_nfo(c.db(), dir.as_deref(), &o, &cancel)?;
            Ok(json!(rep))
        },
    );
}
