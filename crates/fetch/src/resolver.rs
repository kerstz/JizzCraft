//! External resolvers: programs the user installed and configured.
//!
//! The model is mpv with yt-dlp: the application ships none and names none.
//! A resolver turns a page address into media addresses. It is started with
//! an argument vector only (no shell), an emptied environment, a timeout and
//! a capped stdout. It never delivers bytes: the address it returns is
//! fetched by this crate, with the proxy and limits that apply to any
//! request.

use crate::formats::is_http;
pub use crate::formats::{Fault, Format, Meta, muxed, parse, parse_formats, sanitize_title};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_OUTPUT: usize = 1 << 20;
/// Most of the resolver's stderr that is looked at. It is matched against
/// generic phrases and dropped; it is never stored, shown or logged.
pub const MAX_STDERR: usize = 64 * 1024;
/// Placeholder replaced by the address, as one whole argument element.
pub const URL_ARG: &str = "{url}";
/// Optional placeholder, as one whole argument that follows a flag
/// (`--proxy {proxy}`). Replaced by the `socks5h://` proxy in force; with no
/// proxy the flag and the placeholder are both left out.
pub const PROXY_ARG: &str = "{proxy}";
/// Internal error keyword: the source cannot be played without saving it.
pub const NEEDS_SAVING: &str = "cannot stream without saving";

/// Match generic lowercase phrases. The caller drops `stderr` afterwards.
pub fn classify_stderr(stderr: &[u8]) -> Fault {
    let t = String::from_utf8_lossy(&stderr[..stderr.len().min(MAX_STDERR)]).to_ascii_lowercase();
    let word = |w: &str| {
        t.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|x| x == w)
    };
    let has = |p: &str| t.contains(p);
    if word("drm") || has("protected by") || word("protected") {
        Fault::Drm
    } else if has("confirm your age")
        || has("age-restricted")
        || has("age restricted")
        || has("age verification")
        || has("verify your age")
    {
        Fault::AgeWall
    } else if has("sign in") || has("log in") || word("login") || has("logged in") {
        Fault::Login
    } else if has("not available in your country")
        || has("in your region")
        || has("geo restriction")
        || has("geo-restrict")
        || has("geo-block")
        || has("geo block")
        || word("geo")
    {
        Fault::Geo
    } else if word("429") || has("too many requests") {
        Fault::RateLimit
    } else if has("unsupported url") {
        Fault::Unsupported
    } else if has("unavailable")
        || has("removed")
        || has("private video")
        || has("video is private")
    {
        Fault::Gone
    } else {
        Fault::Other
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResolverDef {
    #[serde(default)]
    pub id: u32,
    pub name: String,
    /// `["/abs/path/tool", "-J", "--", "{url}"]`. First element is the program.
    pub argv: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The program honours `ALL_PROXY` (or you wrapped it so it does). Only
    /// then may it run while the "block without proxy" switch is on: this
    /// application cannot enforce a proxy on another program.
    #[serde(default)]
    pub honours_proxy: bool,
}

/// Arguments can carry a token (`--api-key=...`): Debug shows none of them.
impl std::fmt::Debug for ResolverDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolverDef")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .field("honours_proxy", &self.honours_proxy)
            .finish_non_exhaustive()
    }
}

impl ResolverDef {
    /// Honours the proxy: declared so, or its argument vector carries the
    /// `--proxy {proxy}` pair (this application then hands it the proxy).
    pub fn proxy_aware(&self) -> bool {
        self.honours_proxy || self.argv.iter().skip(1).any(|a| a == PROXY_ARG)
    }
}

fn default_timeout() -> u32 {
    30
}
fn yes() -> bool {
    true
}

