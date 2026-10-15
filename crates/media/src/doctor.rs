//! "Can this machine play things?" Looks at the hardened GStreamer registry
//! and the user's directories. Never opens a connection.

use gstreamer::ElementFactory;
use jizzcraft_core::distro;

#[derive(Clone, Debug)]
pub struct Check {
    /// Row id in `assets/gst-packages.tsv`.
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub ok: bool,
    /// The decoder that was found (or the first alternative that was not).
    pub element: String,
}

/// Advisory result: never changes the exit code, always has a plain sentence.
#[derive(Clone, Debug)]
pub struct Note {
    pub id: &'static str,
    pub warn: bool,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub checks: Vec<Check>,
    /// Filesystem, disk space, clock and renderer notes.
    pub notes: Vec<Note>,
    pub dirs_ok: Vec<(&'static str, bool)>,
    pub family: distro::Family,
    pub kind: distro::Kind,
}

/// `(id, group, label, alternatives)`: one decoder of the list is enough.
const WANTED: &[(&str, &str, &str, &[&str])] = &[
    ("base", "required", "playbin", &["playbin"]),
    ("base", "required", "app sink", &["appsink"]),
    ("base", "required", "video convert", &["videoconvert"]),
    ("good", "required", "MP4 demuxer", &["qtdemux"]),
    ("good", "required", "Matroska demuxer", &["matroskademux"]),
    (
        "h264",
        "codec",
        "H.264",
        &["avdec_h264", "openh264dec", "vah264dec"],
    ),
    ("hevc", "codec", "HEVC", &["avdec_h265", "vah265dec"]),
    ("av1", "codec", "AV1", &["dav1ddec", "av1dec", "vaav1dec"]),
    ("vp9", "codec", "VP9", &["vp9dec", "avdec_vp9", "vavp9dec"]),
    ("aac", "codec", "AAC", &["avdec_aac", "faad", "fdkaacdec"]),
    ("opus", "codec", "Opus", &["opusdec", "avdec_opus"]),
    (
        "streaming",
        "streaming",
        "MPEG-TS demuxer (HLS)",
        &["tsdemux"],
    ),
    ("streaming", "streaming", "H.264 parser", &["h264parse"]),
    ("good", "streaming", "AAC parser", &["aacparse"]),
    ("good", "streaming", "fMP4 demuxer (DASH)", &["qtdemux"]),
    (
        "vaapi",
        "hardware",
        "VA-API decoding",
        &["vah264dec", "vaapih264dec"],
    ),
];

fn dir_writable(p: &std::path::Path) -> bool {
    if jizzcraft_core::fsutil::create_private_dir_all(p).is_err() {
        return false;
    }
    let probe = p.join(".jizzcraft-doctor");
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

type RendererProbe = fn() -> Vec<String>;
static RENDERER_PROBE: std::sync::OnceLock<RendererProbe> = std::sync::OnceLock::new();

/// The desktop app registers a function listing GPU adapters (it owns wgpu).
/// Without one the renderer note is simply absent.
pub fn set_renderer_probe(f: RendererProbe) {
    let _ = RENDERER_PROBE.set(f);
}

/// Mount point and filesystem type of the longest mount point containing
/// `path`, from the text of `/proc/self/mountinfo`.
pub fn fs_type_for(mountinfo: &str, path: &std::path::Path) -> Option<String> {
    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\'
                && let Some(oct) = s.get(i + 1..i + 4)
                && let Ok(v) = u8::from_str_radix(oct, 8)
            {
                out.push(v);
                i += 4;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    let mut best: Option<(usize, String)> = None;
    for l in mountinfo.lines() {
        let (head, tail) = l.split_once(" - ")?;
        let mp = unescape(head.split(' ').nth(4)?);
        let fstype = tail.split(' ').next()?.to_owned();
        if path.starts_with(&mp) {
            let n = mp.len();
            if best.as_ref().is_none_or(|(m, _)| n >= *m) {
                best = Some((n, fstype));
            }
        }
    }
    best.map(|(_, t)| t)
}

/// Filesystems where SQLite WAL (shared memory, locks) is not safe.
pub fn is_network_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "nfs" | "nfs4" | "cifs" | "smb" | "smb3" | "smbfs" | "9p" | "fuse.sshfs" | "ceph"
    )
}

/// Free bytes where `p` lives, through `df -Pk` (argv, no shell). Unix only.
fn free_bytes(p: &std::path::Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(p)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    kb.checked_mul(1024)
}

/// Earliest believable date: `SOURCE_DATE_EPOCH` of the build, else a floor.
fn build_epoch() -> u64 {
    option_env!("SOURCE_DATE_EPOCH")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_767_225_600) // 2026-01-01
}

