//! Interface language.
//!
//! English text in the source is the key. [`tr`] returns the translation for
//! the current language, or the English text when there is none, so a missing
//! entry is never an error. Catalogs live in `crates/ui/src/lang/*.rs` as
//! sorted-by-nothing `(english, translated)` slices. A missing translation
//! falls back to English, the lingua franca of error messages.
//!
//! Only the interface is translated. Library file names, user data and the
//! CLI's JSON are never touched.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Lang {
    En = 0,
    Fr,
    Es,
    De,
    It,
    Pt,
}

pub const ALL: [Lang; 6] = [Lang::En, Lang::Fr, Lang::Es, Lang::De, Lang::It, Lang::Pt];

impl Lang {
    /// ISO 639-1 code, as stored in the config.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Fr => "fr",
            Lang::Es => "es",
            Lang::De => "de",
            Lang::It => "it",
            Lang::Pt => "pt",
        }
    }

    /// Name in its own language, for the picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Fr => "Français",
            Lang::Es => "Español",
            Lang::De => "Deutsch",
            Lang::It => "Italiano",
            Lang::Pt => "Português",
        }
    }

    /// `fr`, `fr_FR.UTF-8`, `fr-CA`, `FR` …
    pub fn parse(s: &str) -> Option<Lang> {
        let head = s.split(['_', '-', '.', '@']).next()?.to_ascii_lowercase();
        ALL.into_iter().find(|l| l.code() == head)
    }

    fn table(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Lang::En => &[],
            Lang::Fr => crate::lang::fr::TABLE,
            Lang::Es => crate::lang::es::TABLE,
            Lang::De => crate::lang::de::TABLE,
            Lang::It => crate::lang::it::TABLE,
            Lang::Pt => crate::lang::pt::TABLE,
        }
    }
}

#[cfg(not(test))]
static CURRENT: AtomicU8 = AtomicU8::new(0);

// Unit tests run on parallel threads and several build a whole app, which
// sets the language: keep it per thread there so they cannot race.
#[cfg(test)]
thread_local! {
    static CURRENT_TL: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(not(test))]
pub fn set(l: Lang) {
    CURRENT.store(l as u8, Ordering::Relaxed);
}

#[cfg(test)]
pub fn set(l: Lang) {
    CURRENT_TL.with(|c| c.set(l as u8));
}

#[cfg(not(test))]
pub fn current() -> Lang {
    ALL[CURRENT.load(Ordering::Relaxed) as usize % ALL.len()]
}

#[cfg(test)]
pub fn current() -> Lang {
    ALL[CURRENT_TL.with(|c| c.get()) as usize % ALL.len()]
}

/// The first supported language among the usual locale variables.
pub fn from_env() -> Option<Lang> {
    ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .find_map(|v| v.split(':').find_map(Lang::parse))
}

fn maps() -> &'static [HashMap<&'static str, &'static str>; 6] {
    static M: OnceLock<[HashMap<&'static str, &'static str>; 6]> = OnceLock::new();
    M.get_or_init(|| ALL.map(|l| l.table().iter().copied().collect::<HashMap<_, _>>()))
}

/// Translate an English interface string.
pub fn tr(s: &'static str) -> &'static str {
    let l = current();
    if l == Lang::En {
        return s;
    }
    maps()[l as usize].get(s).copied().unwrap_or(s)
}

/// Catalog entries whose English text has `{}` holes, split around them.
type Templates = Vec<(Vec<&'static str>, &'static str)>;

fn templates() -> &'static [Templates; 6] {
    static T: OnceLock<[Templates; 6]> = OnceLock::new();
    T.get_or_init(|| {
        ALL.map(|l| {
            let mut v: Templates = l
                .table()
                .iter()
                .filter(|(en, _)| en.contains("{}"))
                .map(|(en, tr)| (en.split("{}").collect::<Vec<_>>(), *tr))
                .collect();
            // Most specific first: more fixed text wins over a greedy hole.
            v.sort_by_key(|(parts, _)| {
                std::cmp::Reverse(parts.iter().map(|p| p.len()).sum::<usize>())
            });
            v
        })
    })
}

