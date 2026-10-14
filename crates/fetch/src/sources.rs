//! User-defined streaming sources. Declarative, pure, no I/O.
//!
//! The application ships none: no host, no catalog, no preset. A source is a
//! JSON description the user writes (or imports) of an HTTP search endpoint
//! that answers JSON. Only three placeholders exist (`{query}`, `{page}`,
//! `{limit}`), always percent-encoded, and fields are read with RFC 6901
//! JSON pointers: no regex, no scripting.

use crate::secret::Secret;
use crate::torznab::pct_encode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// Source ids are offset in `SearchResult::indexer_id` so they never collide
/// with Torznab indexer ids.
pub const ID_BASE: u32 = 1_000_000;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Fields {
    pub title: String,
    /// Pointer to a direct media URL (played through `stream.open`).
    #[serde(default)]
    pub media: Option<String>,
    /// Pointer to a page URL (for a resolver).
    #[serde(default)]
    pub page_url: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SourceDef {
    #[serde(default)]
    pub id: u32,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `https://host/api?q={query}&page={page}`
    pub search_url: String,
    /// Pointer to the array of items; empty means the document itself.
    #[serde(default)]
    pub items: String,
    pub fields: Fields,
    /// Header that carries the secret, e.g. `Authorization`.
    #[serde(default)]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub auth_prefix: String,
    /// Only ever read from the 0600 config file, never from an import.
    #[serde(default)]
    pub secret: Secret,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub referer: Option<String>,
    /// Requests per minute (0 = unlimited).
    #[serde(default = "default_rate")]
    pub per_minute: u32,
}

fn yes() -> bool {
    true
}
fn default_rate() -> u32 {
    30
}

impl fmt::Debug for SourceDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceDef")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    pub media: Option<String>,
    pub page_url: Option<String>,
    pub size: Option<u64>,
}

fn is_http(u: &str) -> bool {
    u.starts_with("https://") || u.starts_with("http://")
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("");
    }
    auth.split(':').next().unwrap_or("")
}

