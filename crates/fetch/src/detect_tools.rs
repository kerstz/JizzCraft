//! Finding resolver programs the user already installed.
//!
//! Only file names are matched (`yt-dlp`, `youtube-dl`, `yt-dlp_linux`) in the
//! folders on `PATH` and a few usual places. Each candidate is asked for its
//! version with an argument vector, an emptied environment and a 5 s limit.
//! Nothing is installed, downloaded or updated here. Files anyone could
//! replace are skipped, as they are when a resolver is registered.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// File names looked for, nothing else.
const NAMES: &[&str] = &[
    "yt-dlp",
    "youtube-dl",
    "yt-dlp_linux",
    #[cfg(windows)]
    "yt-dlp.exe",
    #[cfg(windows)]
    "youtube-dl.exe",
];
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    /// First line of `--version`, reduced to `[0-9A-Za-z.+-]`, 40 chars.
    pub version: String,
    /// Days since the release date, when the version is `YYYY.MM.DD`.
    pub age_days: Option<u32>,
}

/// Argument vector for a tool found here: the page address is one whole
/// argument after `--`, and the user's own configuration file is ignored.
pub fn vetted_argv(path: &Path) -> Vec<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |d| d.join(path))
    };
    vec![
        abs.to_string_lossy().into_owned(),
        "-J".into(),
        "--no-playlist".into(),
        "--ignore-config".into(),
        "--".into(),
        crate::resolver::URL_ARG.into(),
    ]
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub fn civil_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Age in days of a `YYYY.MM.DD` version on the day `today` (days since the
/// epoch). A version from the future is age 0.
pub fn age_days_at(version: &str, today: i64) -> Option<u32> {
    let mut it = version.trim().split('.');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    // A fourth numeric part (`2026.01.01.123456`) is fine; letters are not.
    let n = |s: &str| s.parse::<i64>().ok().filter(|_| !s.is_empty());
    let (y, m, d) = (n(y)?, n(m)?, n(d)?);
    if !(2000..=2200).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((today - civil_days(y, m, d)).clamp(0, i64::from(u32::MAX)) as u32)
}

fn today_days() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| (d.as_secs() / 86_400) as i64)
}

fn search_dirs(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    v.extend(extra.iter().cloned());
    if let Some(p) = std::env::var_os("PATH") {
        v.extend(std::env::split_paths(&p));
    }
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h).join(".local/bin"));
    }
    for d in ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        v.push(PathBuf::from(d));
    }
    let mut seen = Vec::new();
    v.retain(|d| {
        d.is_absolute() && !seen.contains(d) && {
            seen.push(d.clone());
            true
        }
    });
    v
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// `<tool> --version`: argv only, cleared environment (only `PATH`, so a
/// script's interpreter is found), 5 s, a few bytes of output.
fn version_of(path: &Path) -> Option<String> {
    let mut cmd = Command::new(path);
    cmd.arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(p) = std::env::var_os("PATH") {
        cmd.env("PATH", p);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.by_ref().take(512).read_to_end(&mut b);
        let _ = tx.send(b);
    });
    let t0 = Instant::now();
    let kill = |c: &mut std::process::Child| {
        #[cfg(unix)]
        // SAFETY: plain syscall on the process group created above.
        unsafe {
            libc::kill(-(c.id() as i32), libc::SIGKILL);
        }
        let _ = c.kill();
        let _ = c.wait();
    };
    let Ok(buf) = rx.recv_timeout(VERSION_TIMEOUT) else {
        kill(&mut child);
        return None;
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() >= VERSION_TIMEOUT => {
                kill(&mut child);
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                kill(&mut child);
                return None;
            }
        }
    };
    #[cfg(unix)]
    // SAFETY: as above.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    if !status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&buf);
    let v: String = line
        .lines()
        .next()?
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ".+-".contains(*c))
        .take(40)
        .collect();
    (!v.is_empty()).then_some(v)
}

/// Resolver tools installed on this computer. Blocking (runs each candidate
/// for its version): call it from a worker thread. Duplicates (the same file
/// reached through two folders or a symlink) are listed once.
pub fn find_resolver_tools(extra_dirs: &[PathBuf]) -> Vec<Found> {
    let today = today_days();
    let mut out: Vec<Found> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in search_dirs(extra_dirs) {
        for name in NAMES {
            let path = dir.join(name);
            if !is_executable(&path) {
                continue;
            }
            let Ok(real) = std::fs::canonicalize(&path) else {
                continue;
            };
            if seen.contains(&real) {
                continue;
            }
            seen.push(real);
            if crate::resolver::program_not_writable_by_others(&path).is_err() {
                continue;
            }
            let Some(version) = version_of(&path) else {
                continue;
            };
            let age_days = age_days_at(&version, today);
            out.push(Found {
                path,
                version,
                age_days,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_ages() {
        assert_eq!(civil_days(1970, 1, 1), 0);
        assert_eq!(civil_days(2000, 3, 1), 11_017);
        let today = civil_days(2026, 1, 31);
        assert_eq!(age_days_at("2026.01.01", today), Some(30));
        assert_eq!(age_days_at("2026.01.01.123456", today), Some(30));
        assert_eq!(age_days_at("2027.01.01", today), Some(0));
        assert_eq!(age_days_at("nightly", today), None);
        assert_eq!(age_days_at("2026.13.01", today), None);
    }

    #[test]
    fn vetted_argument_vector_is_fixed() {
        let v = vetted_argv(Path::new("/opt/t/yt-dlp"));
        assert_eq!(
            v,
            [
                "/opt/t/yt-dlp",
                "-J",
                "--no-playlist",
                "--ignore-config",
                "--",
                "{url}"
            ]
        );
    }
}
