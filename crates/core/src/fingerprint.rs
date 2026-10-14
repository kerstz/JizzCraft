//! Cheap content fingerprints, used to recognise a file that was moved or
//! renamed outside the app. Read-only: files are opened and read, never written.
//!
//! `fp = blake3("jzfp1" || size_le || first 64 KiB || last 64 KiB)[..16]`.
//! Files up to 128 KiB are hashed whole. A middle-only edit of a larger file
//! keeps the same fingerprint, which is why size is always compared too.

use crate::db::{Db, blob_to_path, fp_string};
use crate::error::{Error, Result};
use rusqlite::params;
use serde::Serialize;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const FP_CHUNK: u64 = 64 * 1024;

/// Returns `(size, fingerprint)` for a file.
pub fn quick_fp(path: &Path) -> std::io::Result<(u64, [u8; 16])> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    h.update(b"jzfp1");
    h.update(&size.to_le_bytes());
    let mut buf = Vec::new();
    if size <= FP_CHUNK * 2 {
        f.read_to_end(&mut buf)?;
        h.update(&buf);
    } else {
        buf.resize(FP_CHUNK as usize, 0);
        f.read_exact(&mut buf)?;
        h.update(&buf);
        f.seek(SeekFrom::Start(size - FP_CHUNK))?;
        f.read_exact(&mut buf)?;
        h.update(&buf);
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&h.finalize().as_bytes()[..16]);
    Ok((size, out))
}

pub fn parse_fp(s: &str) -> Option<[u8; 16]> {
    let hex = s.strip_prefix("fp1:")?;
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Rows holding data a user would be upset to lose.
const USER_DATA: &str = "(favorite = 1 OR rating > 0 OR hidden = 1 OR play_count > 0
     OR resume_ms IS NOT NULL
     OR EXISTS (SELECT 1 FROM markers k WHERE k.media_id = media.id))";

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct FpReport {
    pub hashed: u64,
    pub failed: u64,
    /// File changed between the scan and the hash; no fingerprint stored.
    pub changed: u64,
    pub remaining: u64,
}