pub fn validate(d: &ResolverDef) -> Result<(), &'static str> {
    let Some(prog) = d.argv.first() else {
        return Err("a resolver needs a program");
    };
    if prog.is_empty() || prog.contains(URL_ARG) {
        return Err("the first element must be the program");
    }
    if d.argv.iter().filter(|a| a.as_str() == URL_ARG).count() != 1 {
        return Err("exactly one argument must be {url}");
    }
    if d.argv
        .iter()
        .skip(1)
        .any(|a| a.contains(URL_ARG) && a != URL_ARG)
    {
        return Err("{url} must be a whole argument");
    }
    if d.argv
        .iter()
        .skip(1)
        .any(|a| a.contains(PROXY_ARG) && a != PROXY_ARG)
    {
        return Err("{proxy} must be a whole argument");
    }
    let np = d.argv.iter().filter(|a| a.as_str() == PROXY_ARG).count();
    if np > 1 || prog.contains(PROXY_ARG) {
        return Err("{proxy} may appear once, after a flag");
    }
    if let Some(i) = d.argv.iter().position(|a| a == PROXY_ARG)
        && (i < 2 || !d.argv[i - 1].starts_with('-'))
    {
        return Err("{proxy} must follow a flag such as --proxy");
    }
    if !std::path::Path::new(prog).is_absolute() {
        return Err("the program must be an absolute path");
    }
    if d.timeout_secs == 0 || d.timeout_secs > 300 {
        return Err("timeout must be between 1 and 300 seconds");
    }
    if d.argv.len() > 64 || d.argv.iter().any(|a| a.len() > 4096 || a.contains('\0')) {
        return Err("the argument list is not valid");
    }
    program_not_writable_by_others(std::path::Path::new(prog))?;
    Ok(())
}

