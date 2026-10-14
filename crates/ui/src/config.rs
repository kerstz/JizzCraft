//! Tiny JSON config in the XDG config dir. Failure to read or write is never
//! fatal: the app simply falls back to defaults.
//! A lost config is a fresh start, not a tragedy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    pub age_confirmed: bool,
    /// The first-run wizard was finished or skipped.
    pub onboarding_done: bool,
    pub discreet: bool,
    pub screen_share_safe: bool,
    pub inspector_open: bool,
    /// Sidebar is shown unless closed (so existing configs get it too).
    pub sidebar_closed: bool,
    pub volume: Option<f32>,
    /// Keep every key under the left hand (WASD preset).
    pub left_hand: bool,
    /// Last sort: key name and direction.
    pub sort: Option<String>,
    pub sort_desc: bool,
    /// Hide play history (recently played, resume info) in the UI.
    pub hide_history: bool,
    /// Launch as "Viewer" (app ID, title, discreet) from the next start.
    pub disguise: bool,
    /// Play the next item when a video ends.
    pub autoplay_next: bool,
    /// Do not generate scrub previews on the timeline.
    pub no_previews: bool,
    /// Interface language code (`fr`, `es`, …). None: follow the system.
    pub language: Option<String>,
    /// Playful one-liners are on unless this is set.
    pub no_jokes: bool,
    /// Never show the 18+ gate (the owner already attested; local app only).
    pub skip_age_gate: bool,
    /// Local video the panic switch jumps to (`app.panic.maths`).
    pub panic_video: Option<PathBuf>,
    /// Unix seconds of the last start-up cache prune (at most once a day).
    pub cache_pruned_at: Option<u64>,
    /// Interface zoom, 0.75 to 2.0. None: 1.0.
    pub ui_zoom: Option<f32>,
    /// No UI animations (fades, easing).
    pub reduce_motion: bool,
}

pub fn path() -> PathBuf {
    jizzcraft_core::paths::config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        let mut c: Config = std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !path().exists() {
            c.inspector_open = true;
        }
        c
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Temp file then rename: a crash mid-write must not reset settings
        // (including the age confirmation).
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok()
                && jizzcraft_core::fsutil::replace(&tmp, &p).is_err()
            {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}
