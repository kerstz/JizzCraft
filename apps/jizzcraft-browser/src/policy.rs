//! Pure policy functions: no WebKit, no I/O beyond reading a directory name.

// Parts are used only by the engine build.
#![cfg_attr(not(feature = "engine"), allow(dead_code))]

use std::path::{Path, PathBuf};

use jizzcraft_bridge::ToHelper;

/// Started without the app (TTY, no Hello in time, wrong first message).
pub const EXIT_NOT_FROM_APP: i32 = 2;
/// Built without the `engine` feature.
#[cfg_attr(feature = "engine", allow(dead_code))]
pub const EXIT_ENGINE_MISSING: i32 = 3;
/// `require_proxy` set and no proxy given.
pub const EXIT_PROXY_REQUIRED: i32 = 4;
/// Hello failed validation.
pub const EXIT_BAD_HELLO: i32 = 5;

pub const HELLO_VERSION: u32 = 1;
pub const WINDOW_TITLE: &str = "Viewer";
pub const NOT_FROM_APP_TEXT: &str = "Start it from JizzCraft.";
pub const NO_SEARCH_PAGE: &str =
    "Type an address. No search engine is set; you can add your own in Settings.";
pub const DOWNLOAD_NOTICE: &str = "The browser does not download files.";

/// Validated session settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub proxy: Option<String>,
    pub js: bool,
    pub media: bool,
    pub remember: bool,
    pub discreet: bool,
    pub disguised: bool,
    pub rules_path: Option<PathBuf>,
    pub search_template: Option<String>,
    pub software: bool,
}

/// Why a Hello was refused. `code()` is the fixed string sent to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloError {
    NotHello,
    Version,
    Proxy,
    ProxyRequired,
    Rules,
    Template,
}

impl HelloError {
    pub fn code(self) -> &'static str {
        match self {
            HelloError::NotHello => "bad_hello",
            HelloError::Version => "bad_version",
            HelloError::Proxy => "bad_proxy",
            HelloError::ProxyRequired => "proxy_required",
            HelloError::Rules => "bad_rules_path",
            HelloError::Template => "bad_search_template",
        }
    }
    pub fn exit_code(self) -> i32 {
        match self {
            HelloError::ProxyRequired => EXIT_PROXY_REQUIRED,
            _ => EXIT_BAD_HELLO,
        }
    }
}

/// Only `socks5://host:port` goes to the engine, never on a command line.
pub fn proxy_ok(p: &str) -> bool {
    let Some(rest) = p.strip_prefix("socks5://") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 255
        && !rest.contains(['/', '?', '#', '@', ' '])
        && rest.chars().all(|c| !c.is_control())
}

pub fn validate_hello(msg: ToHelper) -> Result<Session, HelloError> {
    let ToHelper::Hello {
        v,
        proxy,
        require_proxy,
        js,
        media,
        remember,
        discreet,
        disguised,
        rules_path,
        search_template,
        software,
    } = msg
    else {
        return Err(HelloError::NotHello);
    };
    if v != HELLO_VERSION {
        return Err(HelloError::Version);
    }
    let proxy = proxy.filter(|p| !p.is_empty());
    if let Some(p) = &proxy {
        if !proxy_ok(p) {
            return Err(HelloError::Proxy);
        }
    } else if require_proxy {
        return Err(HelloError::ProxyRequired);
    }
    let rules_path = match rules_path.filter(|p| !p.is_empty()) {
        Some(p) => {
            let pb = PathBuf::from(p);
            if !pb.is_absolute() {
                return Err(HelloError::Rules);
            }
            Some(pb)
        }
        None => None,
    };
    let search_template = match search_template.filter(|t| !t.is_empty()) {
        Some(t) => {
            let ok = t.matches("{}").count() == 1
                && (t.starts_with("https://") || t.starts_with("http://"))
                && !t.chars().any(|c| c.is_control() || c.is_whitespace());
            if !ok {
                return Err(HelloError::Template);
            }
            Some(t)
        }
        None => None,
    };
    Ok(Session {
        proxy,
        js,
        media,
        remember,
        discreet,
        disguised,
        rules_path,
        search_template,
        software,
    })
}

