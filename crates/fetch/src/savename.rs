//! Names and files for saved videos. Pure std, always compiled.
//!
//! A saved file is named from a sanitized title (never from the address), its
//! extension comes from the bytes (never from the address), and nothing is
//! ever overwritten: the final name and its `.part` are reserved with
//! `create_new`, and the finish is a rename that refuses to replace.

use crate::formats::sanitize_title;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Longest stem, in characters.
pub const MAX_STEM_CHARS: usize = 120;
/// Longest stem, in bytes (leaves room for ` (n)`, the extension and `.part`).
pub const MAX_STEM_BYTES: usize = 200;

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A file stem that is always exactly one safe path component. A disguised
/// library, or a missing or empty title, gives `video`.
pub fn stem_from_title(title: Option<&str>, disguised: bool) -> String {
    const FALLBACK: &str = "video";
    if disguised {
        return FALLBACK.into();
    }
    let Some(t) = title.and_then(sanitize_title) else {
        return FALLBACK.into();
    };
    let replaced: String = t
        .chars()
        .map(|c| {
            if matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
            ) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut s = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trim = |s: &str| s.trim_matches(|c| c == '.' || c == ' ').to_owned();
    s = trim(&s);
    if s.starts_with('-') {
        s.replace_range(0..1, "_");
    }
    let base = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&base.as_str()) {
        s.insert(0, '_');
    }
    s = s.chars().take(MAX_STEM_CHARS).collect();
    while s.len() > MAX_STEM_BYTES {
        s.pop();
    }
    s = trim(&s);
    if s.is_empty() { FALLBACK.into() } else { s }
}

/// File extension from the first bytes of the file. Never from the address.
pub fn ext_for(head: &[u8]) -> Option<&'static str> {
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some(if &head[8..12] == b"qt  " {
            "mov"
        } else {
            "mp4"
        });
    }
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        let w = &head[..head.len().min(64)];
        return Some(if w.windows(4).any(|x| x == b"webm") {
            "webm"
        } else {
            "mkv"
        });
    }
    if head.len() > 188 && head[0] == 0x47 && head[188] == 0x47 {
        return Some("ts");
    }
    if head.starts_with(b"FLV\x01") {
        return Some("flv");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return Some("avi");
    }
    if head.starts_with(b"OggS") {
        return Some("ogv");
    }
    None
}

/// `dir/stem.ext`, or the first `dir/stem (n).ext` where neither the final
/// name nor its `.part` exists. Symlinks, dangling ones included, count as
/// existing. The answer is a reservation hint: `create_part` is the real lock.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let taken = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let name = |n: u32| {
        let s = if n == 0 {
            stem.to_owned()
        } else {
            format!("{stem} ({n})")
        };
        if ext.is_empty() {
            s
        } else {
            format!("{s}.{ext}")
        }
    };
    let mut last = dir.join(name(0));
    for n in 0..1000 {
        let p = dir.join(name(n));
        if !taken(&p) && !taken(&part_path(&p)) {
            return p;
        }
        last = p;
    }
    last
}

/// `<final>.part`.
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Create the `.part` file: new (never an existing file or link), owner only.
pub fn create_part(path: &Path) -> io::Result<File> {
    jizzcraft_core::fsutil::private_file_options()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Move `from` to `to` without ever replacing something at `to` (a file or a
/// link, dangling or not): `AlreadyExists` then. A hard link is tried first,
/// then `renameat2(RENAME_NOREPLACE)` on Linux. The last resort checks and
/// renames, which leaves a window between the two calls (documented, only
/// reached on file systems with neither).
pub fn rename_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(from);
            return Ok(());
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
            ) =>
        {
            return Err(e);
        }
        Err(_) => {}
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        if let (Ok(a), Ok(b)) = (
            std::ffi::CString::new(from.as_os_str().as_bytes()),
            std::ffi::CString::new(to.as_os_str().as_bytes()),
        ) {
            // SAFETY: both paths are valid NUL-terminated strings that live
            // for the call; the syscall reads them and nothing else.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    libc::AT_FDCWD,
                    a.as_ptr(),
                    libc::AT_FDCWD,
                    b.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if rc == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EEXIST) => return Err(io::ErrorKind::AlreadyExists.into()),
                Some(libc::EINVAL | libc::ENOSYS | libc::EPERM | libc::ENOTSUP) => {}
                _ => return Err(e),
            }
        }
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// `path` (not followed if it is a link) is the very file `f` has open.
pub fn same_file(f: &File, path: &Path) -> bool {
    let (Ok(a), Ok(b)) = (f.metadata(), std::fs::symlink_metadata(path)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        // No stable file index in std: a regular file of the same length,
        // creation and write time is as close as it gets.
        b.is_file()
            && a.len() == b.len()
            && a.created().ok() == b.created().ok()
            && a.modified().ok() == b.modified().ok()
    }
}

