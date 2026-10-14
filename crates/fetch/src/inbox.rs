//! Hand-off of `magnet:` links between app launches, without a socket.
//!
//! A second launch (the desktop's magnet handler) writes the link into a
//! private folder and exits; the running app picks it up. A lock file decides
//! which launch is the running app.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

const MAX_LINK: usize = 4096;

pub fn dir() -> PathBuf {
    jizzcraft_core::paths::state_dir().join("inbox")
}

/// Take the "this is the running app" lock. `None` = another instance holds
/// it. Keep the returned file alive for the whole run.
pub fn try_primary_instance() -> Option<File> {
    let dir = jizzcraft_core::paths::state_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("app.lock"))
        .ok()?;
    f.try_lock().ok()?;
    Some(f)
}

/// Queue a link for the running app. Only `magnet:` links are accepted.
pub fn push(link: &str) -> std::io::Result<()> {
    let link = link.trim();
    if !link.starts_with("magnet:") || link.len() > MAX_LINK {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a magnet link",
        ));
    }
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_nanos());
    let tmp = d.join(format!("{nanos}.tmp"));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(link.as_bytes())?;
    f.sync_all()?;
    jizzcraft_core::fsutil::replace(&tmp, &d.join(format!("{nanos}.magnet")))
}

/// Read and remove every queued link (oldest first). Only `.magnet` files
/// this module wrote are touched.
pub fn take_all() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "magnet"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        if let Ok(txt) = std::fs::read_to_string(&p)
            && txt.trim().starts_with("magnet:")
            && txt.len() <= MAX_LINK
        {
            out.push(txt.trim().to_owned());
        }
        let _ = std::fs::remove_file(&p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip_and_junk_is_refused() {
        // Process-wide home: the only test in this crate that sets one.
        let d = tempfile::tempdir().unwrap();
        jizzcraft_core::paths::set_home(d.path().to_path_buf());
        push("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567").unwrap();
        assert!(push("http://example.com/x").is_err());
        assert!(push(&format!("magnet:{}", "a".repeat(5000))).is_err());
        let got = take_all();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("magnet:?xt="));
        assert!(take_all().is_empty());

        let first = try_primary_instance().expect("first launch takes the lock");
        assert!(try_primary_instance().is_none(), "second launch does not");
        drop(first);
    }
}