/// Top-level navigation: http, https and the blank page only.
pub fn navigation_allowed(url: &str) -> bool {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if lower == "about:blank" {
        return true;
    }
    if u.chars().any(|c| c.is_control()) {
        return false;
    }
    match lower.split_once("://") {
        Some(("http" | "https", rest)) => !rest.is_empty() && !rest.starts_with('/'),
        _ => false,
    }
}

/// What the address entry should do with what was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Go(String),
    NoSearch,
    Refused,
}

fn encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else if b == b' ' {
            o.push('+');
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

pub fn resolve_address(typed: &str, template: Option<&str>) -> Address {
    let t = typed.trim();
    if t.is_empty() {
        return Address::Refused;
    }
    if t.contains("://") || t.to_ascii_lowercase().starts_with("about:") {
        return if navigation_allowed(t) {
            Address::Go(t.to_owned())
        } else {
            Address::Refused
        };
    }
    // A scheme-like prefix (javascript:, data:, file:) is never a search.
    if let Some((pre, _)) = t.split_once(':') {
        let l = pre.to_ascii_lowercase();
        if matches!(
            l.as_str(),
            "javascript" | "data" | "file" | "blob" | "view-source"
        ) {
            return Address::Refused;
        }
    }
    let hostlike = !t.contains(char::is_whitespace)
        && (t.contains('.') || t.starts_with("localhost"))
        && !t.starts_with('.')
        && !t.ends_with('.');
    if hostlike {
        let u = format!("https://{t}");
        return if navigation_allowed(&u) {
            Address::Go(u)
        } else {
            Address::Refused
        };
    }
    match template {
        Some(tpl) => Address::Go(tpl.replacen("{}", &encode(t), 1)),
        None => Address::NoSearch,
    }
}

/// Where the helper may write. `None` means memory only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub root: PathBuf,
    /// Wiped on exit and on next start.
    pub ephemeral: bool,
}

pub fn profile_dir(
    remember: bool,
    xdg_runtime: Option<&Path>,
    xdg_data_home: Option<&Path>,
    home: Option<&Path>,
    pid: u32,
) -> Option<Profile> {
    let abs = |p: Option<&Path>| p.filter(|p| p.is_absolute()).map(Path::to_path_buf);
    if remember {
        let data = abs(xdg_data_home).or_else(|| abs(home).map(|h| h.join(".local/share")))?;
        Some(Profile {
            root: data.join("jizzcraft").join("browser"),
            ephemeral: false,
        })
    } else {
        let rt = abs(xdg_runtime)?;
        Some(Profile {
            root: rt.join(format!("jizzcraft-browser-{pid}")),
            ephemeral: true,
        })
    }
}