/// Translate text that was built at run time (`format!`). Exact entries
/// match as in [`tr`]; entries with `{}` holes match the shape of the text,
/// and the filled values are carried over. Anything else is returned as is.
pub fn tr_dyn(s: &str) -> Cow<'_, str> {
    let l = current();
    if l == Lang::En {
        return Cow::Borrowed(s);
    }
    if let Some(t) = maps()[l as usize].get(s) {
        return Cow::Borrowed(t);
    }
    'next: for (parts, tr) in &templates()[l as usize] {
        let mut args: Vec<&str> = Vec::new();
        let Some(mut rest) = s.strip_prefix(parts[0]) else {
            continue;
        };
        for (i, p) in parts.iter().enumerate().skip(1) {
            let found = if i == parts.len() - 1 && !p.is_empty() {
                // The last literal must end the text.
                rest.strip_suffix(p).map(|v| (v.len(), p.len()))
            } else if p.is_empty() {
                Some((rest.len(), 0))
            } else {
                rest.find(p).map(|at| (at, p.len()))
            };
            let Some((at, plen)) = found else {
                continue 'next;
            };
            args.push(&rest[..at]);
            rest = &rest[at + plen..];
        }
        if !rest.is_empty() {
            continue;
        }
        let mut out = String::new();
        let mut it = args.into_iter();
        let mut t = *tr;
        while let Some(i) = t.find("{}") {
            out.push_str(&t[..i]);
            out.push_str(it.next().unwrap_or(""));
            t = &t[i + 2..];
        }
        out.push_str(t);
        return Cow::Owned(out);
    }
    Cow::Borrowed(s)
}

/// Translate a template and fill its `{}` placeholders in order.
pub fn trf(template: &'static str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut it = args.iter();
    let mut rest = tr(template);
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        out.push_str(it.next().copied().unwrap_or(""));
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The language is process-global: tests that set it run one at a time.
    static LANG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parses_locale_strings() {
        assert_eq!(Lang::parse("fr_FR.UTF-8"), Some(Lang::Fr));
        assert_eq!(Lang::parse("pt-BR"), Some(Lang::Pt));
        assert_eq!(Lang::parse("DE"), Some(Lang::De));
        assert_eq!(Lang::parse("xx"), None);
        assert_eq!(Lang::parse(""), None);
    }

    #[test]
    fn placeholders_are_filled_in_order() {
        let _g = LANG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set(Lang::En);
        assert_eq!(trf("a {} b {}", &["1", "2"]), "a 1 b 2");
        assert_eq!(trf("no holes", &[]), "no holes");
    }

    #[test]
    fn built_text_is_translated_by_shape() {
        let _g = LANG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A tiny catalog check through the French table's own entries.
        set(Lang::Fr);
        for (en, fr) in Lang::Fr.table().iter().filter(|(e, _)| e.contains("{}")) {
            let filled = en.replacen("{}", "7", 8);
            let want = fr.replacen("{}", "7", 8);
            assert_eq!(tr_dyn(&filled), want, "{en}");
        }
        set(Lang::En);
        assert_eq!(tr_dyn("12 items"), "12 items");
    }

    #[test]
    fn every_shortcut_description_is_translated_in_every_language() {
        use crate::keys::{KEYMAP, NETWORK_KEYMAP, TRIAGE_KEYMAP};
        let mut missing = Vec::new();
        for l in ALL.into_iter().filter(|l| *l != Lang::En) {
            let have: std::collections::HashSet<_> = l.table().iter().map(|(en, _)| *en).collect();
            for b in KEYMAP.iter().chain(TRIAGE_KEYMAP).chain(NETWORK_KEYMAP) {
                if !have.contains(b.doc) {
                    missing.push(format!("{}: {}", l.code(), b.doc));
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "untranslated shortcut docs:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn vault_warnings_are_translated_in_every_language() {
        let sentences = [
            "Conversion in progress",
            "A conversion is in progress. Closing interrupts the operation. This is safe, but you will have to start it again.",
            "Close anyway",
            "Keep waiting",
            "Playlist file in clear",
            "This M3U8 file is not encrypted, even though your library is. It lists the paths of your files in clear. Anyone who can read the file can read them.",
        ];
        for l in ALL.into_iter().filter(|l| *l != Lang::En) {
            for s in sentences {
                assert!(
                    l.table().iter().any(|(en, t)| *en == s && !t.is_empty()),
                    "{}: {s}",
                    l.code()
                );
            }
        }
    }

    #[test]
    fn catalogs_keep_their_placeholders_and_have_no_duplicates() {
        for l in ALL {
            let mut seen = std::collections::HashSet::new();
            for (en, tr) in l.table() {
                assert!(seen.insert(*en), "{}: duplicate key {en:?}", l.code());
                assert_eq!(
                    en.matches("{}").count(),
                    tr.matches("{}").count(),
                    "{}: placeholder mismatch in {en:?}",
                    l.code()
                );
                assert!(
                    !tr.trim().is_empty(),
                    "{}: empty translation for {en:?}",
                    l.code()
                );
            }
        }
    }
}
