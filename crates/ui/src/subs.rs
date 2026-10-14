//! Subtitle state of one open video: the sidecar tracks found next to the
//! file (read on a worker thread), which one is shown, and the timing shift.
//! The text is drawn by the UI as an overlay; GStreamer never sees it.

use jizzcraft_core::subtitles::{Format, Track};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// Timing shift is clamped to a minute either way.
pub const MAX_DELAY_MS: i64 = 60_000;
/// Step of the keys and of the menu buttons.
pub const STEP_MS: i64 = 100;
/// Saved preference meaning "no subtitles".
pub const PREF_OFF: &str = "off";
/// Saved preference meaning "the track without a language tag".
pub const PREF_UNTAGGED: &str = "-";

#[derive(Default)]
pub struct Subs {
    tracks: Vec<Track>,
    pending: Option<Receiver<Vec<Track>>>,
    active: Option<usize>,
    /// Positive: subtitles appear later than the file says.
    delay_ms: i64,
    /// Saved choice, applied once the tracks have loaded.
    pref: Option<String>,
    /// The user already chose in this session: the load must not override.
    chosen: bool,
    ui_lang: String,
    notice: Option<(String, Instant)>,
}

/// Which track to show: the saved choice, else the interface language, else
/// the first one. `None` also when the user saved "off".
pub fn pick(langs: &[Option<&str>], pref: Option<&str>, ui_lang: &str) -> Option<usize> {
    if langs.is_empty() || pref == Some(PREF_OFF) {
        return None;
    }
    if let Some(p) = pref {
        let hit = langs.iter().position(|l| match l {
            Some(l) => l.eq_ignore_ascii_case(p),
            None => p == PREF_UNTAGGED,
        });
        if hit.is_some() {
            return hit;
        }
    }
    let same = |l: &str| {
        let head = l.split(['-', '_']).next().unwrap_or(l);
        head.eq_ignore_ascii_case(ui_lang)
    };
    langs.iter().position(|l| l.is_some_and(same)).or(Some(0))
}

/// Point size of the text. Points, so the interface zoom scales it; the cap
/// keeps it readable in a small window.
pub fn font_size(fullscreen: bool, area_height: f32) -> f32 {
    let base: f32 = if fullscreen { 28.0 } else { 20.0 };
    base.min((area_height / 8.0).max(12.0))
}

impl Subs {
    /// Look for sidecars of `media` on a worker thread (never on the UI one).
    pub fn spawn(
        media: PathBuf,
        pref: Option<String>,
        ui_lang: &str,
        wake: impl Fn() + Send + 'static,
    ) -> Subs {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let tracks = jizzcraft_core::subtitles::load_all(&media);
            if !tracks.is_empty() {
                let _ = tx.send(tracks);
                wake();
            }
        });
        Subs {
            pending: Some(rx),
            pref,
            ui_lang: ui_lang.to_owned(),
            ..Subs::default()
        }
    }

    /// Take the tracks once the worker is done. True when they just arrived.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        match rx.try_recv() {
            Ok(tracks) => {
                self.pending = None;
                self.install(tracks);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(_) => {
                self.pending = None;
                false
            }
        }
    }

    fn install(&mut self, tracks: Vec<Track>) {
        if !self.chosen {
            let langs: Vec<Option<&str>> = tracks.iter().map(|t| t.lang.as_deref()).collect();
            self.active = pick(&langs, self.pref.as_deref(), &self.ui_lang);
        }
        self.tracks = tracks;
    }

    #[cfg(test)]
    fn with_tracks(tracks: Vec<Track>, pref: Option<&str>, ui_lang: &str) -> Subs {
        let mut s = Subs {
            pref: pref.map(str::to_owned),
            ui_lang: ui_lang.to_owned(),
            ..Subs::default()
        };
        s.install(tracks);
        s
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn active(&self) -> Option<usize> {
        self.active
    }

    pub fn delay_ms(&self) -> i64 {
        self.delay_ms
    }

    /// Menu labels: the language tag, else the format; repeats get a number.
    pub fn labels(&self) -> Vec<String> {
        let base: Vec<String> = self
            .tracks
            .iter()
            .map(|t| match &t.lang {
                Some(l) => l.to_uppercase(),
                None => match t.format {
                    Format::Srt => "SRT".to_owned(),
                    Format::Vtt => "VTT".to_owned(),
                },
            })
            .collect();
        base.iter()
            .enumerate()
            .map(|(i, b)| {
                let before = base[..i].iter().filter(|x| *x == b).count();
                let total = base.iter().filter(|x| *x == b).count();
                if total > 1 {
                    format!("{b} {}", before + 1)
                } else {
                    b.clone()
                }
            })
            .collect()
    }

    /// Choose a track (None: off). The choice is remembered for this session
    /// and returned as the value to save in the settings.
    pub fn set_active(&mut self, i: Option<usize>) -> Option<String> {
        if self.tracks.is_empty() {
            return None;
        }
        self.chosen = true;
        self.active = i.filter(|i| *i < self.tracks.len());
        let saved = match self.active {
            None => PREF_OFF.to_owned(),
            Some(i) => self.tracks[i]
                .lang
                .clone()
                .unwrap_or_else(|| PREF_UNTAGGED.to_owned()),
        };
        let shown = match self.active {
            None => "SUBTITLES OFF".to_owned(),
            Some(i) => format!("SUBTITLES: {}", self.labels()[i]),
        };
        self.notice = Some((shown, Instant::now()));
        self.pref = Some(saved.clone());
        Some(saved)
    }

    /// Next track, then off, then the first again.
    pub fn cycle(&mut self) -> Option<String> {
        let n = self.tracks.len();
        let next = match self.active {
            None => 0,
            Some(i) => i + 1,
        };
        self.set_active((next < n).then_some(next))
    }

    pub fn nudge(&mut self, delta_ms: i64) {
        self.delay_ms = (self.delay_ms + delta_ms).clamp(-MAX_DELAY_MS, MAX_DELAY_MS);
        self.announce_delay();
    }

    pub fn reset_delay(&mut self) {
        self.delay_ms = 0;
        self.announce_delay();
    }

    fn announce_delay(&mut self) {
        let s = format!("SUBTITLE TIMING: {:+.1} S", self.delay_ms as f64 / 1000.0);
        self.notice = Some((s, Instant::now()));
    }

    /// Lines to draw at media position `pos`.
    pub fn lines_at(&self, pos: Duration) -> Vec<&str> {
        let Some(t) = self.active.and_then(|i| self.tracks.get(i)) else {
            return Vec::new();
        };
        let ms = pos.as_millis() as i64 - self.delay_ms;
        if ms < 0 {
            return Vec::new();
        }
        t.active_at(ms as u64)
    }

    /// The short HUD message (English, goes through `tr_dyn`), for 2 s.
    pub fn notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(2))
            .map(|(s, _)| s.as_str())
    }
}

