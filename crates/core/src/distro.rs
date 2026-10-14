//! Which distribution this is and which package fixes a missing GStreamer
//! piece. Pure: no I/O besides reading `/etc/os-release`, easy to test.
//!
//! The table lives in `assets/gst-packages.tsv`, shared with `install.sh`.

const TABLE: &str = include_str!("../../../assets/gst-packages.tsv");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Arch,
    Debian,
    Fedora,
    Suse,
    Unknown,
}

/// How this copy of the program was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Flatpak,
    AppImage,
    /// Image-based system: packages cannot be installed the usual way.
    Immutable,
    System,
}

/// `ID` and `ID_LIKE` of an os-release file to a package family.
pub fn family_from_os_release(text: &str) -> Family {
    let mut words: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k == "ID" || k == "ID_LIKE" {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            words.extend(v.split_whitespace().map(str::to_ascii_lowercase));
        }
    }
    let has = |names: &[&str]| words.iter().any(|w| names.contains(&w.as_str()));
    if has(&[
        "arch",
        "cachyos",
        "manjaro",
        "endeavouros",
        "artix",
        "garuda",
    ]) {
        Family::Arch
    } else if has(&[
        "debian",
        "ubuntu",
        "linuxmint",
        "pop",
        "raspbian",
        "kali",
        "zorin",
    ]) {
        Family::Debian
    } else if has(&["fedora", "rhel", "centos", "rocky", "almalinux", "nobara"]) {
        Family::Fedora
    } else if has(&[
        "opensuse",
        "suse",
        "sles",
        "opensuse-tumbleweed",
        "opensuse-leap",
    ]) {
        Family::Suse
    } else {
        Family::Unknown
    }
}

pub fn detect() -> Family {
    std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .map(|t| family_from_os_release(&t))
        .unwrap_or(Family::Unknown)
}

pub fn install_kind() -> Kind {
    let p = std::path::Path::new;
    if p("/.flatpak-info").exists() {
        Kind::Flatpak
    } else if std::env::var_os("APPIMAGE").is_some() {
        Kind::AppImage
    } else if p("/run/ostree-booted").exists() {
        Kind::Immutable
    } else {
        Kind::System
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub group: String,
    arch: String,
    debian: String,
    fedora: String,
    suse: String,
}

pub fn table() -> Vec<Row> {
    TABLE
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c.len() == 6).then(|| Row {
                id: c[0].into(),
                group: c[1].into(),
                arch: c[2].into(),
                debian: c[3].into(),
                fedora: c[4].into(),
                suse: c[5].into(),
            })
        })
        .collect()
}

impl Row {
    pub fn package(&self, f: Family) -> Option<&str> {
        match f {
            Family::Arch => Some(&self.arch),
            Family::Debian => Some(&self.debian),
            Family::Fedora => Some(&self.fedora),
            Family::Suse => Some(&self.suse),
            Family::Unknown => None,
        }
    }
}

/// The command a user can run to install what `ids` need. `None` when the
/// family is unknown: the caller then shows the element names instead.
pub fn fix_command(f: Family, ids: &[&str]) -> Option<String> {
    let rows = table();
    let mut pkgs: Vec<&str> = Vec::new();
    for id in ids {
        if let Some(p) = rows.iter().find(|r| r.id == *id).and_then(|r| r.package(f))
            && !pkgs.contains(&p)
        {
            pkgs.push(p);
        }
    }
    if pkgs.is_empty() {
        return None;
    }
    let list = pkgs.join(" ");
    Some(match f {
        Family::Arch => format!("sudo pacman -S --needed {list}"),
        Family::Debian => format!("sudo apt install {list}"),
        Family::Fedora => format!("sudo dnf install {list}"),
        Family::Suse => format!("sudo zypper install {list}"),
        Family::Unknown => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_from_os_release_files() {
        let f = |s| family_from_os_release(s);
        assert_eq!(f("ID=cachyos\nID_LIKE=arch\n"), Family::Arch);
        assert_eq!(f("ID=ubuntu\nID_LIKE=debian\n"), Family::Debian);
        assert_eq!(
            f("ID=\"linuxmint\"\nID_LIKE=\"ubuntu debian\""),
            Family::Debian
        );
        assert_eq!(f("ID=fedora\n"), Family::Fedora);
        assert_eq!(
            f("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\""),
            Family::Suse
        );
        assert_eq!(f("ID=nixos\n"), Family::Unknown);
        assert_eq!(f(""), Family::Unknown);
    }

    #[test]
    fn table_is_complete_and_commands_dedupe() {
        let t = table();
        assert!(t.len() >= 8);
        assert!(t.iter().all(|r| !r.arch.is_empty() && !r.debian.is_empty()));
        assert_eq!(
            fix_command(Family::Arch, &["h264", "hevc", "aac"]).as_deref(),
            Some("sudo pacman -S --needed gst-libav")
        );
        assert_eq!(
            fix_command(Family::Debian, &["h264", "av1"]).as_deref(),
            Some("sudo apt install gstreamer1.0-libav gstreamer1.0-plugins-bad")
        );
        assert_eq!(fix_command(Family::Unknown, &["h264"]), None);
        assert_eq!(fix_command(Family::Arch, &["nope"]), None);
    }
}
