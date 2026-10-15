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
    /// Subtitle choice: `"off"`, a language tag (`"fr"`), or `"-"` for the
    /// untagged sidecar. None: automatic (interface language, else first).
    pub subs: Option<String>,
    /// Preferred audio language for streams that keep their audio in
    /// separate tracks: a tag (`"fr"`), `""` for the stream's own default
    /// track. None: the interface language.
    pub audio_lang: Option<String>,
    /// Minutes of inactivity before an encrypted library locks itself.
    /// None: the default (15). `Some(0)`: never.
    pub autolock_minutes: Option<u32>,
    /// The boss key also locks an encrypted library.
    pub boss_locks: bool,
}

/// Language codes offered for the audio preference, in menu order.
pub const AUDIO_LANGS: [&str; 12] = [
    "en", "fr", "es", "de", "it", "pt", "nl", "pl", "ru", "ja", "zh", "ko",
];

/// Short, safe tag for the config: letters and `-` only, at most 12.
fn clean_tag(s: &str) -> Option<String> {
    jizzcraft_core::timed::clean_lang(s)
}

impl Config {
    /// Effective inactivity delay in minutes (0 = never), clamped to a day.
    pub fn autolock_minutes(&self) -> u32 {
        self.autolock_minutes
            .unwrap_or(crate::app::DEFAULT_AUTOLOCK_MINUTES)
            .min(crate::app::MAX_AUTOLOCK_MINUTES)
    }

    /// The language to ask a stream for: the saved one, else the interface
    /// language. `None` means "the stream's default track".
    pub fn effective_audio_lang(&self, interface: &str) -> Option<String> {
        match self.audio_lang.as_deref() {
            None => clean_tag(interface),
            Some(s) => clean_tag(s),
        }
    }

    /// Store a choice: `None` = follow the interface, `Some("")` = default
    /// track, otherwise a tag. Unusable tags are ignored (returns false).
    pub fn set_audio_lang(&mut self, choice: Option<&str>) -> bool {
        match choice {
            None => self.audio_lang = None,
            Some("") => self.audio_lang = Some(String::new()),
            Some(t) => match clean_tag(t) {
                Some(t) => self.audio_lang = Some(t.to_ascii_lowercase()),
                None => return false,
            },
        }
        true
    }
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
            let _ = jizzcraft_core::fsutil::create_private_dir_all(dir);
        }
        // Temp file then rename: a crash mid-write must not reset settings
        // (including the age confirmation).
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            // 0600 on Unix: the file holds the age confirmation and paths.
            let written = jizzcraft_core::fsutil::private_file_options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
                .is_ok();
            if written && jizzcraft_core::fsutil::replace(&tmp, &p).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_language_defaults_to_the_interface() {
        let c = Config::default();
        assert_eq!(c.audio_lang, None);
        assert_eq!(c.effective_audio_lang("fr"), Some("fr".to_owned()));
    }

    #[test]
    fn audio_language_empty_means_the_default_track() {
        let mut c = Config::default();
        assert!(c.set_audio_lang(Some("")));
        assert_eq!(c.effective_audio_lang("fr"), None);
    }

    #[test]
    fn audio_language_explicit_choice_wins_and_is_cleaned() {
        let mut c = Config::default();
        assert!(c.set_audio_lang(Some("PT-br")));
        assert_eq!(c.audio_lang.as_deref(), Some("pt-br"));
        assert_eq!(c.effective_audio_lang("de"), Some("pt-br".to_owned()));
        assert!(!c.set_audio_lang(Some("not a tag!")));
        assert_eq!(c.audio_lang.as_deref(), Some("pt-br"));
        assert!(c.set_audio_lang(None));
        assert_eq!(c.audio_lang, None);
    }

    #[test]
    fn audio_language_survives_a_json_round_trip() {
        for v in [None, Some(String::new()), Some("ja".to_owned())] {
            let c = Config {
                audio_lang: v.clone(),
                ..Config::default()
            };
            let back: Config = serde_json::from_slice(&serde_json::to_vec(&c).unwrap()).unwrap();
            assert_eq!(back.audio_lang, v);
        }
        // An older config without the key reads as "follow the interface".
        let old: Config = serde_json::from_str(r#"{"volume":0.5}"#).unwrap();
        assert_eq!(old.audio_lang, None);
    }

    #[test]
    fn every_offered_code_is_a_clean_tag() {
        for c in AUDIO_LANGS {
            assert_eq!(clean_tag(c).as_deref(), Some(c));
        }
    }
}
