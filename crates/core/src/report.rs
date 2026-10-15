//! The redacted support report (`jizzcraft-cli doctor --report`).
//!
//! Built from an ALLOW-LIST: every line is a fixed key with a value that is a
//! number, a boolean, or a short string passed through [`clean`]. Nothing is
//! ever copied from the library or the config except the listed booleans, so
//! no path, file name, URL, key or folder name can reach it.

use crate::distro::{Family, Kind};
use serde_json::Value;

/// Config keys whose boolean value may appear in the report.
pub const CONFIG_FLAGS: &[&str] = &[
    "age_confirmed",
    "discreet",
    "screen_share_safe",
    "left_hand",
    "hide_history",
    "disguise",
    "autoplay_next",
    "no_previews",
    "no_jokes",
    "skip_age_gate",
];

/// Interface languages that may be reported verbatim.
const LANGS: &[&str] = &["en", "fr", "es", "de", "it", "pt"];

/// Vault state as the report may say it: a word, never a count or a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultState {
    Off,
    On,
    /// Encrypted and not opened: the report then carries no library statistics.
    Locked,
}

/// What the caller collects. No field holds user text.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub app_version: String,
    pub os: String,
    pub arch: String,
    pub family: Option<Family>,
    pub kind: Option<Kind>,
    pub gst_version: Option<String>,
    /// `(check id, ok)` pairs from the doctor.
    pub plugins: Vec<(String, bool)>,
    pub schema_version: Option<i64>,
    pub items: Option<u64>,
    pub folders: Option<u64>,
    /// `None` when unknown (printed as `unknown`).
    pub vault: Option<VaultState>,
    /// Parsed `config.json`, if any. Only [`CONFIG_FLAGS`] and `language` are read.
    pub config: Option<Value>,
}

/// Keep a short, boring string: ASCII letters, digits and `. _ - +` only.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect()
}

fn family_name(f: Family) -> &'static str {
    match f {
        Family::Arch => "arch",
        Family::Debian => "debian",
        Family::Fedora => "fedora",
        Family::Suse => "suse",
        Family::Unknown => "unknown",
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Flatpak => "flatpak",
        Kind::AppImage => "appimage",
        Kind::Immutable => "immutable",
        Kind::System => "system",
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "unknown".into(), |v| v.to_string())
}

/// The report as `key: value` lines.
pub fn render(i: &Input) -> String {
    let mut o = String::from("JizzCraft support report (no paths, names, URLs or keys)\n");
    let mut line = |k: &str, v: String| o.push_str(&format!("{k}: {v}\n"));
    line("version", clean(&i.app_version));
    line("os", clean(&i.os));
    line("arch", clean(&i.arch));
    line("distro_family", opt(i.family.map(family_name)));
    line("install_kind", opt(i.kind.map(kind_name)));
    line(
        "gstreamer",
        i.gst_version.as_deref().map_or("unavailable".into(), clean),
    );
    for (id, ok) in &i.plugins {
        line(
            &format!("plugin.{}", clean(id)),
            if *ok { "ok" } else { "missing" }.into(),
        );
    }
    line("schema_version", opt(i.schema_version));
    line("items", opt(i.items));
    line("folders", opt(i.folders));
    line(
        "vault",
        match i.vault {
            Some(VaultState::Off) => "off",
            Some(VaultState::On) => "on",
            Some(VaultState::Locked) => "locked",
            None => "unknown",
        }
        .into(),
    );
    let cfg = i.config.as_ref();
    for k in CONFIG_FLAGS {
        let v = cfg.and_then(|c| c.get(*k)).and_then(Value::as_bool);
        line(&format!("config.{k}"), opt(v));
    }
    let lang = cfg
        .and_then(|c| c.get("language"))
        .and_then(Value::as_str)
        .map(|l| {
            if LANGS.contains(&l) {
                l.to_owned()
            } else {
                "other".into()
            }
        });
    line("config.language", lang.unwrap_or_else(|| "system".into()));
    line(
        "config.panic_video_set",
        opt(cfg.map(|c| c.get("panic_video").is_some_and(|v| !v.is_null()))),
    );
    o
}

// ---- crash notes ---------------------------------------------------------

/// Last two components of a source path: enough to find the line, nothing
/// about the machine (registry paths carry the user name).
fn short_location(file: &str) -> String {
    let parts: Vec<&str> = file.split(['/', '\\']).collect();
    let n = parts.len().saturating_sub(2);
    parts[n..]
        .iter()
        .map(|p| clean(p))
        .collect::<Vec<_>>()
        .join("/")
}

/// The text of a crash note: thread, `file:line`, version. The panic message
/// is included only when `debug` (JIZZCRAFT_DEBUG=1) because it can hold paths.
pub fn crash_text(
    app: &str,
    version: &str,
    thread: &str,
    loc: Option<(&str, u32)>,
    message: Option<&str>,
    debug: bool,
) -> String {
    let mut s = format!(
        "{app} {} crashed\nthread: {}\n",
        clean(version),
        clean(thread)
    );
    match loc {
        Some((f, l)) => s.push_str(&format!("at: {}:{l}\n", short_location(f))),
        None => s.push_str("at: unknown\n"),
    }
    if debug && let Some(m) = message {
        s.push_str(&format!("message: {m}\n"));
    }
    s
}

/// Write `crash-<unixts>.txt` into `paths::state_dir()` on panic, then run the previous hook.
/// Never panics itself; write errors are ignored.
pub fn install_crash_hook(app: &'static str, version: &'static str) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let th = std::thread::current();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned());
        let text = crash_text(
            app,
            version,
            th.name().unwrap_or("unnamed"),
            info.location().map(|l| (l.file(), l.line())),
            msg.as_deref(),
            std::env::var_os("JIZZCRAFT_DEBUG").is_some_and(|v| v == "1"),
        );
        // Resolved now, so a --home given after startup is honoured.
        let dir = crate::paths::state_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join(format!("crash-{ts}.txt")), text);
        }
        prev(info);
    }));
}

/// The newest unseen crash note in `dir`, marked seen (renamed `.seen`) so it
/// is reported once. Returns its file name.
pub fn take_unseen_crash(dir: &std::path::Path) -> Option<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        .collect();
    found.sort();
    let name = found.pop()?;
    let _ = std::fs::rename(dir.join(&name), dir.join(format!("{name}.seen")));
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allow_listed_values_survive() {
        let cfg = serde_json::json!({
            "age_confirmed": true,
            "panic_video": "/home/secret-name/video.mp4",
            "language": "klingon-SECRET",
            "indexer_key": "KEY-SECRET",
            "future_field": "SECRET"
        });
        let r = render(&Input {
            app_version: "1.0.0".into(),
            config: Some(cfg),
            ..Input::default()
        });
        assert!(!r.contains("SECRET") && !r.contains("secret"));
        assert!(r.contains("config.age_confirmed: true"));
        assert!(r.contains("config.language: other"));
    }

    #[test]
    fn crash_text_hides_payload_unless_debug() {
        let f = |d| {
            crash_text(
                "jizzcraft",
                "1.0.0",
                "main",
                Some(("/home/bob/.cargo/registry/x/src/foo.rs", 7)),
                Some("no such file /home/bob/private.mp4"),
                d,
            )
        };
        assert!(!f(false).contains("private") && !f(false).contains("bob"));
        assert!(f(false).contains("at: src/foo.rs:7"));
        assert!(f(true).contains("private.mp4"));
    }

    #[test]
    fn clean_strips_everything_odd() {
        assert_eq!(clean("1.26.1 /home/x"), "1.26.1__home_x");
    }
}
