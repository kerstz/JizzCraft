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

pub fn path_to_bytes(p: &Path) -> Vec<u8> {
    os_to_bytes(p.as_os_str())
}

#[cfg(test)]
mod tests {
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