/// Loopback, private and link-local literals, and `localhost`.
fn is_local_host(h: &str) -> bool {
    use std::net::IpAddr;
    let h = h.trim_end_matches('.').to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified()
        }
        Ok(IpAddr::V6(a)) => {
            a.is_loopback()
                || a.is_unspecified()
                || (a.segments()[0] & 0xfe00) == 0xfc00
                || (a.segments()[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

fn is_header_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_header_value(s: &str) -> bool {
    s.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
}

/// Check a definition. Returns a fixed reason, never an echo of the input.
pub fn validate(d: &SourceDef) -> Result<(), &'static str> {
    if d.name.trim().is_empty() {
        return Err("a source needs a name");
    }
    if !is_http(&d.search_url) {
        return Err("the search address must be http or https");
    }
    // Every `{...}` must be one of the three placeholders. A fourth would need
    // a committee.
    let mut rest = d.search_url.as_str();
    while let Some(i) = rest.find(['{', '}']) {
        if !rest[i..].starts_with('{') {
            return Err("unbalanced braces in the search address");
        }
        let Some(j) = rest[i..].find('}') else {
            return Err("unbalanced braces in the search address");
        };
        match &rest[i..=i + j] {
            "{query}" | "{page}" | "{limit}" => {}
            _ => return Err("unknown placeholder in the search address"),
        }
        rest = &rest[i + j + 1..];
    }
    if !d.search_url.contains("{query}") {
        return Err("the search address needs {query}");
    }
    let ptr_ok = |p: &str| p.is_empty() || p.starts_with('/');
    if !ptr_ok(&d.items)
        || !ptr_ok(&d.fields.title)
        || d.fields.title.is_empty()
        || d.fields.media.as_deref().is_some_and(|p| !ptr_ok(p))
        || d.fields.page_url.as_deref().is_some_and(|p| !ptr_ok(p))
        || d.fields.size.as_deref().is_some_and(|p| !ptr_ok(p))
    {
        return Err("fields must be JSON pointers like /data/0/name");
    }
    if d.fields.media.is_none() && d.fields.page_url.is_none() {
        return Err("map at least `media` or `page_url`");
    }
    if let Some(h) = &d.auth_header {
        if !is_header_name(h) || !is_header_value(&d.auth_prefix) {
            return Err("invalid authentication header");
        }
        // A key never travels in clear text.
        if !d.search_url.starts_with("https://") && !is_local_host(host_of(&d.search_url)) {
            return Err("a source with a key must use https");
        }
    }
    if d.user_agent.as_deref().is_some_and(|v| !is_header_value(v))
        || d.referer.as_deref().is_some_and(|v| !is_header_value(v))
    {
        return Err("invalid header value");
    }
    Ok(())
}

/// Parse a definition a user wrote. The secret is never taken from there.
pub fn parse_import(json: &[u8]) -> Result<SourceDef, &'static str> {
    let mut v: Value = serde_json::from_slice(json).map_err(|_| "not valid JSON")?;
    if let Some(o) = v.as_object_mut() {
        o.remove("secret");
        o.remove("id");
    }
    let d: SourceDef = serde_json::from_value(v).map_err(|_| "not a source definition")?;
    validate(&d)?;
    Ok(d)
}

pub fn build_url(d: &SourceDef, query: &str, page: u32, limit: u32) -> String {
    d.search_url
        .replace("{query}", &pct_encode(query))
        .replace("{page}", &page.to_string())
        .replace("{limit}", &limit.to_string())
}

fn pointer_str(v: &Value, ptr: &str) -> Option<String> {
    match v.pointer(ptr)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Items of a JSON answer, at most `max`. Items whose media or page address is
/// not http(s) lose that address; items with neither are dropped.
pub fn parse_items(d: &SourceDef, body: &[u8], max: usize) -> Result<Vec<Item>, &'static str> {
    let doc: Value = serde_json::from_slice(body).map_err(|_| "the source did not answer JSON")?;
    let root = if d.items.is_empty() {
        &doc
    } else {
        doc.pointer(&d.items).ok_or("no items in the answer")?
    };
    let arr = root.as_array().ok_or("items is not a list")?;
    // A public source cannot point the app at this machine or its network.
    // Strangers do not get to give directions to your own living room.
    let source_is_local = is_local_host(host_of(&d.search_url));
    let mut out = Vec::new();
    for it in arr.iter().take(max) {
        let Some(title) = pointer_str(it, &d.fields.title) else {
            continue;
        };
        let get = |p: &Option<String>| {
            p.as_deref()
                .and_then(|p| pointer_str(it, p))
                .filter(|u| is_http(u))
                .filter(|u| source_is_local || !is_local_host(host_of(u)))
        };
        let media = get(&d.fields.media);
        let page_url = get(&d.fields.page_url);
        if media.is_none() && page_url.is_none() {
            continue;
        }
        let size = d
            .fields
            .size
            .as_deref()
            .and_then(|p| pointer_str(it, p))
            .and_then(|s| s.parse().ok());
        out.push(Item {
            title,
            media,
            page_url,
            size,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> SourceDef {
        parse_import(
            br#"{"name":"mine","search_url":"https://api.example.invalid/v1/s?q={query}&p={page}",
                "items":"/data","fields":{"title":"/name","media":"/play","page_url":"/url","size":"/bytes"},
                "secret":"leak","id":99}"#,
        )
        .unwrap()
    }

    #[test]
    fn import_drops_secret_and_id() {
        let d = def();
        assert!(d.secret.is_empty());
        assert_eq!(d.id, 0);
    }

    #[test]
    fn url_is_encoded_and_placeholders_are_closed_set() {
        let d = def();
        assert_eq!(
            build_url(&d, "a b&c=d", 2, 50),
            "https://api.example.invalid/v1/s?q=a%20b%26c%3Dd&p=2"
        );
        let mut bad = d.clone();
        bad.search_url = "https://x.invalid/?q={query}&k={secret}".into();
        assert!(validate(&bad).is_err());
        bad.search_url = "file:///etc/passwd?{query}".into();
        assert!(validate(&bad).is_err());
        bad.search_url = "https://x.invalid/".into();
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn items_are_read_with_pointers_and_urls_are_filtered() {
        let d = def();
        let body = br#"{"data":[
            {"name":"One","play":"https://cdn.invalid/a.mp4","url":"https://p.invalid/1","bytes":1234},
            {"name":"Two","play":"javascript:alert(1)","url":"ftp://x"},
            {"name":"Three","play":"http://cdn.invalid/c.mp4"},
            {"play":"https://cdn.invalid/untitled.mp4"}
        ]}"#;
        let v = parse_items(&d, body, 10).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].size, Some(1234));
        assert_eq!(v[1].title, "Three");
        assert!(parse_items(&d, b"<html>", 10).is_err());
    }

    #[test]
    fn keys_need_https_and_public_sources_cannot_point_inward() {
        let mut d = def();
        d.search_url = "http://api.example.invalid/s?q={query}".into();
        d.auth_header = Some("X-Api-Key".into());
        assert!(validate(&d).is_err());
        d.search_url = "https://api.example.invalid/s?q={query}".into();
        assert!(validate(&d).is_ok());
        let body = br#"{"data":[
            {"name":"a","play":"http://127.0.0.1:9696/x.mp4"},
            {"name":"b","play":"http://192.168.1.1/x.mp4"},
            {"name":"c","play":"http://[::1]/x.mp4"},
            {"name":"d","play":"https://cdn.example.invalid/x.mp4"}]}"#;
        let v = parse_items(&d, body, 10).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "d");
    }

    #[test]
    fn header_injection_is_refused() {
        let mut d = def();
        d.auth_header = Some("Authorization\r\nX-Evil".into());
        assert!(validate(&d).is_err());
        d.auth_header = Some("Authorization".into());
        d.auth_prefix = "Bearer \r\nX: y".into();
        assert!(validate(&d).is_err());
    }
}
