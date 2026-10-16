//! Extractor rules: patterns the user writes for pages the generic reader
//! does not understand. Declarative JSON, like a streaming source. The
//! application ships none, names no site and has no preset.
//!
//! A rule never causes a request. It is matched against the page text that
//! was already fetched (at most 2 MiB), and what it finds goes through the
//! same resolving and `inward_ok` checks as any other address in a page.
//! Patterns run on the `regex` crate (no backtracking, so no pattern can
//! stall the reader) with small size limits.

use super::scripts::{js_unescape, parse_height, percent_decode, proto_of};
use super::{MAX_CANDIDATES, MAX_URL, Origin, Proto, RawCandidate, html};
use crate::formats::sanitize_title;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Largest rules file, in bytes.
pub const MAX_FILE: usize = 64 * 1024;
pub const MAX_RULES: usize = 32;
pub const MAX_MEDIA: usize = 8;
pub const MAX_JSON: usize = 4;
pub const MAX_HOSTS: usize = 16;
pub const MAX_PATTERN: usize = 1024;
pub const MAX_POINTER: usize = 512;
/// Most matches taken from one pattern.
pub const MAX_MATCHES: usize = 64;
/// Most page text a rule reads.
pub const MAX_INPUT: usize = 2 << 20;
/// Largest JSON capture parsed.
pub const MAX_CAPTURE: usize = 1 << 20;
const SIZE_LIMIT: usize = 256 * 1024;
const DFA_LIMIT: usize = 1 << 20;
const NEST_LIMIT: u32 = 32;
const MAX_NAME: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unescape {
    #[default]
    None,
    Js,
    Html,
    Url,
}

/// `(?P<url>...)` is the address; the optional fields fill in what the page
/// does not say.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPattern {
    pub regex: String,
    #[serde(default)]
    pub height: Option<u32>,
    /// `hls`, `dash` or `http`; absent: from the address.
    #[serde(default)]
    pub protocol: Option<String>,
}

/// `(?P<json>...)` captures a JSON document; pointers (RFC 6901) read it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonPattern {
    pub regex: String,
    /// Array of items; empty means the capture itself.
    #[serde(default)]
    pub items: String,
    /// Pointer to the address, inside an item.
    pub url: String,
    #[serde(default)]
    pub height: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractRule {
    #[serde(default)]
    pub id: u32,
    pub name: String,
    /// `videos.example.invalid`, `*.example.invalid`.
    pub hosts: Vec<String>,
    #[serde(default)]
    pub media: Vec<MediaPattern>,
    #[serde(default)]
    pub json: Vec<JsonPattern>,
    /// A pattern with `(?P<title>...)`.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub unescape: Unescape,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Patterns are the user's business: they are never printed.
impl std::fmt::Debug for ExtractRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractRule")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

fn build(pattern: &str, group: &str) -> Result<Regex, &'static str> {
    if pattern.len() > MAX_PATTERN {
        return Err("a pattern is too long");
    }
    let re = RegexBuilder::new(pattern)
        .size_limit(SIZE_LIMIT)
        .dfa_size_limit(DFA_LIMIT)
        .nest_limit(NEST_LIMIT)
        .build()
        .map_err(|_| "a pattern is not valid")?;
    if !re.capture_names().flatten().any(|n| n == group) {
        return Err("a pattern lacks its named group");
    }
    Ok(re)
}

fn host_ok(h: &str) -> bool {
    let valid = |s: &str| {
        !s.is_empty()
            && s.len() <= 253
            && s.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
    };
    match h.strip_prefix("*.") {
        Some(suffix) => suffix.contains('.') && valid(suffix),
        None => valid(h),
    }
}

fn pointer_ok(p: &str, required: bool) -> bool {
    p.len() <= MAX_POINTER && (p.is_empty() && !required || p.starts_with('/'))
}

