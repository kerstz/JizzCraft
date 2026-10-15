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

/// Room kept free while a torrent runs: the monitor pauses before the
/// volume is closer to full than this.
pub const RUN_MARGIN: u64 = 64 * 1024 * 1024;

/// True when `remaining` bytes still to write plus `margin` do not fit in
/// `free`. An unknown free space is never "low".
pub fn is_low(free: Option<u64>, remaining: u64, margin: u64) -> bool {
    free.is_some_and(|f| f < remaining.saturating_add(margin))
}

/// What the engine reported through its panics since the last resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// A panic in the chunk-write path: a partial write, i.e. no room.
    Write,
    /// Any other panic of the engine.
    Other,
}

/// Verdict of the watchdog for one running torrent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Pause it and report the typed "disk full" state.
    DiskFull,
    /// Pause it and report a generic failure.
    Error,
}

/// Decide what to do with a running torrent.
///
/// * the volume cannot hold what is left (plus `margin`, normally
///   [`RUN_MARGIN`]): `DiskFull`,
///   before anything is written;
/// * no byte arrived for `window` after an engine panic: the torrent is stuck
///   (a panicked task never reports back), `DiskFull` for a write fault and
///   `Error` for any other;
/// * otherwise `Ok`. A silent swarm without any panic is just slow.
pub fn judge(
    free: Option<u64>,
    remaining: u64,
    idle: std::time::Duration,
    window: std::time::Duration,
    fault: Fault,
    margin: u64,
) -> Verdict {
    if is_low(free, remaining, margin) {
        return Verdict::DiskFull;
    }
    if idle >= window {
        return match fault {
            Fault::Write => Verdict::DiskFull,
            Fault::Other => Verdict::Error,
            Fault::None => Verdict::Ok,
        };
    }
    Verdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MIB: u64 = 1024 * 1024;
    const W: Duration = Duration::from_secs(30);

    #[test]
    fn judge_pauses_before_the_disk_is_full() {
        let idle = Duration::ZERO;
        // 100 MiB to go, 100 MiB free: below the margin.
        assert_eq!(
            judge(Some(100 * MIB), 100 * MIB, idle, W, Fault::None, RUN_MARGIN),
            Verdict::DiskFull
        );
        // Exactly remaining + margin fits.
        assert_eq!(
            judge(
                Some(100 * MIB + RUN_MARGIN),
                100 * MIB,
                idle,
                W,
                Fault::None,
                RUN_MARGIN
            ),
            Verdict::Ok
        );
        // Unknown space never pauses.
        assert_eq!(
            judge(None, u64::MAX, idle, W, Fault::None, RUN_MARGIN),
            Verdict::Ok
        );
        // Overflow cannot wrap into "fits".
        assert_eq!(
            judge(
                Some(u64::MAX - 1),
                u64::MAX,
                idle,
                W,
                Fault::None,
                RUN_MARGIN
            ),
            Verdict::DiskFull
        );
    }

    #[test]
    fn judge_detects_a_stuck_torrent_only_after_a_fault() {
        let plenty = Some(10 * 1024 * MIB);
        let late = W + Duration::from_secs(1);
        // Quiet swarm, no panic: leave it alone, however long.
        assert_eq!(
            judge(
                plenty,
                MIB,
                Duration::from_secs(3600),
                W,
                Fault::None,
                RUN_MARGIN
            ),
            Verdict::Ok
        );
        // Panic, but data still arrived recently: wait.
        assert_eq!(
            judge(
                plenty,
                MIB,
                Duration::from_secs(1),
                W,
                Fault::Write,
                RUN_MARGIN
            ),
            Verdict::Ok
        );
        assert_eq!(
            judge(plenty, MIB, late, W, Fault::Write, RUN_MARGIN),
            Verdict::DiskFull
        );
        assert_eq!(
            judge(plenty, MIB, late, W, Fault::Other, RUN_MARGIN),
            Verdict::Error
        );
        // Disk full wins over a generic fault.
        assert_eq!(
            judge(Some(MIB), MIB, late, W, Fault::Other, RUN_MARGIN),
            Verdict::DiskFull
        );
    }

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
