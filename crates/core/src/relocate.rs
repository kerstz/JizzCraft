//! A library folder that moved: the same files under a new path (a drive
//! that now mounts somewhere else, a folder renamed or moved outside the
//! app). The library entries follow it, with their ratings, history,
//! markers, tags and playlists.
//!
//! A rescan cannot do this on its own: it only recognises moved files whose
//! old library folder is still present, and only fingerprinted ones.
//!
//! Database only. Files are looked at (their size, and for fingerprinted
//! entries the first and last 64 KiB), never opened for writing. A dry run by
//! default, with counts and no names; the real run keeps a `pre-move` backup
//! of the library first.

use crate::db::{Db, blob_to_path, parent_blob, path_to_blob};
use crate::error::{Error, Result};
use crate::fingerprint::quick_fp;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Counts only: no names, no paths.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct MoveReport {
    /// Entries of the folder.
    pub items: u64,
    /// The same file is at the new place (same size, and same fingerprint
    /// when the entry had one). These follow the folder.
    pub found: u64,
    /// Of `found`, how many were checked by fingerprint.
    pub verified: u64,
    /// Another file is at the new place (other size or content): the entry
    /// keeps its old path and is shown as missing, and a rescan adds the
    /// file there as a new item.
    pub different: u64,
    /// Nothing at the new place: the entry keeps its old path, missing.
    pub not_found: u64,
    pub dry_run: bool,
    /// Stopped by the user: nothing was changed.
    pub cancelled: bool,
    /// A `pre-move` backup was written beside the library first (never for a
    /// dry run or an in-memory database).
    pub backed_up: bool,
}

/// `(id, path, size, fingerprint)` of one entry of the folder.
type Entry = (i64, Vec<u8>, i64, Option<Vec<u8>>);

/// What became of one entry at the new place.
enum Seen {
    Found { verified: bool },
    Different,
    NotFound,
}

/// The same file at `new`? Size first, then the fingerprint if the entry has
/// one. A symbolic link never counts: the scanner does not follow them.
fn look(new: &Path, size: i64, fp: Option<&[u8]>) -> Seen {
    let Ok(md) = std::fs::symlink_metadata(new) else {
        return Seen::NotFound;
    };
    if !md.is_file() {
        return Seen::Different;
    }
    if i64::try_from(md.len()).ok() != Some(size) {
        return Seen::Different;
    }
    match fp {
        None => Seen::Found { verified: false },
        Some(old) => match quick_fp(new) {
            Ok((s, f)) if i64::try_from(s).ok() == Some(size) && f.as_slice() == old => {
                Seen::Found { verified: true }
            }
            _ => Seen::Different,
        },
    }
}

/// True when one path is the other or lies inside it, by components.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Point the library folder `from` at `to`, where its files are now.
///
/// `from` is the folder as the library knows it (it may no longer exist);
/// `to` must be an existing folder that is neither inside nor above any
/// library folder. Refused, with nothing changed, when no entry is found at
/// the new place: that is almost always the wrong folder.
pub fn move_root(
    db: &mut Db,
    from: &Path,
    to: &Path,
    dry_run: bool,
    cancel: &AtomicBool,
) -> Result<MoveReport> {
    let folders = db.folders()?;
    let found_root = folders.iter().find(|(_, p)| p == from).or_else(|| {
        crate::pathbytes::canonical(from)
            .ok()
            .and_then(|c| folders.iter().find(|(_, p)| *p == c))
    });
    let Some((folder_id, old_root)) = found_root.cloned() else {
        return Err(Error::NotFound("library folder".into()));
    };
    let new_root = crate::pathbytes::canonical(to)
        .map_err(|_| Error::Invalid("the new folder cannot be opened".into()))?;
    if !new_root.is_dir() {
        return Err(Error::Invalid("the new place is not a folder".into()));
    }
    if folders.iter().any(|(_, p)| overlaps(p, &new_root)) {
        return Err(Error::Invalid(
            "the new folder is, contains or lies inside a library folder".into(),
        ));
    }

    let rows: Vec<Entry> = {
        let mut st = db
            .conn
            .prepare("SELECT id, path, size, fp FROM media WHERE folder_id = ?1 ORDER BY id")?;
        let it = st.query_map([folder_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        it.collect::<std::result::Result<_, _>>()?
    };
    let mut rep = MoveReport {
        items: rows.len() as u64,
        dry_run,
        ..MoveReport::default()
    };
    // (id, new path) of the entries that follow the folder.
    let mut moves: Vec<(i64, PathBuf)> = Vec::new();
    let mut stay: Vec<i64> = Vec::new();
    for (id, path, size, fp) in rows {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            return Ok(rep);
        }
        let old = blob_to_path(path);
        let seen = match old.strip_prefix(&old_root) {
            Ok(rel) if !rel.as_os_str().is_empty() => {
                let new = new_root.join(rel);
                let s = look(&new, size, fp.as_deref());
                if let Seen::Found { .. } = s {
                    moves.push((id, new));
                }
                s
            }
            _ => Seen::NotFound,
        };
        match seen {
            Seen::Found { verified } => {
                rep.found += 1;
                rep.verified += u64::from(verified);
            }
            Seen::Different => {
                rep.different += 1;
                stay.push(id);
            }
            Seen::NotFound => {
                rep.not_found += 1;
                stay.push(id);
            }
        }
    }
    if rep.found == 0 && rep.items > 0 {
        return Err(Error::Invalid(
            "none of this folder's items is at the new place: nothing was changed".into(),
        ));
    }
    if dry_run {
        return Ok(rep);
    }

    rep.backed_up = db.backup_tagged("pre-move")?.is_some();
    let tx = db.conn.transaction()?;
    // Another process may have changed the folder list since it was read.
    let still: Option<i64> = tx
        .query_row(
            "SELECT id FROM folders WHERE id = ?1 AND path = ?2",
            params![folder_id, path_to_blob(&old_root)],
            |r| r.get(0),
        )
        .optional()?;
    if still.is_none() {
        return Err(Error::Invalid(
            "the library folder changed meanwhile: nothing was changed".into(),
        ));
    }
    tx.execute(
        "UPDATE folders SET path = ?2 WHERE id = ?1",
        params![folder_id, path_to_blob(&new_root)],
    )?;
    {
        let mut up = tx.prepare(
            "UPDATE media SET path = ?2, dir = ?3, missing = 0 WHERE id = ?1 AND folder_id = ?4",
        )?;
        for (id, new) in &moves {
            up.execute(params![id, path_to_blob(new), parent_blob(new), folder_id])?;
        }
        let mut gone = tx.prepare("UPDATE media SET missing = 1 WHERE id = ?1")?;
        for id in &stay {
            gone.execute([id])?;
        }
    }
    tx.commit()?;
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::overlaps;
    use std::path::Path;

    #[test]
    fn overlap_is_by_component() {
        assert!(overlaps(Path::new("/a/b"), Path::new("/a")));
        assert!(overlaps(Path::new("/a"), Path::new("/a/b/c")));
        assert!(overlaps(Path::new("/a"), Path::new("/a")));
        assert!(!overlaps(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!overlaps(Path::new("/x"), Path::new("/y")));
    }
}
