//! Which edition this binary is. One source of truth for the About box, the
//! CLI `--version` and `doctor`.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edition {
    Offline,
    Online,
}

/// Online iff compiled with the `network` feature.
pub const EDITION: Edition = if cfg!(feature = "network") {
    Edition::Online
} else {
    Edition::Offline
};

/// `"offline"` or `"online"`.
pub fn label() -> &'static str {
    match EDITION {
        Edition::Offline => "offline",
        Edition::Online => "online",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserStatus {
    NotInstalled,
    Installed(PathBuf),
    /// Offline edition: the browser helper is not used.
    Off,
}

/// Looks for `../libexec/jizzcraft/jizzcraft-browser` next to the executable.
/// Name only: it is never executed here.
pub fn browser_status() -> BrowserStatus {
    if EDITION == Edition::Offline {
        return BrowserStatus::Off;
    }
    let found = std::env::current_exe()
        .ok()
        .and_then(|e| {
            e.parent()
                .map(|d| d.join("../libexec/jizzcraft/jizzcraft-browser"))
        })
        .filter(|p| usable(p));
    match found {
        Some(p) => BrowserStatus::Installed(p),
        None => BrowserStatus::NotInstalled,
    }
}

#[cfg(unix)]
fn usable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| {
        let mode = m.permissions().mode();
        m.is_file() && mode & 0o111 != 0 && mode & 0o002 == 0
    })
}

#[cfg(not(unix))]
fn usable(p: &std::path::Path) -> bool {
    p.is_file()
}

impl BrowserStatus {
    pub fn word(&self) -> &'static str {
        match self {
            Self::NotInstalled => "not installed",
            Self::Installed(_) => "installed",
            Self::Off => "off",
        }
    }
}
