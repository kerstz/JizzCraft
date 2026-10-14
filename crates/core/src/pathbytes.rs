//! Paths as bytes, for database keys and cache hashes.
//!
//! On Unix the bytes are the path's own, so existing libraries stay valid.
//! Elsewhere (Windows) paths are UTF-16: they are stored as UTF-8, which is
//! lossless for every path that is valid Unicode (all but pathological names
//! with unpaired surrogates).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The path separator as a byte, as it appears in stored paths.
pub const SEP: u8 = std::path::MAIN_SEPARATOR as u8;

#[cfg(unix)]
pub fn os_to_bytes(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
pub fn os_to_bytes(s: &OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
pub fn bytes_to_path(b: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(b))
}

#[cfg(not(unix))]
pub fn bytes_to_path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&b).into_owned())
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\share` -> `\\srv\share`. Any other
/// string is returned unchanged. Pure string work so it is testable anywhere.
pub fn strip_verbatim(s: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Cow::Owned(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        // Only plain drive paths; `\\?\Volume{{..}}` and friends stay as they are.
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return Cow::Borrowed(rest);
        }
    }
    Cow::Borrowed(s)
}

/// The verbatim prefix is what lets Windows open long paths and names with a
/// trailing dot or space or a device name. It may only be dropped when none of
/// that applies.
// A file called `CON` is still illegal on Windows. Some laws age like milk,
// some like DOS.
pub fn safe_without_verbatim(plain: &str) -> bool {
    if plain.chars().count() >= 240 {
        return false;
    }
    plain
        .trim_start_matches(['\\', '/'])
        .split(['\\', '/'])
        .all(|c| {
            if c.is_empty() || c.ends_with('.') && c != "." && c != ".." || c.ends_with(' ') {
                return c.is_empty() || c == "." || c == "..";
            }
            let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
            !(matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && stem.as_bytes()[3].is_ascii_digit()))
        })
}

/// `fs::canonicalize` without the Windows verbatim prefix, which neither
/// GStreamer's `file://` conversion nor external players accept.
pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    #[cfg(windows)]
    {
        if let Some(s) = c.to_str() {
            let plain = strip_verbatim(s);
            if safe_without_verbatim(&plain) {
                return Ok(PathBuf::from(plain.into_owned()));
            }
        }
    }
    Ok(c)
}

pub fn path_to_bytes(p: &Path) -> Vec<u8> {
    os_to_bytes(p.as_os_str())
}

#[cfg(test)]
mod tests {
    #[test]
    fn verbatim_is_kept_for_long_or_odd_names() {
        use super::safe_without_verbatim as ok;
        assert!(ok(r"C:\Users\me\clip.mp4"));
        assert!(!ok(r"C:\Users\me\clip."));
        assert!(!ok(r"C:\Users\me\CON.mp4"));
        assert!(!ok(r"C:\Users\me\a "));
        assert!(!ok(&format!(r"C:\{}", "x".repeat(300))));
    }

    #[test]
    fn verbatim_prefix_is_stripped_only_for_drive_and_unc() {
        use super::strip_verbatim as s;
        assert_eq!(s(r"\\?\C:\Users\x"), r"C:\Users\x");
        assert_eq!(s(r"\\?\UNC\srv\share\a"), r"\\srv\share\a");
        assert_eq!(s(r"\\?\Volume{1234}\a"), r"\\?\Volume{1234}\a");
        assert_eq!(s("/home/me"), "/home/me");
        assert_eq!(s(r"C:\plain"), r"C:\plain");
    }

    use super::*;

    #[test]
    fn unicode_paths_roundtrip() {
        for s in ["a/b.mp4", "dossier/été 日本語.png", "x y/z"] {
            let p = Path::new(s);
            assert_eq!(bytes_to_path(path_to_bytes(p)), p);
        }
        assert_eq!(SEP, std::path::MAIN_SEPARATOR as u8);
    }
}