/// Check a rule. Reasons are fixed phrases that never echo the input.
pub fn validate(r: &ExtractRule) -> Result<(), &'static str> {
    if r.name.trim().is_empty() || r.name.len() > MAX_NAME || r.name.chars().any(char::is_control) {
        return Err("a rule needs a plain name");
    }
    if r.hosts.is_empty() || r.hosts.len() > MAX_HOSTS || !r.hosts.iter().all(|h| host_ok(h)) {
        return Err("the hosts are not valid");
    }
    if r.media.is_empty() && r.json.is_empty() {
        return Err("a rule needs a pattern");
    }
    if r.media.len() > MAX_MEDIA || r.json.len() > MAX_JSON {
        return Err("a rule has too many patterns");
    }
    for m in &r.media {
        build(&m.regex, "url")?;
        if m.height.is_some_and(|h| !(100..=4320).contains(&h)) {
            return Err("a height is not valid");
        }
        if m.protocol
            .as_deref()
            .is_some_and(|p| !matches!(p, "hls" | "dash" | "http"))
        {
            return Err("a protocol is not valid");
        }
    }
    for j in &r.json {
        build(&j.regex, "json")?;
        if !pointer_ok(&j.items, false)
            || !pointer_ok(&j.url, true)
            || j.height.as_deref().is_some_and(|h| !pointer_ok(h, true))
        {
            return Err("a pointer is not valid");
        }
    }
    if let Some(t) = &r.title {
        build(t, "title")?;
    }
    Ok(())
}

/// Read a rules file: one rule or an array of rules, at most 64 KiB. Ids are
/// ignored (the host numbers them).
pub fn parse_import(bytes: &[u8]) -> Result<Vec<ExtractRule>, &'static str> {
    if bytes.len() > MAX_FILE {
        return Err("the file is too large");
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "not valid JSON")?;
    let list = match v {
        Value::Array(a) => a,
        o @ Value::Object(_) => vec![o],
        _ => return Err("not a rule"),
    };
    if list.is_empty() || list.len() > MAX_RULES {
        return Err("the number of rules is not valid");
    }
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let mut r: ExtractRule = serde_json::from_value(item).map_err(|_| "not a rule")?;
        r.id = 0;
        validate(&r)?;
        out.push(r);
    }
    Ok(out)
}

/// Does the rule apply to this host? Exact name, or `*.suffix` for any
/// subdomain of it (not the bare suffix).
pub fn applies(r: &ExtractRule, host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    r.enabled
        && r.hosts.iter().any(|p| match p.strip_prefix("*.") {
            Some(suffix) => h.len() > suffix.len() + 1 && h.ends_with(&format!(".{suffix}")),
            None => *p == h,
        })
}

fn unescape(mode: Unescape, s: &str) -> String {
    match mode {
        Unescape::None => s.to_owned(),
        Unescape::Js => js_unescape(s),
        Unescape::Html => html::decode(s),
        Unescape::Url => percent_decode(s),
    }
}

fn clean_url(s: String) -> Option<String> {
    let s = s.trim().to_owned();
    (!s.is_empty() && s.len() <= MAX_URL && !s.chars().any(char::is_control)).then_some(s)
}

fn proto_named(p: Option<&str>, url: &str) -> Proto {
    match p {
        Some("hls") => Proto::Hls,
        Some("dash") => Proto::Dash,
        Some("http") => Proto::Http,
        _ => proto_of(url),
    }
}

fn cand(url: String, height: Option<u32>, proto: Proto) -> RawCandidate {
    RawCandidate {
        url,
        height,
        bitrate: None,
        proto,
        origin: Origin::Rule,
    }
}

fn json_height(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|h| u32::try_from(h).ok()),
        Value::String(s) => parse_height(s),
        _ => None,
    }
    .filter(|h| (100..=4320).contains(h))
}

