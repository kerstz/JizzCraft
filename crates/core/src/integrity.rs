//! Whole-file checksums (core half of the integrity check). The decode test
//! lives in the `media` crate because it needs GStreamer.
//!
//! Bit-rot rule: a stored checksum, an unchanged size and mtime, and a new
//! hash that differs means the content changed under us (`damaged = 2`).
//! Same size, same date, different bytes: the file has been living a double
//! life.

use crate::db::{Db, blob_to_path, now_secs};
use crate::error::Result;
use rusqlite::params;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Streams the file with a 1 MiB buffer. `None` means cancelled.
pub fn checksum_file(path: &Path, cancel: &AtomicBool) -> std::io::Result<Option<[u8; 32]>> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Some(*h.finalize().as_bytes()))
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct CheckReport {
    pub checked: u64,
    pub ok: u64,
    pub damaged: u64,
    pub unreadable: u64,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Never checked since the last change.
    Unchecked,
    All,
}

pub struct CheckResult<'a> {
    pub damaged: u8,
    pub note: Option<&'a str>,
    pub checksum: Option<[u8; 32]>,
}

impl Db {
    pub fn set_check(&self, id: i64, r: &CheckResult<'_>) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET damaged = ?2, check_note = ?3,
                    checksum = COALESCE(?4, checksum), checked_at = ?5
             WHERE id = ?1",
            params![
                id,
                r.damaged,
                r.note,
                r.checksum.as_ref().map(|c| c.as_slice()),
                now_secs()
            ],
        )?;
        Ok(())
    }

    /// Present files to check, in path order (HDD locality):
    /// `(id, path, size, mtime, stored_checksum)`.
    #[allow(clippy::type_complexity)]
    pub fn check_pending(
        &self,
        scope: Scope,
        limit: u64,
    ) -> Result<Vec<(i64, PathBuf, u64, i64, Option<Vec<u8>>)>> {
        let cond = match scope {
            Scope::Unchecked => " AND checked_at IS NULL",
            Scope::All => "",
        };
        let mut st = self.conn.prepare(&format!(
            "SELECT id, path, size, mtime, checksum FROM media
             WHERE missing = 0{cond} ORDER BY path LIMIT ?1"
        ))?;
        let rows = st.query_map([limit.min(i64::MAX as u64) as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                blob_to_path(r.get(1)?),
                r.get::<_, i64>(2)? as u64,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// Checksum present files and apply the bit-rot rule.
pub fn checksum_pending(
    db: &mut Db,
    scope: Scope,
    limit: Option<u64>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<CheckReport> {
    let mut rep = CheckReport::default();
    let rows = db.check_pending(scope, limit.unwrap_or(u64::MAX))?;
    let total = rows.len() as u64;
    for (id, path, size, mtime, stored) in rows {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        rep.checked += 1;
        let md = std::fs::metadata(&path);
        let unchanged = md.as_ref().is_ok_and(|m| {
            m.len() == size
                && m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    == Some(mtime)
        });
        match checksum_file(&path, cancel) {
            Ok(None) => {
                rep.cancelled = true;
                rep.checked -= 1;
                break;
            }
            Ok(Some(sum)) => {
                let rotted = unchanged && stored.as_deref().is_some_and(|s| s != sum.as_slice());
                if rotted {
                    db.set_check(
                        id,
                        &CheckResult {
                            damaged: 2,
                            note: Some("checksum-changed"),
                            checksum: None,
                        },
                    )?;
                    rep.damaged += 1;
                } else {
                    db.set_check(
                        id,
                        &CheckResult {
                            damaged: 0,
                            note: None,
                            checksum: Some(sum),
                        },
                    )?;
                    rep.ok += 1;
                }
            }
            Err(_) => {
                db.set_check(
                    id,
                    &CheckResult {
                        damaged: 1,
                        note: Some("unreadable"),
                        checksum: None,
                    },
                )?;
                rep.unreadable += 1;
            }
        }
        progress(rep.checked, total);
    }
    Ok(rep)
}