fn unix_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn clock_note(dir: &std::path::Path) -> Note {
    let mut warn = false;
    let mut text = String::from("system clock looks sane");
    if let Some(now) = unix_now() {
        if now < build_epoch() {
            warn = true;
            text = "system clock is earlier than this build: TLS and file dates will be wrong; set the clock".into();
        } else {
            // A fresh file's mtime comes from the filesystem's clock.
            let probe = dir.join(".jizzcraft-clock");
            if std::fs::write(&probe, b"x").is_ok() {
                let m = std::fs::metadata(&probe)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let _ = std::fs::remove_file(&probe);
                if m.is_some_and(|m| m.abs_diff(now) > 86_400) {
                    warn = true;
                    text =
                        "clock and filesystem disagree by more than a day: file dates may be wrong"
                            .into();
                }
            }
        }
    }
    Note {
        id: "clock",
        warn,
        text,
    }
}

fn fs_notes(dirs: &[(&'static str, std::path::PathBuf)]) -> Vec<Note> {
    let mut v = Vec::new();
    let mi = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/self/mountinfo").ok()
    } else {
        None
    };
    for (name, p) in dirs {
        if let Some(mi) = &mi {
            let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            if let Some(t) = fs_type_for(mi, &real)
                && is_network_fs(&t)
            {
                v.push(Note {
                    id: "filesystem",
                    warn: true,
                    text: format!(
                        "{name} folder is on a network filesystem ({t}): SQLite WAL is unsafe there, move it to a local disk (--home)"
                    ),
                });
            }
        }
        if let Some(f) = free_bytes(p)
            && f < (1 << 30)
        {
            v.push(Note {
                id: "disk",
                warn: true,
                text: format!("{name} folder has only {} MiB free", f >> 20),
            });
        }
    }
    v
}

/// GStreamer version string, e.g. `1.26.1`, once initialised.
pub fn gst_version() -> Option<String> {
    let _ = crate::init();
    let (a, b, c, _) = gstreamer::version();
    Some(format!("{a}.{b}.{c}"))
}

pub fn run() -> Report {
    // Same registry hardening as playback, so the answer is the real one.
    let _ = crate::init();
    let checks = WANTED
        .iter()
        .map(|(id, group, label, alts)| {
            let found = alts.iter().find(|e| ElementFactory::find(e).is_some());
            Check {
                id,
                group,
                label,
                ok: found.is_some(),
                element: found.unwrap_or(&alts[0]).to_string(),
            }
        })
        .collect();
    let data = jizzcraft_core::paths::db_path()
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let cache = jizzcraft_core::paths::cache_dir();
    let mut notes = fs_notes(&[("data", data.clone()), ("cache", cache.clone())]);
    notes.push(clock_note(&cache));
    if let Some(probe) = RENDERER_PROBE.get() {
        let names = probe();
        notes.push(if names.is_empty() {
            Note {
                id: "renderer",
                warn: true,
                text: "no GPU adapter found: try --renderer gl, or install Vulkan drivers".into(),
            }
        } else {
            Note {
                id: "renderer",
                warn: false,
                text: format!("GPU adapters: {}", names.join(", ")),
            }
        });
    }
    Report {
        checks,
        notes,
        dirs_ok: vec![
            (
                "data",
                dir_writable(
                    jizzcraft_core::paths::db_path()
                        .parent()
                        .unwrap_or(std::path::Path::new(".")),
                ),
            ),
            ("config", dir_writable(&jizzcraft_core::paths::config_dir())),
            ("cache", dir_writable(&jizzcraft_core::paths::cache_dir())),
        ],
        family: distro::detect(),
        kind: distro::install_kind(),
    }
}

impl Report {
    /// Human-readable report, ending with the fix command when one is known.
    pub fn render_text(&self) -> String {
        let mut s = String::new();
        for c in &self.checks {
            let mark = if c.ok {
                "ok"
            } else if c.group == "hardware" {
                "--"
            } else {
                "MISSING"
            };
            s.push_str(&format!("{mark:<8} {:<10} {}\n", c.group, c.label));
        }
        for (n, ok) in &self.dirs_ok {
            let mark = if *ok { "ok" } else { "MISSING" };
            s.push_str(&format!("{mark:<8} {:<10} {n} folder\n", "folders"));
        }
        for n in &self.notes {
            let mark = if n.warn { "WARN" } else { "ok" };
            s.push_str(&format!("{mark:<8} {:<10} {}\n", n.id, n.text));
        }
        match (self.fix(), self.exit_code()) {
            (_, 0) => s.push_str("\nAll good.\n"),
            (Some(cmd), _) => s.push_str(&format!("\nTo fix, run:\n  {cmd}\n")),
            (None, _) => s.push_str(
                "\nInstall the GStreamer plugin packages for your distribution (base, good, libav).\n",
            ),
        }
        s
    }

    pub fn required_missing(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| c.group == "required" && !c.ok)
            .collect()
    }

    /// Codecs and streaming parts the player lacks (hardware is optional, never listed).
    pub fn codecs_missing(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| (c.group == "codec" || c.group == "streaming") && !c.ok)
            .collect()
    }

    /// 0 fine, 1 something required is missing, 2 only codecs are missing.
    pub fn exit_code(&self) -> u8 {
        if !self.required_missing().is_empty() || self.dirs_ok.iter().any(|(_, ok)| !ok) {
            1
        } else if !self.codecs_missing().is_empty() {
            2
        } else {
            0
        }
    }

    /// What to run to fix what is missing, or a note when it cannot be run.
    pub fn fix(&self) -> Option<String> {
        use distro::Kind;
        match self.kind {
            Kind::Flatpak => {
                return Some(
                    "flatpak install flathub org.freedesktop.Platform.codecs-extra".into(),
                );
            }
            Kind::Immutable => {
                return Some(
                    "image-based system: use the Flatpak build or layer the packages".into(),
                );
            }
            Kind::AppImage | Kind::System => {}
        }
        let mut ids: Vec<&str> = self.required_missing().iter().map(|c| c.id).collect();
        ids.extend(self.codecs_missing().iter().map(|c| c.id));
        distro::fix_command(self.family, &ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const MI: &str = "\
25 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 25 0:35 / /mnt/nas\\040share rw - nfs4 srv:/x rw
41 25 0:36 / /mnt/ssh rw - fuse.sshfs u@h:/ rw
";

    #[test]
    fn longest_mount_wins_and_escapes_decode() {
        assert_eq!(
            fs_type_for(MI, Path::new("/home/x")).as_deref(),
            Some("ext4")
        );
        assert_eq!(
            fs_type_for(MI, Path::new("/mnt/nas share/lib")).as_deref(),
            Some("nfs4")
        );
        assert!(is_network_fs("nfs4") && is_network_fs("fuse.sshfs") && !is_network_fs("ext4"));
    }

    #[test]
    fn streaming_group_is_listed() {
        assert!(
            WANTED
                .iter()
                .any(|w| w.1 == "streaming" && w.3 == ["tsdemux"])
        );
    }
}
