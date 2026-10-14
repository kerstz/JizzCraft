//! Recursive folder indexing. Read-only on user media: files are only stat'ed
//! and (for images) have their header read for dimensions.
//! Look, don't touch: the scanner has excellent museum manners.

use crate::db::{Db, now_secs, parent_blob, path_to_blob};
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
    /// Files recognised by fingerprint as a moved or renamed known item.
    pub relinked: u64,
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
        self.relinked += o.relinked;
        self.unchanged += o.unchanged;
        self.marked_missing += o.marked_missing;
        self.skipped_symlinks += o.skipped_symlinks;
        self.skipped_unsupported += o.skipped_unsupported;
        self.errors += o.errors;
    }
}

/// Import a folder (idempotent) and scan it. Returns `(folder_id, report)`.
pub fn import_folder(db: &mut Db, path: &Path) -> Result<(i64, ScanReport)> {
    let root = crate::pathbytes::canonical(path)
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

// Windows file attributes (winnt.h). Kept as plain numbers so the decision is
// testable on every platform.
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// Hidden or system entries (`$RECYCLE.BIN`, `System Volume Information`).
fn attrs_hidden(attrs: u32) -> bool {
    attrs & (ATTR_HIDDEN | ATTR_SYSTEM) != 0
}

/// A cloud placeholder: reading its header would download the whole file.
fn attrs_placeholder(attrs: u32) -> bool {
    attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(windows)]
fn win_attrs(md: &std::fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes()
}

#[cfg(not(windows))]
fn win_attrs(_: &std::fs::Metadata) -> u32 {
    0
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
    let mut pending: Vec<NewFile> = Vec::new();
    {
        let mut find = tx.prepare("SELECT size, mtime FROM media WHERE path = ?1")?;
        let mut insert = tx.prepare(
            "INSERT INTO media(folder_id, path, name, name_lc, kind, size, mtime,
                               width, height, added_at, missing, scan_gen, dir)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12)",
        )?;
        let mut update = tx.prepare(
            "UPDATE media SET size = ?2, mtime = ?3, width = ?4, height = ?5,
                              duration_ms = NULL, codec = NULL, missing = 0, scan_gen = ?6,
                              fp = NULL, checksum = NULL, checked_at = NULL,
                              damaged = 0, check_note = NULL
             WHERE path = ?1",
        )?;
        let mut touch =
            tx.prepare("UPDATE media SET missing = 0, scan_gen = ?2 WHERE path = ?1")?;
        let mut cands = tx.prepare(crate::fingerprint::candidate_sql())?;
        let mut relink = tx.prepare(
            "UPDATE media SET folder_id = ?2, path = ?3, name = ?4, name_lc = ?5, dir = ?6,
                              mtime = ?7, missing = 0, scan_gen = ?8
             WHERE id = ?1",
        )?;

        // Sorted, so a scan behaves the same on every filesystem.
        let walker = WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || (!is_hidden(e.file_name())
                        && !e.metadata().is_ok_and(|m| attrs_hidden(win_attrs(&m))))
            });

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
                    let (w, h) = if attrs_placeholder(win_attrs(&md)) {
                        (None, None)
                    } else {
                        probe_dims(entry.path(), kind)
                    };
                    update.execute(params![blob, size, mtime, w, h, gen_])?;
                    rep.updated += 1;
                }
                None => {
                    // Decided after the walk: pairing must not depend on the
                    // order the filesystem lists files in.
                    pending.push(NewFile {
                        path: entry.path().to_path_buf(),
                        blob,
                        name: entry.file_name().to_string_lossy().into_owned(),
                        size,
                        mtime,
                        kind,
                        placeholder: attrs_placeholder(win_attrs(&md)),
                    });
                }
            }
        }

        // A new path whose size matches a fingerprinted item that has vanished
        // may simply be that item, moved or renamed. Same-name matches claim
        // their row first; the rest pair by fingerprint (lowest id first).
        let mut todo = pending;
        for same_name_only in [true, false] {
            let mut rest = Vec::new();
            for f in todo {
                if f.placeholder {
                    // Matching would read the file's bytes.
                    rest.push(f);
                    continue;
                }
                let moved = find_moved(
                    &mut cands,
                    &f.path,
                    &f.name,
                    f.size,
                    f.kind.as_i64(),
                    same_name_only,
                )?;
                match moved {
                    Some(old_id) => {
                        relink.execute(params![
                            old_id,
                            folder_id,
                            f.blob,
                            f.name,
                            f.name.to_lowercase(),
                            parent_blob(&f.path),
                            f.mtime,
                            gen_
                        ])?;
                        rep.relinked += 1;
                    }
                    None => rest.push(f),
                }
            }
            todo = rest;
        }
        for f in todo {
            let (w, h) = if f.placeholder {
                (None, None)
            } else {
                probe_dims(&f.path, f.kind)
            };
            insert.execute(params![
                folder_id,
                f.blob,
                f.name,
                f.name.to_lowercase(),
                f.kind.as_i64(),
                f.size,
                f.mtime,
                w,
                h,
                now_secs(),
                gen_,
                parent_blob(&f.path)
            ])?;
            rep.added += 1;
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

/// A file seen for the first time during a scan.
struct NewFile {
    path: PathBuf,
    blob: Vec<u8>,
    name: String,
    size: i64,
    mtime: i64,
    kind: MediaKind,
    /// A cloud placeholder: never read, so it is never downloaded.
    placeholder: bool,
}

/// Match a new file against fingerprinted rows whose files are gone.
fn find_moved(
    cands: &mut rusqlite::Statement<'_>,
    new_path: &Path,
    new_name: &str,
    size: i64,
    kind: i64,
    same_name_only: bool,
) -> Result<Option<i64>> {
    let rows: Vec<_> = cands
        .query_map(params![size, kind], crate::fingerprint::candidate_row)?
        .collect::<std::result::Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(crate::fingerprint::pick_moved(
        rows,
        new_path,
        new_name,
        size as u64,
        None,
        same_name_only,
    ))
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

#[cfg(test)]
mod attr_tests {
    use super::*;

    #[test]
    fn windows_attribute_decisions() {
        assert!(attrs_hidden(0x2) && attrs_hidden(0x4 | 0x20) && !attrs_hidden(0x20));
        assert!(attrs_placeholder(0x40_0000) && attrs_placeholder(0x1000));
        assert!(attrs_placeholder(0x4_0000) && !attrs_placeholder(0x20));
        assert_eq!(win_attrs_default(), 0);
    }

    fn win_attrs_default() -> u32 {
        #[cfg(not(windows))]
        {
            let md = std::fs::metadata(".").unwrap();
            win_attrs(&md)
        }
        #[cfg(windows)]
        {
            0
        }
    }
}
