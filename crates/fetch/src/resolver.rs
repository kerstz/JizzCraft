//! External resolvers: programs the user installed and configured.
//!
//! The model is mpv with yt-dlp: the application ships none and names none.
//! A resolver turns a page address into media addresses. It is started with
//! an argument vector only (no shell), an emptied environment, a timeout and
//! a capped stdout. It never delivers bytes: the address it returns is
//! fetched by this crate, with the proxy and limits that apply to any
//! request.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_OUTPUT: usize = 1 << 20;
/// Placeholder replaced by the address, as one whole argument element.
pub const URL_ARG: &str = "{url}";

#[derive(Clone, Debug, Serialize, Deserialize)]
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

fn default_timeout() -> u32 {
    30
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub url: String,
    pub height: Option<u32>,
    /// `http`, `https`, `hls` or `dash`.
    pub protocol: String,
    /// Only `User-Agent`, `Referer` and `Origin` are kept.
    pub headers: Vec<(String, String)>,
    pub audio_only: bool,
    pub video_only: bool,
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
    if !std::path::Path::new(prog).is_absolute() {
        return Err("the program must be an absolute path");
    }
    if d.timeout_secs == 0 || d.timeout_secs > 300 {
        return Err("timeout must be between 1 and 300 seconds");
    }
    Ok(())
}

fn is_http(u: &str) -> bool {
    u.starts_with("https://") || u.starts_with("http://")
}

/// Run the resolver on `page_url`. Blocking: call it from a worker thread.
pub fn run(d: &ResolverDef, page_url: &str) -> Result<Vec<Format>, String> {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    validate(d).map_err(str::to_owned)?;
    if !is_http(page_url) {
        return Err("unsupported address".into());
    }
    let mut cmd = Command::new(&d.argv[0]);
    for a in &d.argv[1..] {
        cmd.arg(if a == URL_ARG { page_url } else { a });
    }
    cmd.env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    if let Some(p) = crate::service::current_proxy() {
        // socks5h: the proxy resolves names too. Both spellings, because
        // tools disagree on which one they read.
        let p = p.replacen("socks5://", "socks5h://", 1);
        for k in [
            "ALL_PROXY",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "all_proxy",
            "https_proxy",
            "http_proxy",
        ] {
            cmd.env(k, &p);
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
    let mut child = cmd
        .spawn()
        .map_err(|_| "could not start the resolver".to_string())?;
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
        return Err("resolver failed".into());
    }
    parse_formats(&buf)
}

/// `{formats:[{url, protocol?, height?}]}` or a single top-level `{url}`.
pub fn parse_formats(json: &[u8]) -> Result<Vec<Format>, String> {
    let v: Value = serde_json::from_slice(json).map_err(|_| "resolver did not answer JSON")?;
    let mut list: Vec<&Value> = Vec::new();
    if let Some(a) = v.get("formats").and_then(Value::as_array) {
        list.extend(a.iter());
    } else if v.get("url").is_some() {
        list.push(&v);
    }
    let mut out: Vec<Format> = list
        .into_iter()
        .filter_map(|f| {
            let url = f.get("url")?.as_str()?.to_owned();
            if !is_http(&url) {
                return None;
            }
            let codec = |k: &str| {
                f.get(k)
                    .and_then(Value::as_str)
                    .map(|c| c.eq_ignore_ascii_case("none"))
            };
            let (v_none, a_none) = (codec("vcodec") == Some(true), codec("acodec") == Some(true));
            // Neither picture nor sound: storyboards and the like.
            if v_none && a_none {
                return None;
            }
            let raw = f.get("protocol").and_then(Value::as_str).unwrap_or("http");
            let protocol = if raw.starts_with("m3u8") {
                "hls"
            } else if raw == "http_dash_segments" || raw == "dash" {
                "dash"
            } else {
                raw
            }
            .to_owned();
            let headers = f
                .get("http_headers")
                .and_then(Value::as_object)
                .map(|o| {
                    crate::segmented::allowed_headers(
                        o.iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                            .collect(),
                    )
                })
                .unwrap_or_default();
            Some(Format {
                url,
                height: f.get("height").and_then(Value::as_u64).map(|h| h as u32),
                protocol,
                headers,
                audio_only: v_none,
                video_only: a_none,
            })
        })
        .collect();
    if out.is_empty() {
        return Err("resolver returned no address".into());
    }
    // Best first: highest picture within what the app can stream today.
    out.sort_by_key(|f| std::cmp::Reverse(f.height.unwrap_or(0)));
    Ok(out)
}

/// Muxed formats only, best first. Separate audio and video tracks are not
/// joined here: with nothing muxed the answer is `unsupported stream`.
pub fn muxed(formats: Vec<Format>) -> Result<Vec<Format>, String> {
    let m: Vec<Format> = formats
        .into_iter()
        .filter(|f| !f.audio_only && !f.video_only)
        .collect();
    if m.is_empty() {
        return Err("unsupported stream".into());
    }
    Ok(m)
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
}
