//! Hand-off of `magnet:` links and `http(s)` addresses (`.link` files) between app launches, without a socket.
//!
//! A second launch (the desktop's magnet handler) writes the link into a
//! private folder and exits; the running app picks it up. A lock file decides
//! which launch is the running app.
//!
//! Like a note slipped under a door: no socket, no small talk.

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
    write_entry(link, "magnet")
}

/// Queue an `http(s)` address for the running app. It is vetted with the
/// bridge's `check_play_url` (no credentials, no local hosts, <= 4096 bytes);
/// a refused address is never written. The running app only ever shows it on
/// a confirmation card.
pub fn push_link(url: &str) -> std::io::Result<()> {
    let ok = vet_link(url).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    write_entry(&ok, "link")
}

/// The normalized address, or a fixed sentence saying why it is refused.
pub fn vet_link(url: &str) -> Result<String, &'static str> {
    let url = url.trim();
    if url.len() > MAX_LINK {
        return Err("the address is too long");
    }
    jizzcraft_bridge::check_play_url(url)
}

fn write_entry(body: &str, ext: &str) -> std::io::Result<()> {
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
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    jizzcraft_core::fsutil::replace(&tmp, &d.join(format!("{nanos}.{ext}")))
}

fn take_ext(ext: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        if let Ok(txt) = std::fs::read_to_string(&p)
            && txt.len() <= MAX_LINK
        {
            let t = txt.trim();
            if ext == "magnet" {
                if t.starts_with("magnet:") {
                    out.push(t.to_owned());
                }
            } else if let Ok(u) = vet_link(t) {
                // Validated again on this end: the folder is not trusted.
                out.push(u);
            }
        }
        let _ = std::fs::remove_file(&p);
    }
    out
}

/// Read and remove every queued magnet (oldest first). Only `.magnet` files
/// this module wrote are touched.
pub fn take_all() -> Vec<String> {
    take_ext("magnet")
}

/// Read and remove every queued address (oldest first), vetted again. Only
/// `.link` files are touched.
pub fn take_links() -> Vec<String> {
    take_ext("link")
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

        // Addresses: round trip, normalized, junk refused and never queued.
        push_link("HTTPS://Example.com/v.m3u8?a=1").unwrap();
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:pw@example.com/",
            "http://localhost/x",
            "http://192.168.1.2/x",
            "https://exa mple.com/",
            &format!("https://example.com/{}", "a".repeat(5000)),
        ] {
            assert!(push_link(bad).is_err(), "accepted {bad:?}");
        }
        // A hand-written bad file is dropped on the reading end too.
        std::fs::write(dir().join("1.link"), "file:///etc/passwd").unwrap();
        assert!(take_all().is_empty(), "links are not magnets");
        assert_eq!(take_links(), vec!["https://example.com/v.m3u8?a=1"]);
        assert!(take_links().is_empty());
        assert!(std::fs::read_dir(dir()).unwrap().next().is_none());

        let first = try_primary_instance().expect("first launch takes the lock");
        assert!(try_primary_instance().is_none(), "second launch does not");
        drop(first);
    }
}