/// Run a rule over the page text. Returns the addresses found (unresolved,
/// `Origin::Rule`) and the title, if the rule has a pattern for it.
pub fn apply(r: &ExtractRule, body: &str) -> (Vec<RawCandidate>, Option<String>) {
    let mut end = body.len().min(MAX_INPUT);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let text = &body[..end];
    let mut out: Vec<RawCandidate> = Vec::new();
    for m in &r.media {
        let Ok(re) = build(&m.regex, "url") else {
            continue;
        };
        for caps in re.captures_iter(text).take(MAX_MATCHES) {
            let Some(u) = caps
                .name("url")
                .and_then(|c| clean_url(unescape(r.unescape, c.as_str())))
            else {
                continue;
            };
            let p = proto_named(m.protocol.as_deref(), &u);
            out.push(cand(u, m.height, p));
        }
    }
    for j in &r.json {
        let Ok(re) = build(&j.regex, "json") else {
            continue;
        };
        for caps in re.captures_iter(text).take(MAX_MATCHES) {
            let Some(c) = caps.name("json").filter(|c| c.len() <= MAX_CAPTURE) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<Value>(&unescape(r.unescape, c.as_str())) else {
                continue;
            };
            let Some(root) = (if j.items.is_empty() {
                Some(&doc)
            } else {
                doc.pointer(&j.items)
            }) else {
                continue;
            };
            let list: Vec<&Value> = match root {
                Value::Array(a) => a.iter().take(MAX_MATCHES).collect(),
                other => vec![other],
            };
            for item in list {
                let Some(u) = item
                    .pointer(&j.url)
                    .and_then(Value::as_str)
                    .and_then(|s| clean_url(s.to_owned()))
                else {
                    continue;
                };
                let h = j
                    .height
                    .as_deref()
                    .and_then(|p| item.pointer(p))
                    .and_then(json_height);
                let p = proto_of(&u);
                out.push(cand(u, h, p));
            }
        }
    }
    out.truncate(MAX_CANDIDATES);
    let title = r
        .title
        .as_deref()
        .and_then(|t| build(t, "title").ok())
        .and_then(|re| {
            re.captures(text)
                .and_then(|c| c.name("title").map(|m| unescape(r.unescape, m.as_str())))
        })
        .and_then(|t| sanitize_title(&t));
    (out, title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(json: &str) -> ExtractRule {
        parse_import(json.as_bytes()).unwrap().remove(0)
    }

    const GOOD: &str = r#"{"name":"t","hosts":["videos.example.invalid","*.cdn.example.invalid"],
        "media":[{"regex":"data-file=\"(?P<url>[^\"]+)\"","height":720}]}"#;

    #[test]
    fn validation_limits() {
        assert!(parse_import(GOOD.as_bytes()).is_ok());
        let with = |extra: &str| format!(r#"{{"name":"t","hosts":["a.example.invalid"],{extra}}}"#);
        let media = |re: &str| with(&format!(r#""media":[{{"regex":"{re}"}}]"#));
        assert!(parse_import(media("(?P<url>x)").as_bytes()).is_ok());
        for bad in [
            media("(?P<url>"),                                // not a regex
            media("no group"),                                // missing named group
            media(&format!("(?P<url>{})", "a".repeat(1100))), // too long
            media("(?P<url>a{1000}{1000}{1000})"),            // too big to compile
            with(r#""media":[]"#),
            with(r#""media":[{"regex":"(?P<url>x)","height":5}]"#),
            with(r#""media":[{"regex":"(?P<url>x)","protocol":"rtmp"}]"#),
            with(r#""json":[{"regex":"(?P<url>x)","url":"/u"}]"#), // json needs `json`
            with(r#""json":[{"regex":"(?P<json>x)","url":"u"}]"#), // pointer
            with(r#""media":[{"regex":"(?P<url>x)","extra":1}]"#),
            "[]".to_string(),
            "7".to_string(),
            "not json".to_string(),
        ] {
            assert!(parse_import(bad.as_bytes()).is_err(), "{bad}");
        }
        // Counts.
        let many: Vec<String> = (0..9)
            .map(|_| r#"{"regex":"(?P<url>x)"}"#.to_string())
            .collect();
        assert!(
            parse_import(with(&format!(r#""media":[{}]"#, many.join(","))).as_bytes()).is_err()
        );
        let rules: Vec<String> = (0..33).map(|_| GOOD.to_string()).collect();
        assert!(parse_import(format!("[{}]", rules.join(",")).as_bytes()).is_err());
        let rules: Vec<String> = (0..32).map(|_| GOOD.to_string()).collect();
        assert_eq!(
            parse_import(format!("[{}]", rules.join(",")).as_bytes())
                .unwrap()
                .len(),
            32
        );
        assert!(parse_import(&vec![b' '; MAX_FILE + 1]).is_err());
        // Ids from a file are ignored.
        assert_eq!(rule(&GOOD.replace("{\"name\"", "{\"id\":9,\"name\"")).id, 0);
    }

    #[test]
    fn host_patterns() {
        let r = rule(GOOD);
        assert!(applies(&r, "videos.example.invalid"));
        assert!(applies(&r, "VIDEOS.Example.Invalid."));
        assert!(applies(&r, "a.cdn.example.invalid"));
        assert!(applies(&r, "a.b.cdn.example.invalid"));
        assert!(!applies(&r, "cdn.example.invalid"));
        assert!(!applies(&r, "xcdn.example.invalid"));
        assert!(!applies(&r, "other.example.invalid"));
        assert!(!applies(&r, "videos.example.invalid.evil.invalid"));
        let mut off = r.clone();
        off.enabled = false;
        assert!(!applies(&off, "videos.example.invalid"));
        for bad in [
            r#""*.invalid""#,
            r#""*""#,
            r#""Upper.example.invalid""#,
            r#""a b.invalid""#,
            r#""a..invalid""#,
            r#""http://x.invalid""#,
            r#""a.invalid/path""#,
            r#""""#,
        ] {
            let j = format!(r#"{{"name":"t","hosts":[{bad}],"media":[{{"regex":"(?P<url>x)"}}]}}"#);
            assert!(parse_import(j.as_bytes()).is_err(), "{bad}");
        }
        let sixteen = (0..17)
            .map(|i| format!("\"h{i}.example.invalid\""))
            .collect::<Vec<_>>();
        let j = format!(
            r#"{{"name":"t","hosts":[{}],"media":[{{"regex":"(?P<url>x)"}}]}}"#,
            sixteen.join(",")
        );
        assert!(parse_import(j.as_bytes()).is_err());
    }

    #[test]
    fn media_and_json_rules_extract() {
        let r = rule(
            r#"{"name":"t","hosts":["a.example.invalid"],"unescape":"js",
            "title":"<h1 class=\"t\">(?P<title>[^<]+)</h1>",
            "media":[{"regex":"data-file=\"(?P<url>[^\"]+)\"","height":720},
                     {"regex":"hls\\('(?P<url>[^']+)'\\)","protocol":"hls"}],
            "json":[{"regex":"(?s)var cfg = (?P<json>\\{.*?\\});","items":"/sources","url":"/file","height":"/label"}]}"#,
        );
        let page = r#"<h1 class="t">Clip &amp; Co</h1>
            <div data-file="https:\/\/c.invalid\/v.mp4"></div>
            <script>hls('https://c.invalid/stream')</script>
            <script>var cfg = {"sources":[{"file":"https://c.invalid/hi","label":"1080p"},
              {"file":"https://c.invalid/lo.mp4","label":480},{"nofile":1}]};</script>"#;
        let (c, title) = apply(&r, page);
        let urls: Vec<_> = c
            .iter()
            .map(|x| (x.url.as_str(), x.height, x.proto))
            .collect();
        assert_eq!(
            urls,
            [
                ("https://c.invalid/v.mp4", Some(720), Proto::Http),
                ("https://c.invalid/stream", None, Proto::Hls),
                ("https://c.invalid/hi", Some(1080), Proto::Http),
                ("https://c.invalid/lo.mp4", Some(480), Proto::Http),
            ]
        );
        assert!(c.iter().all(|x| x.origin == Origin::Rule));
        assert_eq!(title.as_deref(), Some("Clip &amp; Co"));
        // Other unescaping.
        let mut h = r.clone();
        h.unescape = Unescape::Html;
        let (_, t) = apply(&h, page);
        assert_eq!(t.as_deref(), Some("Clip & Co"));
        // Broken JSON in a capture is skipped, not fatal.
        let (c, _) = apply(&r, "var cfg = {not json};");
        assert!(c.is_empty());
    }

    #[test]
    fn matches_are_capped() {
        let r = rule(
            r#"{"name":"t","hosts":["a.example.invalid"],"media":[{"regex":"u=(?P<url>[a-z0-9:/.]+);"}]}"#,
        );
        let page = "u=https://c.invalid/a.mp4;".repeat(500);
        let (c, _) = apply(&r, &page);
        assert_eq!(c.len(), MAX_MATCHES);
    }

    #[test]
    fn debug_hides_patterns() {
        let r = rule(
            r#"{"name":"secretname","hosts":["a.example.invalid"],"media":[{"regex":"(?P<url>PATTERNTEXT)"}]}"#,
        );
        let d = format!("{r:?}");
        assert!(!d.contains("PATTERNTEXT") && !d.contains("secretname") && !d.contains("example"));
        assert!(d.contains("enabled"));
    }
}
