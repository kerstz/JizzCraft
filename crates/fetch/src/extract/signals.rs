//! Why a page gave nothing: signals read from its visible text and markup.
//! They only choose which fixed sentence to show; nothing here gets past a
//! wall, and no text is kept.

use super::Signals;

/// Most visible text looked at.
pub const MAX_TEXT: usize = 256 * 1024;

const AGE_PHRASES: &[&str] = &[
    "are you 18",
    "18 years or older",
    "age verification",
    "verify your age",
    "i am 18",
    "adults only",
];
const DRM_WORDS: &[&str] = &[
    "widevine",
    "playready",
    "fairplay",
    "requestmediakeysystemaccess",
    "com.widevine.alpha",
];

/// What the walk over the page saw.
#[derive(Default)]
pub struct Facts {
    /// Lowercased visible text and a few attribute texts, capped.
    pub text: String,
    /// Lowercased script text (capped), for the DRM words.
    pub script_text: String,
    pub form_or_button: bool,
    pub password: bool,
    pub script_tags: usize,
    pub noscript: bool,
    pub empty_root: bool,
    pub meta_refresh: bool,
}

impl Facts {
    pub fn add_text(&mut self, t: &str) {
        push_capped(&mut self.text, t);
    }

    pub fn add_script(&mut self, t: &str) {
        push_capped(&mut self.script_text, t);
    }

    pub fn evaluate(&self) -> Signals {
        let has = |hay: &str, words: &[&str]| words.iter().any(|w| hay.contains(w));
        Signals {
            age_gate: self.form_or_button && has(&self.text, AGE_PHRASES),
            password_form: self.password,
            drm: has(&self.text, DRM_WORDS) || has(&self.script_text, DRM_WORDS),
            scripted: self.script_tags >= 3 && (self.noscript || self.empty_root),
            meta_refresh: self.meta_refresh,
        }
    }
}

fn push_capped(buf: &mut String, t: &str) {
    if buf.len() >= MAX_TEXT {
        return;
    }
    let room = MAX_TEXT - buf.len();
    let mut end = t.len().min(room);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    buf.push_str(&t[..end].to_lowercase());
    buf.push(' ');
}
