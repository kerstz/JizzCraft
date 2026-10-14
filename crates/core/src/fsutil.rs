//! Small filesystem helpers shared by the crates.

use std::io;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

/// Rename `tmp` over `dst`. On Windows an antivirus scan or the search
/// indexer can hold either file for a moment and the rename then fails with
/// `PermissionDenied`: retry a few times before giving up. Elsewhere the first
/// attempt is the only one that matters.
///
/// Five attempts: enough for an antivirus to finish reading, not enough for it
/// to finish forming opinions.
pub fn replace(tmp: &Path, dst: &Path) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..5u64 {
        match std::fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && cfg!(windows) => {
                last = Some(e);
                sleep(Duration::from_millis(40 * (attempt + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::ErrorKind::PermissionDenied.into()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn replace_overwrites_the_target() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, b"new").unwrap();
        std::fs::write(&b, b"old").unwrap();
        super::replace(&a, &b).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"new");
        assert!(!a.exists());
    }
}