/// Delete `path` only when it is still the file `f` created. `Ok(false)` when
/// it is gone or has been swapped for something else (which is left alone).
pub fn remove_own_part(f: &File, path: &Path) -> io::Result<bool> {
    if !same_file(f, path) {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hostile_titles_stay_one_component() {
        let long = "x".repeat(10_000);
        let accents = "\u{e9}".repeat(300);
        let titles = [
            "../../x",
            "..",
            ".",
            "CON.txt",
            "a/b\\c",
            "\u{202e}gpj.exe",
            long.as_str(),
            "a\0b",
            "-rf",
            "  ",
            "C:\\Windows",
            "lpt1",
            accents.as_str(),
        ];
        for t in titles {
            let s = stem_from_title(Some(t), false);
            assert!(!s.is_empty() && s.len() <= MAX_STEM_BYTES, "{t:?}");
            let p = Path::new(&s);
            assert_eq!(p.components().count(), 1, "{s:?}");
            assert!(matches!(
                p.components().next(),
                Some(std::path::Component::Normal(_))
            ));
            assert!(!s.contains(['/', '\\', '\0', ':']) && !s.starts_with(['-', '.', ' ']));
            assert!(!s.ends_with(['.', ' ']));
        }
        assert_eq!(stem_from_title(Some("-rf"), false), "_rf");
        assert!(stem_from_title(Some("CON.txt"), false).starts_with('_'));
        assert_eq!(stem_from_title(Some("Nice  clip"), false), "Nice clip");
        assert_eq!(stem_from_title(Some("Nice"), true), "video");
        assert_eq!(stem_from_title(None, false), "video");
        assert_eq!(stem_from_title(Some(".."), false), "video");
    }

    #[test]
    fn extension_comes_from_the_bytes() {
        let mut ts = vec![0u8; 400];
        ts[0] = 0x47;
        ts[188] = 0x47;
        let mut mp4 = vec![0, 0, 0, 24];
        mp4.extend(b"ftypisom");
        let mut mov = vec![0, 0, 0, 24];
        mov.extend(b"ftypqt  ");
        let mut webm = vec![0x1a, 0x45, 0xdf, 0xa3, 0x42, 0x82, 0x84];
        webm.extend(b"webm");
        let mkv = [0x1a, 0x45, 0xdf, 0xa3, 0x42, 0x82, 0x88, b'm', b'a', b't'];
        assert_eq!(ext_for(&mp4), Some("mp4"));
        assert_eq!(ext_for(&mov), Some("mov"));
        assert_eq!(ext_for(&webm), Some("webm"));
        assert_eq!(ext_for(&mkv), Some("mkv"));
        assert_eq!(ext_for(&ts), Some("ts"));
        assert_eq!(ext_for(b"FLV\x01\x05"), Some("flv"));
        assert_eq!(ext_for(b"RIFF\0\0\0\0AVI LIST"), Some("avi"));
        assert_eq!(ext_for(b"OggS\0\0"), Some("ogv"));
        assert_eq!(ext_for(b"<html><body>"), None);
        assert_eq!(ext_for(b""), None);
    }

    #[test]
    fn unique_path_skips_files_parts_and_links() {
        let d = tempfile::tempdir().unwrap();
        let p0 = unique_path(d.path(), "T", "mp4");
        assert_eq!(p0.file_name().unwrap(), "T.mp4");
        std::fs::write(&p0, b"x").unwrap();
        let p1 = unique_path(d.path(), "T", "mp4");
        assert_eq!(p1.file_name().unwrap(), "T (1).mp4");
        std::fs::write(part_path(&p1), b"x").unwrap();
        let p2 = unique_path(d.path(), "T", "mp4");
        assert_eq!(p2.file_name().unwrap(), "T (2).mp4");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path().join("missing"), &p2).unwrap();
            let p3 = unique_path(d.path(), "T", "mp4");
            assert_eq!(p3.file_name().unwrap(), "T (3).mp4");
            std::os::unix::fs::symlink(d.path().join("missing"), part_path(&p3)).unwrap();
            let p4 = unique_path(d.path(), "T", "mp4");
            assert_eq!(p4.file_name().unwrap(), "T (4).mp4");
        }
    }

    #[test]
    fn create_part_refuses_existing_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.mp4.part");
        let mut f = create_part(&p).unwrap();
        f.write_all(b"hi").unwrap();
        assert!(create_part(&p).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let l = d.path().join("l.part");
            std::os::unix::fs::symlink(d.path().join("elsewhere"), &l).unwrap();
            assert!(create_part(&l).is_err());
            assert!(!d.path().join("elsewhere").exists());
        }
    }

    #[test]
    fn rename_noclobber_never_replaces() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a.part"), d.path().join("a.mp4"));
        std::fs::write(&a, b"new").unwrap();
        std::fs::write(&b, b"old").unwrap();
        let e = rename_noclobber(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"old");
        assert!(a.exists());
        #[cfg(unix)]
        {
            let c = d.path().join("c.mp4");
            std::os::unix::fs::symlink(d.path().join("nowhere"), &c).unwrap();
            assert_eq!(
                rename_noclobber(&a, &c).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert!(!d.path().join("nowhere").exists());
        }
        std::fs::remove_file(&b).unwrap();
        rename_noclobber(&a, &b).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"new");
        assert!(!a.exists());
        assert_eq!(
            rename_noclobber(&a, &d.path().join("z"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn remove_own_part_refuses_a_swapped_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.part");
        let f = create_part(&p).unwrap();
        // Someone swaps the path for another file.
        std::fs::remove_file(&p).unwrap();
        std::fs::write(&p, b"theirs").unwrap();
        assert!(!remove_own_part(&f, &p).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), b"theirs");
        std::fs::remove_file(&p).unwrap();
        let f = create_part(&p).unwrap();
        assert!(same_file(&f, &p));
        assert!(remove_own_part(&f, &p).unwrap());
        assert!(!p.exists());
        assert!(!remove_own_part(&f, &p).unwrap());
        #[cfg(unix)]
        {
            let f = create_part(&p).unwrap();
            std::fs::remove_file(&p).unwrap();
            let other = d.path().join("other");
            std::fs::write(&other, b"o").unwrap();
            std::os::unix::fs::symlink(&other, &p).unwrap();
            assert!(!remove_own_part(&f, &p).unwrap());
            assert!(other.exists());
        }
    }
}
