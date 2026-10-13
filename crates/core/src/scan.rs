//! Recursive folder indexing. Read-only on user media: files are only stat'ed
//! and (for images) have their header read for dimensions.

use crate::db::{Db, now_secs, path_to_blob};
use crate::error::{Error, Result};
use crate::model::{MediaKind, classify};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ScanReport {
    pub added: u64,
    pub updated: u64,
    pub unchanged: u64,
    /// Files that disappeared since the last scan (kept in the library, flagged).
    pub marked_missing: u64,
    pub skipped_symlinks: u64,
    pub skipped_unsupported: u64,
    /// Permission errors, vanished entries, unreadable metadata.
    pub errors: u64,
}

impl ScanReport {
    fn merge(&mut self, o: &ScanReport) {
        self.added += o.added;
        self.updated += o.updated;
        self.unchanged += o.unchanged;
        self.marked_missing += o.marked_missing;
        self.skipped_symlinks += o.skipped_symlinks;
        self.skipped_unsupported += o.skipped_unsupported;
        self.errors += o.errors;
    }
}

/// Import a folder (idempotent) and scan it. Returns `(folder_id, report)`.
pub fn import_folder(db: &mut Db, path: &Path) -> Result<(i64, ScanReport)> {
    let root = std::fs::canonicalize(path)
        .map_err(|e| Error::Invalid(format!("cannot open folder: {e}")))?;
    if !root.is_dir() {
        return Err(Error::Invalid("not a directory".into()));
    }
    let (id, _) = db.add_folder(&root)?;
    let report = scan_folder(db, id, &root)?;
    Ok((id, report))
}

pub fn rescan_all(db: &mut Db) -> Result<ScanReport> {
    let mut total = ScanReport::default();
    for (id, root) in db.folders()? {
        total.merge(&scan_folder(db, id, &root)?);
    }
    Ok(total)
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|s| s.starts_with('.'))
}

pub fn scan_folder(db: &mut Db, folder_id: i64, root: &Path) -> Result<ScanReport> {
    let mut rep = ScanReport::default();
    let tx = db.conn.transaction()?;
    tx.execute(
        "UPDATE folders SET scan_gen = scan_gen + 1 WHERE id = ?1",
        [folder_id],
    )?;
    let gen_: i64 = tx.query_row(
        "SELECT scan_gen FROM folders WHERE id = ?1",
        [folder_id],
        |r| r.get(0),
    )?;

    // Root gone (unmounted drive, deleted): everything below is missing.
    if !root.exists() {
        rep.errors += 1;
        rep.marked_missing = tx.execute(
            "UPDATE media SET missing = 1 WHERE folder_id = ?1 AND missing = 0",
            [folder_id],
        )? as u64;
        tx.commit()?;
        return Ok(rep);
    }

    let mut unreadable: Vec<PathBuf> = Vec::new();
    {
        let mut find = tx.prepare("SELECT size, mtime FROM media WHERE path = ?1")?;
        let mut insert = tx.prepare(
            "INSERT INTO media(folder_id, path, name, name_lc, kind, size, mtime,
                               width, height, added_at, missing, scan_gen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)",
        )?;
        let mut update = tx.prepare(
            "UPDATE media SET size = ?2, mtime = ?3, width = ?4, height = ?5,
                              duration_ms = NULL, codec = NULL, missing = 0, scan_gen = ?6
             WHERE path = ?1",
        )?;
        let mut touch =
            tx.prepare("UPDATE media SET missing = 0, scan_gen = ?2 WHERE path = ?1")?;

        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()));

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    rep.errors += 1;
                    if let Some(p) = e.path() {
                        unreadable.push(p.to_path_buf());
                    }
                    continue;
                }
            };
            let ft = entry.file_type();
            if ft.is_symlink() {
                rep.skipped_symlinks += 1;
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let Some(kind) = classify(entry.path()) else {
                rep.skipped_unsupported += 1;
                continue;
            };
            let md = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    rep.errors += 1;
                    unreadable.push(entry.path().to_path_buf());
                    continue;
                }
            };
            let size = md.len() as i64;
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let blob = path_to_blob(entry.path());

            let existing: Option<(i64, i64)> = find
                .query_row([&blob], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match existing {
                Some((s, m)) if s == size && m == mtime => {
                    touch.execute(params![blob, gen_])?;
                    rep.unchanged += 1;
                }
                Some(_) => {
                    let (w, h) = probe_dims(entry.path(), kind);
                    update.execute(params![blob, size, mtime, w, h, gen_])?;
                    rep.updated += 1;
                }
                None => {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let (w, h) = probe_dims(entry.path(), kind);
                    insert.execute(params![
                        folder_id,
                        blob,
                        name,
                        name.to_lowercase(),
                        kind.as_i64(),
                        size,
                        mtime,
                        w,
                        h,
                        now_secs(),
                        gen_
                    ])?;
                    rep.added += 1;
                }
            }
        }
    }

    // A directory we could not read must not make its known files "missing".
    for p in &unreadable {
        let prefix = path_to_blob(p);
        tx.execute(
            "UPDATE media SET scan_gen = ?2
             WHERE folder_id = ?1 AND substr(path, 1, ?3) = ?4",
            params![folder_id, gen_, prefix.len() as i64, prefix],
        )?;
    }
    rep.marked_missing = tx.execute(
        "UPDATE media SET missing = 1 WHERE folder_id = ?1 AND scan_gen <> ?2 AND missing = 0",
        params![folder_id, gen_],
    )? as u64;
    tx.commit()?;
    Ok(rep)
}

/// Cheap header read; videos are probed later by the `media` crate.
fn probe_dims(path: &Path, kind: MediaKind) -> (Option<u32>, Option<u32>) {
    if kind != MediaKind::Image {
        return (None, None);
    }
    image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map_or((None, None), |(w, h)| (Some(w), Some(h)))
}
