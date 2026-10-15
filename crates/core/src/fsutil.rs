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

/// `create_dir_all` with mode 0700 on Unix for every directory it creates
/// (library, config and cache directories hold private data; existing
/// directories are left as they are). Elsewhere it is `create_dir_all`.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// `OpenOptions` that create files readable by their owner only (0600 on
/// Unix, the default elsewhere).
pub fn private_file_options() -> std::fs::OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut o = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Make an existing file readable by its owner only (no-op off Unix and on
/// a missing file).
pub fn make_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Create an empty private file when `path` does not exist yet, so that
/// SQLite (which would use the umask) opens an already-0600 file.
pub fn precreate_private(path: &Path) {
    let _ = private_file_options()
        .write(true)
        .create_new(true)
        .open(path);
}

/// Filesystem types where SQLite's WAL (shared memory) is unsafe.
const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "9p",
    "afs",
    "ncpfs",
    "coda",
    "ceph",
    "glusterfs",
    "lustre",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.gvfsd-fuse",
    "fuse.davfs",
    "davfs",
];

/// Pure part of `is_network_fs`: parse mountinfo text and decide for `path`
/// (longest mount point prefix wins).
pub fn network_fs_in(mountinfo: &str, path: &Path) -> bool {
    let mut best: Option<(usize, bool)> = None;
    for line in mountinfo.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let mount = pre.split(' ').nth(4).unwrap_or("");
        let mount = mount.replace("\\040", " ");
        let fstype = post.split(' ').next().unwrap_or("");
        let p = Path::new(&mount);
        if path.starts_with(p) {
            let len = p.as_os_str().len();
            if best.is_none_or(|(l, _)| len >= l) {
                best = Some((len, NETWORK_FS.contains(&fstype)));
            }
        }
    }
    best.is_some_and(|(_, n)| n)
}

/// Is `path` on a network filesystem? Linux only (`/proc/self/mountinfo`);
/// `false` elsewhere or when unsure.
pub fn is_network_fs(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let abs = std::fs::canonicalize(path)
            .or_else(|_| {
                path.parent()
                    .map_or(Err(io::ErrorKind::NotFound.into()), std::fs::canonicalize)
            })
            .unwrap_or_else(|_| path.to_path_buf());
        std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|m| network_fs_in(&m, &abs))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_mounts_are_recognised() {
        let mi = "20 1 8:1 / / rw - ext4 /dev/sda1 rw\n40 20 0:35 / /mnt/nas rw - nfs4 srv:/x rw\n";
        let p = std::path::Path::new;
        assert!(super::network_fs_in(mi, p("/mnt/nas/lib/library.db")));
        assert!(!super::network_fs_in(mi, p("/home/u/library.db")));
    }

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
