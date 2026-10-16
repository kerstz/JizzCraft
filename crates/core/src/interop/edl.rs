//! Kodi `.edl` files (edit decision lists): one line per span,
//! `start end action`, times in seconds (or `HH:MM:SS.sss`). Each usable line
//! becomes a marker on the video of the same name, labelled with its action
//! (`Cut`, `Mute`, `Scene`). Nothing is skipped or muted on playback: the
//! markers only show on the timeline. Read-only, bounded, counts only.

use super::nfo::{DirRow, Found, sidecar_owner};
use super::{
    ImportOpts, InteropReport, MAX_DEPTH, MAX_FILES, Record, apply_record, read_capped, walk_files,
};
use crate::db::Db;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Largest file read.
pub const MAX_EDL_BYTES: u64 = 64 * 1024;
/// Most lines in one file.
pub const MAX_EDL_LINES: usize = 2_000;
/// Longest time accepted, in seconds.
const MAX_SECONDS: f64 = 1.0e7;

/// Why a file was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdlReject {
    TooBig,
    Malformed,
    TooMany,
}

/// Seconds from `12.5` or `HH:MM:SS(.sss)` (also `MM:SS`).
fn parse_time(t: &str) -> Option<f64> {
    let secs = if t.contains(':') {
        let parts: Vec<&str> = t.split(':').collect();
        if !(2..=3).contains(&parts.len()) {
            return None;
        }
        let mut total = 0.0f64;
        for (i, p) in parts.iter().enumerate() {
            let last = i == parts.len() - 1;
            let v = if last {
                p.parse::<f64>().ok()?
            } else {
                p.parse::<u32>().ok()? as f64
            };
            if !p.chars().all(|c| c.is_ascii_digit() || (last && c == '.')) {
                return None;
            }
            total = total * 60.0 + v;
        }
        total
    } else {
        if !t.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }
        t.parse::<f64>().ok()?
    };
    (secs.is_finite() && (0.0..MAX_SECONDS).contains(&secs)).then_some(secs)
}

/// Parse one `.edl`: markers `(position in ms, label)` and the number of
/// lines or actions left out. Pure: no file, no network.
pub fn parse_edl(bytes: &[u8]) -> std::result::Result<(Vec<(i64, String)>, u64), EdlReject> {
    if bytes.len() as u64 > MAX_EDL_BYTES {
        return Err(EdlReject::TooBig);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| EdlReject::Malformed)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    let mut ignored = 0u64;
    for (n, line) in text.lines().enumerate() {
        if n >= MAX_EDL_LINES {
            return Err(EdlReject::TooMany);
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(a), Some(b), Some(act)) = (it.next(), it.next(), it.next()) else {
            ignored += 1;
            continue;
        };
        // `#frame` positions are not times.
        if a.starts_with('#') || b.starts_with('#') {
            ignored += 1;
            continue;
        }
        let (Some(start), Some(end)) = (parse_time(a), parse_time(b)) else {
            ignored += 1;
            continue;
        };
        if end < start {
            ignored += 1;
            continue;
        }
        let label = match act {
            "0" => "Cut",
            "1" => "Mute",
            "2" => "Scene",
            _ => {
                ignored += 1;
                continue;
            }
        };
        out.push(((start * 1000.0).round() as i64, label.to_owned()));
    }
    Ok((out, ignored))
}

/// Read every `.edl` below `dir` (or below every library folder when `dir`
/// is `None`) and add its markers to the video of the same name. Existing
/// markers at the same millisecond are kept, so a second run adds nothing.
/// With `dry_run` (or on cancel) nothing is written.
pub fn import_edl(
    db: &mut Db,
    dir: Option<&Path>,
    o: &ImportOpts,
    cancel: &AtomicBool,
) -> Result<InteropReport> {
    let roots: Vec<PathBuf> = match dir {
        Some(d) => {
            if !d.is_dir() {
                return Err(Error::Invalid("not a directory".into()));
            }
            vec![d.to_path_buf()]
        }
        None => db.folders()?.into_iter().map(|(_, p)| p).collect(),
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let room = MAX_FILES.saturating_sub(files.len());
        if room == 0 {
            break;
        }
        files.extend(walk_files(root, "edl", room, MAX_DEPTH));
    }
    let mut rep = InteropReport {
        dry_run: o.dry_run,
        files_seen: files.len() as u64,
        ..Default::default()
    };
    let tx = db.conn.transaction()?;
    let mut dirs: HashMap<Vec<u8>, Vec<DirRow>> = HashMap::new();
    for path in &files {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        let Ok(bytes) = read_capped(path, MAX_EDL_BYTES) else {
            rep.rejected += 1;
            continue;
        };
        let Ok((markers, ignored)) = parse_edl(&bytes) else {
            rep.rejected += 1;
            continue;
        };
        rep.parsed += 1;
        match sidecar_owner(&tx, path, &mut dirs)? {
            Found::None => rep.unmatched += 1,
            Found::Many => rep.ambiguous += 1,
            Found::One(id) => {
                rep.matched += 1;
                let rec = Record {
                    markers,
                    ignored,
                    ..Default::default()
                };
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