impl Db {
    pub fn set_fp(&self, id: i64, fp: &[u8; 16]) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET fp = ?2 WHERE id = ?1",
            params![id, fp.as_slice()],
        )?;
        Ok(())
    }

    /// Present files without a fingerprint: `(id, path, size)`.
    pub fn fp_pending(&self, user_data_only: bool, limit: u64) -> Result<Vec<(i64, PathBuf, u64)>> {
        let extra = if user_data_only {
            format!(" AND {USER_DATA}")
        } else {
            String::new()
        };
        let mut st = self.conn.prepare(&format!(
            "SELECT id, path, size FROM media
             WHERE fp IS NULL AND missing = 0{extra} ORDER BY id LIMIT ?1"
        ))?;
        let rows = st.query_map([limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                blob_to_path(r.get(1)?),
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn fp_remaining(&self, user_data_only: bool) -> Result<u64> {
        let extra = if user_data_only {
            format!(" AND {USER_DATA}")
        } else {
            String::new()
        };
        Ok(self.conn.query_row(
            &format!("SELECT COUNT(*) FROM media WHERE fp IS NULL AND missing = 0{extra}"),
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Fold the row `from` into `to`: user data is combined, then `from` is
    /// deleted. Database only; no file is touched.
    pub fn merge_into(&mut self, from: i64, to: i64) -> Result<()> {
        if from == to {
            return Err(Error::Invalid("cannot merge an item into itself".into()));
        }
        for id in [from, to] {
            if self.get(id)?.is_none() {
                return Err(Error::NotFound(format!("media id {id}")));
            }
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE media SET
                favorite = MAX(favorite, (SELECT favorite FROM media WHERE id = ?1)),
                hidden = MAX(hidden, (SELECT hidden FROM media WHERE id = ?1)),
                rating = MAX(rating, (SELECT rating FROM media WHERE id = ?1)),
                play_count = MAX(play_count, (SELECT play_count FROM media WHERE id = ?1)),
                resume_ms = CASE
                    WHEN COALESCE((SELECT last_played_at FROM media WHERE id = ?1), 0)
                         > COALESCE(last_played_at, 0)
                    THEN (SELECT resume_ms FROM media WHERE id = ?1)
                    ELSE resume_ms END,
                last_played_at = MAX(COALESCE(last_played_at, 0),
                                     COALESCE((SELECT last_played_at FROM media WHERE id = ?1), 0))
             WHERE id = ?2",
            params![from, to],
        )?;
        tx.execute(
            "UPDATE media SET last_played_at = NULL WHERE id = ?1 AND last_played_at = 0",
            [to],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO markers(media_id, pos_ms, label, created_at)
             SELECT ?2, pos_ms, label, created_at FROM markers WHERE media_id = ?1",
            params![from, to],
        )?;
        tx.execute("DELETE FROM media WHERE id = ?1", [from])?;
        tx.commit()?;
        Ok(())
    }
}

/// Fingerprint present files in small transactions so the UI never waits long.
/// With `user_data_only` (the default for background use) only rows carrying
/// user data are read, which bounds disk reads to what relinking needs.
pub fn fingerprint_pending(
    db: &mut Db,
    user_data_only: bool,
    limit: Option<u64>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<FpReport> {
    let mut rep = FpReport::default();
    let mut tried: HashSet<i64> = HashSet::new();
    let budget = limit.unwrap_or(u64::MAX);
    let total = db.fp_remaining(user_data_only)?;
    'outer: loop {
        let batch = db.fp_pending(user_data_only, 32 + tried.len() as u64)?;
        let batch: Vec<_> = batch
            .into_iter()
            .filter(|(id, _, _)| !tried.contains(id))
            .take(32)
            .collect();
        if batch.is_empty() {
            break;
        }
        for (id, path, size) in batch {
            if cancel.load(Ordering::Relaxed) || rep.hashed + rep.failed + rep.changed >= budget {
                break 'outer;
            }
            tried.insert(id);
            match quick_fp(&path) {
                Ok((s, fp)) if s == size => {
                    db.set_fp(id, &fp)?;
                    rep.hashed += 1;
                }
                Ok(_) => rep.changed += 1,
                Err(_) => rep.failed += 1,
            }
            progress(rep.hashed + rep.failed + rep.changed, total);
        }
    }
    rep.remaining = db.fp_remaining(user_data_only)?;
    Ok(rep)
}

/// Used by the scanner: rows with this size and kind that carry a fingerprint.
pub(crate) struct Candidate {
    pub id: i64,
    pub path: PathBuf,
    pub name: String,
    pub root: PathBuf,
    pub duration_ms: Option<i64>,
    pub fp: Vec<u8>,
}

pub(crate) fn candidate_sql() -> &'static str {
    "SELECT m.id, m.path, m.name, f.path, m.duration_ms, m.fp
     FROM media m JOIN folders f ON f.id = m.folder_id
     WHERE m.size = ?1 AND m.kind = ?2 AND m.fp IS NOT NULL
     ORDER BY m.id"
}

pub(crate) fn candidate_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        id: r.get(0)?,
        path: blob_to_path(r.get(1)?),
        name: r.get(2)?,
        root: blob_to_path(r.get(3)?),
        duration_ms: r.get(4)?,
        fp: r.get(5)?,
    })
}

/// Pick the row a new file most likely is, among `candidates`. Only gone
/// files qualify, in libraries whose root is present. Ties prefer the same
/// file name, then the lowest id, so the result is deterministic.
pub(crate) fn pick_moved(
    candidates: Vec<Candidate>,
    new_path: &Path,
    new_name: &str,
    size: u64,
    new_duration_ms: Option<i64>,
    same_name_only: bool,
) -> Option<i64> {
    let mut live: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| !c.path.exists() && c.root.exists())
        .filter(|c| !same_name_only || c.name == new_name)
        .filter(|c| match (c.duration_ms, new_duration_ms) {
            (Some(a), Some(b)) => (a - b).abs() <= 1000,
            _ => true,
        })
        .collect();
    if live.is_empty() {
        return None;
    }
    let (s, fp) = quick_fp(new_path).ok()?;
    if s != size {
        return None;
    }
    live.retain(|c| c.fp.as_slice() == fp.as_slice());
    live.sort_by_key(|c| (c.name != new_name, c.id));
    live.first().map(|c| c.id)
}

#[doc(hidden)]
pub fn fp_hex(fp: &[u8; 16]) -> String {
    fp_string(fp)
}