/// Dead helpers' leftover folders: names `jizzcraft-browser-<pid>` whose pid is
/// not `own` and not alive.
pub fn stale_names(names: &[String], own: u32, alive: impl Fn(u32) -> bool) -> Vec<String> {
    names
        .iter()
        .filter(|n| {
            n.strip_prefix("jizzcraft-browser-")
                .and_then(|p| p.parse::<u32>().ok())
                .is_some_and(|pid| pid != own && !alive(pid))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ToHelper {
        ToHelper::Hello {
            v: 1,
            proxy: None,
            require_proxy: false,
            js: true,
            media: true,
            remember: false,
            discreet: false,
            disguised: false,
            rules_path: None,
            search_template: None,
            software: false,
        }
    }

    #[test]
    fn scheme_filter() {
        for ok in [
            "http://a.example/",
            "HTTPS://a.example/x?y#z",
            "about:blank",
        ] {
            assert!(navigation_allowed(ok), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "data:text/html,hi",
            "javascript:alert(1)",
            "blob:https://a/b",
            "ftp://a/",
            "about:config",
            "https:///x",
            "http://a\n.example",
            "",
        ] {
            assert!(!navigation_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn addresses() {
        assert_eq!(
            resolve_address("example.org", None),
            Address::Go("https://example.org".into())
        );
        assert_eq!(resolve_address("two words", None), Address::NoSearch);
        assert_eq!(
            resolve_address("javascript:alert(1)", None),
            Address::Refused
        );
        assert_eq!(resolve_address("file:///x", None), Address::Refused);
        assert_eq!(
            resolve_address("a b&c", Some("https://s.example/?q={}")),
            Address::Go("https://s.example/?q=a+b%26c".into())
        );
        assert_eq!(resolve_address("  ", None), Address::Refused);
    }

    #[test]
    fn hello_validation() {
        assert!(validate_hello(hello()).is_ok());
        assert_eq!(validate_hello(ToHelper::Close), Err(HelloError::NotHello));
        let mut h = hello();
        if let ToHelper::Hello { v, .. } = &mut h {
            *v = 2;
        }
        assert_eq!(validate_hello(h), Err(HelloError::Version));
        let mk = |proxy: Option<&str>, req: bool| {
            let mut h = hello();
            if let ToHelper::Hello {
                proxy: p,
                require_proxy,
                ..
            } = &mut h
            {
                *p = proxy.map(str::to_owned);
                *require_proxy = req;
            }
            validate_hello(h)
        };
        assert_eq!(mk(None, true), Err(HelloError::ProxyRequired));
        assert_eq!(HelloError::ProxyRequired.exit_code(), EXIT_PROXY_REQUIRED);
        assert_eq!(mk(Some("http://x:1"), false), Err(HelloError::Proxy));
        assert!(mk(Some("socks5://127.0.0.1:9050"), true).is_ok());
    }

    #[test]
    fn rules_and_template() {
        let mut h = hello();
        if let ToHelper::Hello {
            rules_path,
            search_template,
            ..
        } = &mut h
        {
            *rules_path = Some("rel/rules.json".into());
            *search_template = None;
        }
        assert_eq!(validate_hello(h), Err(HelloError::Rules));
        let mut h = hello();
        if let ToHelper::Hello {
            search_template, ..
        } = &mut h
        {
            *search_template = Some("file:///{}".into());
        }
        assert_eq!(validate_hello(h), Err(HelloError::Template));
    }

    #[test]
    fn profile_paths() {
        let rt = Path::new("/run/user/1000");
        let p = profile_dir(false, Some(rt), None, None, 42).unwrap();
        assert_eq!(p.root, Path::new("/run/user/1000/jizzcraft-browser-42"));
        assert!(p.ephemeral);
        assert_eq!(profile_dir(false, None, None, None, 42), None);
        assert_eq!(
            profile_dir(false, Some(Path::new("rel")), None, None, 1),
            None
        );
        let p = profile_dir(
            true,
            Some(rt),
            Some(Path::new("/d")),
            Some(Path::new("/h")),
            1,
        )
        .unwrap();
        assert_eq!(p.root, Path::new("/d/jizzcraft/browser"));
        assert!(!p.ephemeral);
        let p = profile_dir(true, None, None, Some(Path::new("/h")), 1).unwrap();
        assert_eq!(p.root, Path::new("/h/.local/share/jizzcraft/browser"));
    }

    #[test]
    fn stale_selection() {
        let names: Vec<String> = [
            "jizzcraft-browser-1",
            "jizzcraft-browser-2",
            "jizzcraft-browser-x",
            "other",
            "jizzcraft-browser-3",
        ]
        .map(String::from)
        .into();
        let got = stale_names(&names, 3, |pid| pid == 2);
        assert_eq!(got, vec!["jizzcraft-browser-1".to_string()]);
    }

    #[test]
    fn exit_codes_distinct() {
        let c = [
            EXIT_NOT_FROM_APP,
            EXIT_ENGINE_MISSING,
            EXIT_PROXY_REQUIRED,
            EXIT_BAD_HELLO,
        ];
        for (i, a) in c.iter().enumerate() {
            assert!(*a > 1);
            assert!(!c[i + 1..].contains(a));
        }
    }
}
