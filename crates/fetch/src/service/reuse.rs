//! When may a torrent that went to the error state be added again over its
//! own partial files? Only when every file that exists is still the one this
//! session wrote: same inode, a regular file, not longer than the torrent says.
//! Anything else (a symlink dropped in, a file replaced behind our back) makes
//! the restart refuse instead of writing through it.

use super::torrent::path_is_safe;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What a file looked like when this session first saw it on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// The most the file may hold: its length in the torrent.
    len_max: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

const REFUSED: &str = "files changed on disk; refused";

/// Stamp `path` if it is a regular file (never a symlink); `None` otherwise.
pub(crate) fn stamp(path: &Path, expected_len: u64) -> Option<Stamp> {
    let md = std::fs::symlink_metadata(path).ok()?;
    if !md.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(Stamp {
            len_max: expected_len,
            dev: md.dev(),
            ino: md.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Some(Stamp {
            len_max: expected_len,
        })
    }
}

/// `Ok` when adding the torrent again over `files` (relative path, length)
/// under `dir` can only touch what this session wrote. A missing file is
/// fine: it is created anew.
pub(crate) fn may_reuse(
    dir: &Path,
    files: &[(PathBuf, u64)],
    stamps: &HashMap<PathBuf, Stamp>,
) -> Result<(), &'static str> {
    for (rel, expected) in files {
        if !path_is_safe(rel) {
            return Err(REFUSED);
        }
        // No folder on the way may have become a link.
        let mut cur = dir.to_path_buf();
        if let Some(parent) = rel.parent() {
            for c in parent.components() {
                cur.push(c);
                match std::fs::symlink_metadata(&cur) {
                    Ok(m) if m.file_type().is_dir() => {}
                    Ok(_) => return Err(REFUSED),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_) => return Err(REFUSED),
                }
            }
        }
        let md = match std::fs::symlink_metadata(dir.join(rel)) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(REFUSED),
        };
        if !md.file_type().is_file() || md.len() > *expected {
            return Err(REFUSED);
        }
        let Some(s) = stamps.get(rel) else {
            return Err(REFUSED);
        };
        if md.len() > s.len_max {
            return Err(REFUSED);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if md.dev() != s.dev || md.ino() != s.ino {
                return Err(REFUSED);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(rel: &str, len: u64) -> Vec<(PathBuf, u64)> {
        vec![(PathBuf::from(rel), len)]
    }

    fn stamped(dir: &Path, rel: &str, len: u64) -> HashMap<PathBuf, Stamp> {
        let mut m = HashMap::new();
        m.insert(PathBuf::from(rel), stamp(&dir.join(rel), len).unwrap());
        m
    }

    #[test]
    fn a_missing_file_is_fine() {
        let d = tempfile::tempdir().unwrap();
        assert!(may_reuse(d.path(), &one("a/b.bin", 10), &HashMap::new()).is_ok());
    }

    #[test]
    fn a_shorter_stamped_file_is_fine() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.bin"), [0u8; 4]).unwrap();
        let s = stamped(d.path(), "a.bin", 10);
        assert!(may_reuse(d.path(), &one("a.bin", 10), &s).is_ok());
    }

    #[test]
    fn a_longer_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.bin"), [0u8; 4]).unwrap();
        let s = stamped(d.path(), "a.bin", 10);
        std::fs::write(d.path().join("a.bin"), [0u8; 11]).unwrap();
        assert!(may_reuse(d.path(), &one("a.bin", 10), &s).is_err());
    }

    #[test]
    fn an_existing_file_without_a_stamp_is_refused() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.bin"), [0u8; 4]).unwrap();
        assert!(may_reuse(d.path(), &one("a.bin", 10), &HashMap::new()).is_err());
    }

    #[test]
    fn an_unsafe_path_is_refused() {
        let d = tempfile::tempdir().unwrap();
        assert!(may_reuse(d.path(), &one("../x", 10), &HashMap::new()).is_err());
        assert!(may_reuse(d.path(), &one("/etc/x", 10), &HashMap::new()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_and_never_stamped() {
        let d = tempfile::tempdir().unwrap();
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("target"), [0u8; 2]).unwrap();
        std::fs::write(d.path().join("a.bin"), [0u8; 2]).unwrap();
        let s = stamped(d.path(), "a.bin", 10);
        std::fs::remove_file(d.path().join("a.bin")).unwrap();
        std::os::unix::fs::symlink(t.path().join("target"), d.path().join("a.bin")).unwrap();
        assert!(stamp(&d.path().join("a.bin"), 10).is_none());
        assert!(may_reuse(d.path(), &one("a.bin", 10), &s).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_recreated_file_with_a_new_inode_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.bin");
        std::fs::write(&p, [0u8; 4]).unwrap();
        let s = stamped(d.path(), "a.bin", 10);
        // Keep the old inode alive so the new file cannot reuse its number.
        let keep = d.path().join("old");
        std::fs::rename(&p, &keep).unwrap();
        std::fs::write(&p, [0u8; 4]).unwrap();
        assert!(may_reuse(d.path(), &one("a.bin", 10), &s).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_on_the_way_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let t = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(t.path(), d.path().join("sub")).unwrap();
        assert!(may_reuse(d.path(), &one("sub/a.bin", 10), &HashMap::new()).is_err());
    }
}