/// Refuse a program that anyone could replace: a world-writable file, or one
/// that lives in a world-writable directory (or under a world-writable,
/// non-sticky ancestor). Whatever the resolver is, it runs as the user.
/// A missing file is not judged here: starting it will fail on its own.
#[cfg(unix)]
pub(crate) fn program_not_writable_by_others(prog: &std::path::Path) -> Result<(), &'static str> {
    use std::os::unix::fs::PermissionsExt;
    const MSG: &str = "the program (or its folder) is writable by everyone";
    let Ok(real) = std::fs::canonicalize(prog) else {
        return Ok(());
    };
    let mode = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.permissions().mode());
    if mode(&real).is_ok_and(|m| m & 0o002 != 0) {
        return Err(MSG);
    }
    for (i, dir) in real.ancestors().skip(1).enumerate() {
        if let Ok(m) = mode(dir)
            && m & 0o002 != 0
            && (i == 0 || m & 0o1000 == 0)
        {
            return Err(MSG);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn program_not_writable_by_others(_: &std::path::Path) -> Result<(), &'static str> {
    Ok(())
}

/// Run the resolver on `page_url`. Blocking: call it from a worker thread.
pub fn run(d: &ResolverDef, page_url: &str) -> Result<Vec<Format>, String> {
    run_meta(d, page_url).map(|(f, _)| f)
}

/// Like [`run`], with what the page said about itself. Errors are fixed
/// keywords (see [`Fault::keyword`]); the resolver's own text never leaves
/// this function.
pub fn run_meta(d: &ResolverDef, page_url: &str) -> Result<(Vec<Format>, Meta), String> {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    validate(d).map_err(str::to_owned)?;
    if !is_http(page_url) {
        return Err("unsupported address".into());
    }
    let proxy = crate::service::current_proxy().map(|p| p.replacen("socks5://", "socks5h://", 1));
    let mut cmd = Command::new(&d.argv[0]);
    let mut i = 1;
    while i < d.argv.len() {
        let a = &d.argv[i];
        if d.argv.get(i + 1).is_some_and(|n| n == PROXY_ARG) {
            // The `--proxy {proxy}` pair: filled in, or dropped.
            if let Some(p) = &proxy {
                cmd.arg(a).arg(p);
            }
            i += 2;
            continue;
        }
        cmd.arg(if a == URL_ARG { page_url } else { a });
        i += 1;
    }
    cmd.env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Keep the executable findable and give it the user's proxy, nothing else.
    if let Some(p) = std::env::var_os("PATH") {
        cmd.env("PATH", p);
    }
    #[cfg(unix)]
    if let Some(h) = std::env::var_os("HOME") {
        cmd.env("HOME", h);
    }
    #[cfg(windows)]
    for k in ["SystemRoot", "TEMP", "TMP", "USERPROFILE"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    if let Some(p) = &proxy {
        // socks5h: the proxy resolves names too. Both spellings, because
        // tools disagree on which one they read.
        for k in [
            "ALL_PROXY",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "all_proxy",
            "https_proxy",
            "http_proxy",
        ] {
            cmd.env(k, p);
        }
    }
    let dir = tempfile::Builder::new()
        .prefix("resolver")
        .tempdir()
        .map_err(|e| e.to_string())?;
    cmd.current_dir(dir.path());
    // Own process group: a timeout takes the whole tree down with it.
    // Family plan, family billing.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Fault::Missing.keyword().to_string()
        } else {
            "could not start the resolver".to_string()
        }
    })?;
    let kill = |child: &mut std::process::Child| {
        #[cfg(unix)]
        // SAFETY: plain syscall on the process group this function created.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let Some(mut out) = child.stdout.take() else {
        kill(&mut child);
        return Err("resolver failed".into());
    };
    // Stderr is read (capped) so a chatty tool never blocks on a full pipe,
    // matched against generic phrases, then dropped.
    let (etx, erx) = channel();
    if let Some(mut err) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.by_ref().take(MAX_STDERR as u64).read_to_end(&mut buf);
            let _ = etx.send(buf);
            let _ = std::io::copy(&mut err, &mut std::io::sink());
        });
    }
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out
            .by_ref()
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let t0 = Instant::now();
    let limit = Duration::from_secs(u64::from(d.timeout_secs));
    let buf = match rx.recv_timeout(limit) {
        Ok(b) => b,
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            kill(&mut child);
            return Err("resolver timed out".into());
        }
    };
    // Output is closed: the program is done or about to be. Never wait on it
    // longer than what is left of the budget.
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() >= limit => {
                kill(&mut child);
                return Err("resolver timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                kill(&mut child);
                return Err("resolver failed".into());
            }
        }
    };
    // Leftover background children die with the group. Nobody stays for the
    // credits.
    #[cfg(unix)]
    // SAFETY: as above.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    if buf.len() > MAX_OUTPUT {
        return Err("resolver output too large".into());
    }
    if !status.success() {
        let fault = if status.code() == Some(127) {
            Fault::Missing
        } else {
            let e = erx
                .recv_timeout(Duration::from_millis(500))
                .unwrap_or_default();
            let f = classify_stderr(&e);
            drop(e);
            f
        };
        return Err(fault.keyword().into());
    }
    drop(erx);
    parse(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(argv: &[&str]) -> ResolverDef {
        ResolverDef {
            id: 1,
            name: "t".into(),
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
            timeout_secs: 5,
            enabled: true,
            honours_proxy: false,
        }
    }

    #[test]
    fn validation_wants_absolute_program_and_one_whole_url_arg() {
        assert!(validate(&def(&["/usr/bin/tool", "-J", "{url}"])).is_ok());
        assert!(validate(&def(&["tool", "{url}"])).is_err());
        assert!(validate(&def(&["/usr/bin/tool"])).is_err());
        assert!(validate(&def(&["/usr/bin/tool", "--x={url}"])).is_err());
        assert!(validate(&def(&["/usr/bin/tool", "{url}", "{url}"])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn writable_programs_and_folders_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let set = |p: &std::path::Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        };
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("bin");
        std::fs::create_dir(&dir).unwrap();
        set(&dir, 0o755);
        let prog = dir.join("tool");
        std::fs::write(&prog, "#!/bin/sh\n").unwrap();
        set(&prog, 0o755);
        let argv = [prog.to_str().unwrap(), "{url}"];
        assert!(validate(&def(&argv)).is_ok());
        set(&prog, 0o757);
        assert!(validate(&def(&argv)).is_err(), "world-writable file");
        set(&prog, 0o755);
        set(&dir, 0o777);
        assert!(validate(&def(&argv)).is_err(), "world-writable folder");
        set(&dir, 0o1777);
        assert!(validate(&def(&argv)).is_err(), "sticky is still refused");
        set(&dir, 0o755);
        // A symlink to a writable program is judged by its target.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&prog, &link).unwrap();
        set(&prog, 0o666);
        assert!(validate(&def(&[link.to_str().unwrap(), "{url}"])).is_err());
        assert!(validate(&def(&["/usr/bin/tool\0x", "{url}"])).is_err());
    }

    #[test]
    fn formats_are_filtered_and_sorted() {
        let f = parse_formats(
            br#"{"formats":[{"url":"https://a.invalid/lo.mp4","height":360},
                {"url":"file:///etc/passwd","height":9999},
                {"url":"https://a.invalid/hi.mp4","height":1080}]}"#,
        )
        .unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].height, Some(1080));
        assert!(parse_formats(b"{}").is_err());
    }

    #[test]
    fn protocols_headers_and_tracks() {
        let f = parse_formats(
            br#"{"formats":[
            {"url":"https://a.invalid/m.m3u8","protocol":"m3u8_native","height":720,
             "http_headers":{"User-Agent":"UA","Cookie":"x=1","Referer":"https://r.invalid/","X-Evil":"1"}},
            {"url":"https://a.invalid/d.mpd","protocol":"http_dash_segments","height":480},
            {"url":"https://a.invalid/v.mp4","vcodec":"avc1","acodec":"none","height":1080},
            {"url":"https://a.invalid/a.m4a","vcodec":"none","acodec":"mp4a"},
            {"url":"https://a.invalid/sb.mhtml","vcodec":"none","acodec":"none"}]}"#,
        )
        .unwrap();
        assert_eq!(f.len(), 4);
        let hls = f.iter().find(|x| x.protocol == "hls").unwrap();
        assert_eq!(hls.headers.len(), 2);
        assert!(f.iter().any(|x| x.protocol == "dash"));
        assert!(f.iter().any(|x| x.video_only && !x.audio_only));
        assert!(f.iter().any(|x| x.audio_only));
        let m = muxed(f).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].protocol, "hls");
        let only = parse_formats(
            br#"{"formats":[{"url":"https://a.invalid/v.mp4","vcodec":"avc1","acodec":"none"}]}"#,
        )
        .unwrap();
        assert_eq!(muxed(only).unwrap_err(), "unsupported stream");
    }

    #[test]
    fn stderr_phrases_become_faults() {
        let cases: [(&str, Fault); 12] = [
            (
                "ERROR: Unsupported URL: https://x.invalid/a",
                Fault::Unsupported,
            ),
            ("Sign in to confirm your age", Fault::AgeWall),
            ("this video is age-restricted", Fault::AgeWall),
            ("please log in to view", Fault::Login),
            ("Video not available in your country", Fault::Geo),
            ("This video is DRM protected", Fault::Drm),
            ("HTTP Error 429: Too Many Requests", Fault::RateLimit),
            ("Video unavailable", Fault::Gone),
            ("This is a private video", Fault::Gone),
            ("segfault", Fault::Other),
            ("", Fault::Other),
            ("address admin dream", Fault::Other),
        ];
        for (t, want) in cases {
            assert_eq!(classify_stderr(t.as_bytes()), want, "{t}");
        }
        assert!(Fault::Drm.rank() > Fault::AgeWall.rank());
        assert!(Fault::Gone.rank() > Fault::Other.rank());
        for f in [Fault::Missing, Fault::Drm, Fault::Other] {
            assert_eq!(Fault::from_keyword(f.keyword()), Some(f));
        }
    }

    #[test]
    fn meta_is_sanitized_and_not_debuggable() {
        let long = "x".repeat(300);
        let doc = serde_json::json!({"url":"https://a.invalid/v.mp4",
            "title": format!("a\u{0007}b\u{202e}c\n{long}"),
            "duration": 61.5, "thumbnail": "javascript:alert(1)"});
        let (_, m) = parse(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let t = m.title.clone().unwrap();
        assert!(t.starts_with("abcx") && t.chars().count() == 120);
        assert_eq!(m.duration_secs, Some(61));
        assert_eq!(m.thumbnail, None);
        assert!(!format!("{m:?}").contains("abcx"));
    }

    #[test]
    fn proxy_placeholder_rules() {
        assert!(validate(&def(&["/usr/bin/tool", "--proxy", "{proxy}", "{url}"])).is_ok());
        assert!(validate(&def(&["/usr/bin/tool", "--proxy={proxy}", "{url}"])).is_err());
        assert!(validate(&def(&["/usr/bin/tool", "{proxy}", "{url}"])).is_err());
        assert!(
            validate(&def(&[
                "/usr/bin/tool",
                "--proxy",
                "{proxy}",
                "--p",
                "{proxy}",
                "{url}"
            ]))
            .is_err()
        );
        assert!(def(&["/usr/bin/tool", "--proxy", "{proxy}", "{url}"]).proxy_aware());
        assert!(!def(&["/usr/bin/tool", "{url}"]).proxy_aware());
    }
}
