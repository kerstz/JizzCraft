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

#[derive(Clone, Debug)]
pub struct Report {
    pub checks: Vec<Check>,
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
        "vaapi",
        "hardware",
        "VA-API decoding",
        &["vah264dec", "vaapih264dec"],
    ),
];

fn dir_writable(p: &std::path::Path) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    let probe = p.join(".jizzcraft-doctor");
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
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
    Report {
        checks,
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

    /// Codecs the player cannot decode (hardware is optional, never listed).
    pub fn codecs_missing(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| c.group == "codec" && !c.ok)
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