/// Only local paths have sidecars; streams never do.
pub fn spawn_for(path: &Path, pref: Option<String>, wake: impl Fn() + Send + 'static) -> Subs {
    Subs::spawn(
        path.to_path_buf(),
        pref,
        crate::i18n::current().code(),
        wake,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jizzcraft_core::subtitles::{Cue, Track};

    fn track(lang: Option<&str>, fmt: Format, at_ms: u64) -> Track {
        Track {
            lang: lang.map(str::to_owned),
            format: fmt,
            cues: vec![Cue {
                start_ms: at_ms,
                end_ms: at_ms + 1_000,
                text: "x".into(),
            }],
        }
    }

    #[test]
    fn pick_prefers_saved_choice_then_ui_language_then_first() {
        let l = [None, Some("en"), Some("pt-br")];
        assert_eq!(pick(&l, Some("off"), "en"), None);
        assert_eq!(pick(&l, Some("en"), "fr"), Some(1));
        assert_eq!(pick(&l, Some("-"), "fr"), Some(0));
        assert_eq!(pick(&l, None, "pt"), Some(2));
        assert_eq!(pick(&l, Some("de"), "fr"), Some(0));
        assert_eq!(pick(&[], None, "en"), None);
    }

    #[test]
    fn cycle_walks_tracks_then_off_and_returns_the_saved_value() {
        let mut s = Subs::with_tracks(
            vec![
                track(Some("fr"), Format::Srt, 0),
                track(Some("en"), Format::Vtt, 0),
            ],
            Some("off"),
            "en",
        );
        assert_eq!(s.active(), None);
        assert_eq!(s.cycle().as_deref(), Some("fr"));
        assert_eq!(s.cycle().as_deref(), Some("en"));
        assert_eq!(s.cycle().as_deref(), Some("off"));
        assert_eq!(s.active(), None);
        assert_eq!(s.cycle().as_deref(), Some("fr"));
    }

    #[test]
    fn delay_shifts_cues_and_is_clamped() {
        let mut s = Subs::with_tracks(vec![track(None, Format::Srt, 10_000)], None, "en");
        assert_eq!(s.lines_at(Duration::from_millis(10_500)), ["x"]);
        s.nudge(2_000);
        assert!(s.lines_at(Duration::from_millis(10_500)).is_empty());
        assert_eq!(s.lines_at(Duration::from_millis(12_500)), ["x"]);
        s.nudge(MAX_DELAY_MS * 3);
        assert_eq!(s.delay_ms(), MAX_DELAY_MS);
        s.nudge(-MAX_DELAY_MS * 5);
        assert_eq!(s.delay_ms(), -MAX_DELAY_MS);
        s.reset_delay();
        assert_eq!(s.delay_ms(), 0);
        // A negative delay before the start never underflows.
        s.nudge(5_000);
        assert!(s.lines_at(Duration::from_millis(1_000)).is_empty());
    }

    #[test]
    fn labels_use_language_or_format_and_number_repeats() {
        let s = Subs::with_tracks(
            vec![
                track(None, Format::Srt, 0),
                track(None, Format::Vtt, 0),
                track(Some("fr"), Format::Srt, 0),
                track(Some("fr"), Format::Vtt, 0),
            ],
            None,
            "en",
        );
        assert_eq!(s.labels(), ["SRT", "VTT", "FR 1", "FR 2"]);
    }

    #[test]
    fn off_when_nothing_loaded_and_font_scales_with_points() {
        let mut s = Subs::default();
        assert!(s.is_empty() && s.cycle().is_none());
        assert!(font_size(true, 1000.0) > font_size(false, 1000.0));
        assert_eq!(font_size(false, 1000.0), 20.0);
        assert_eq!(font_size(false, 100.0), 12.5);
        assert_eq!(font_size(false, 20.0), 12.0);
    }
}
