//! Free space of the volume that holds a path.
//!
//! Used as a preflight before a torrent is added, so a full disk is reported
//! before anything is written rather than as a stalled download.

use std::path::Path;

/// Bytes a normal user may still write on the volume holding `path`.
/// The path may not exist yet: the nearest existing ancestor is asked.
/// `None` when the platform call is unavailable or fails; callers must then
/// let the download go on (the runtime check still catches ENOSPC).
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    imp(p)
}

#[cfg(unix)]
fn imp(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a valid out pointer.
    let rc = unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, the struct is initialised.
    let s = unsafe { s.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

// TODO(windows): GetDiskFreeSpaceExW needs `windows-sys`, which fetch does not
// depend on. Until it does, Windows reports "unknown" and skips the preflight.
#[cfg(not(unix))]
fn imp(_p: &Path) -> Option<u64> {
    None
}

/// Room kept free on top of what a torrent needs.
pub const HEADROOM: u64 = 512 * 1024 * 1024;

/// True when `needed` bytes plus [`HEADROOM`] do not fit in `free`.
pub fn is_full(free: Option<u64>, needed: u64) -> bool {
    free.is_some_and(|f| f < needed.saturating_add(HEADROOM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_arithmetic() {
        assert!(!is_full(None, u64::MAX));
        assert!(is_full(Some(HEADROOM), 1));
        assert!(!is_full(Some(HEADROOM + 1), 1));
    }

    #[cfg(unix)]
    #[test]
    fn real_volume_answers_even_for_a_missing_leaf() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_bytes(&d.path().join("a/b/c")).is_some());
    }
}
