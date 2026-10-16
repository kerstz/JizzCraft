//! What a page or a resolver tells about a video: formats, title, faults.
//!
//! Pure data and parsing, always compiled. Nothing here touches a socket or a
//! process; resolving addresses against a page happens in the network code.

use serde_json::Value;

/// Most entries (videos) one page may describe.
pub const MAX_ENTRIES: usize = 50;

pub(crate) fn is_http(u: &str) -> bool {
    u.starts_with("https://") || u.starts_with("http://")
}

/// Only `User-Agent`, `Referer` and `Origin` travel to a media server, and
/// only with printable ASCII values.
pub(crate) fn allowed_headers(h: Vec<(String, String)>) -> Vec<(String, String)> {
    h.into_iter()
        .filter(|(k, v)| {
            ["user-agent", "referer", "origin"].contains(&k.to_ascii_lowercase().as_str())
                && v.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
        })
        .collect()
}

/// Why a resolver gave nothing. Decided from generic phrases in its stderr
/// (or its exit status); the text itself is dropped right after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Missing,
    Unsupported,
    AgeWall,
    Login,
    Geo,
    Drm,
    RateLimit,
    Gone,
    /// The page builds its video with scripts, which are never run.
    Scripted,
    Other,
}

impl Fault {
    /// The only text that travels in an error string.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Missing => "resolver missing",
            Self::Unsupported => "resolver unsupported",
            Self::AgeWall => "resolver age wall",
            Self::Login => "resolver login",
            Self::Geo => "resolver geo",
            Self::Drm => "resolver drm",
            Self::RateLimit => "resolver rate limit",
            Self::Gone => "resolver gone",
            Self::Scripted => "page needs scripts",
            Self::Other => "resolver failed",
        }
    }

    pub fn from_keyword(s: &str) -> Option<Self> {
        [
            Self::Missing,
            Self::Unsupported,
            Self::AgeWall,
            Self::Login,
            Self::Geo,
            Self::Drm,
            Self::RateLimit,
            Self::Gone,
            Self::Scripted,
            Self::Other,
        ]
        .into_iter()
        .find(|f| f.keyword() == s)
    }

    /// Specificity when several resolvers fail: DRM > age wall > login > geo
    /// > rate limit > unsupported = scripted > gone > missing > other.
    pub fn rank(self) -> u8 {
        match self {
            Self::Drm => 9,
            Self::AgeWall => 8,
            Self::Login => 7,
            Self::Geo => 6,
            Self::RateLimit => 5,
            Self::Unsupported | Self::Scripted => 3,
            Self::Gone => 2,
            Self::Missing => 1,
            Self::Other => 0,
        }
    }
}

/// What the resolver said about the page. Memory only: never written to disk,
/// never logged (the `Debug` form shows nothing of it).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// Control characters stripped, at most 120 characters.
    pub title: Option<String>,
    pub duration_secs: Option<u64>,
    pub thumbnail: Option<String>,
}

impl std::fmt::Debug for Meta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Meta")
            .field("title", &self.title.is_some())
            .field("duration", &self.duration_secs.is_some())
            .finish_non_exhaustive()
    }
}

/// A title safe to show: no control or bidi-override characters, 120 chars.
pub fn sanitize_title(raw: &str) -> Option<String> {
    let s: String = raw
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
        })
        .take(120)
        .collect();
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
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
    /// Bits per second, when the source said so.
    pub bitrate: Option<u64>,
}

/// `{formats:[{url, protocol?, height?}]}` or a single top-level `{url}`.
pub fn parse_formats(json: &[u8]) -> Result<Vec<Format>, String> {
    parse(json).map(|(f, _)| f)
}

/// Formats plus the page's title, duration and thumbnail address.
pub fn parse(json: &[u8]) -> Result<(Vec<Format>, Meta), String> {
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
                    allowed_headers(
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
                bitrate: f
                    .get("tbr")
                    .and_then(Value::as_f64)
                    .filter(|b| b.is_finite() && *b > 0.0 && *b < 1e9)
                    .map(|b| (b * 1000.0) as u64),
            })
        })
        .collect();
    if out.is_empty() {
        return Err("resolver returned no address".into());
    }
    // Best first: highest picture within what the app can stream today.
    out.sort_by_key(|f| std::cmp::Reverse(f.height.unwrap_or(0)));
    let meta = Meta {
        title: v
            .get("title")
            .and_then(Value::as_str)
            .and_then(sanitize_title),
        duration_secs: v
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|d| d.is_finite() && *d >= 0.0 && *d < 1e7)
            .map(|d| d as u64),
        thumbnail: v
            .get("thumbnail")
            .and_then(Value::as_str)
            .filter(|t| is_http(t) && t.len() <= 2048 && !t.chars().any(char::is_control))
            .map(str::to_owned),
    };
    Ok((out, meta))
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

/// One video of a page: its formats (ranked, never empty) and what the page
/// said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub formats: Vec<Format>,
    pub meta: Meta,
}

/// `{"entries":[{...}]}` (at most [`MAX_ENTRIES`], those without a usable
/// format dropped) or one resolver-style document as a single entry.
pub fn parse_entries(json: &[u8]) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_slice(json).map_err(|_| "resolver did not answer JSON")?;
    let Some(list) = v.get("entries").and_then(Value::as_array) else {
        let (formats, meta) = parse(json)?;
        return Ok(vec![Entry { formats, meta }]);
    };
    let out: Vec<Entry> = list
        .iter()
        .take(MAX_ENTRIES)
        .filter_map(|e| {
            let (mut formats, meta) = parse(&serde_json::to_vec(e).ok()?).ok()?;
            rank(&mut formats);
            Some(Entry { formats, meta })
        })
        .collect();
    if out.is_empty() {
        return Err("resolver returned no address".into());
    }
    Ok(out)
}

/// Best first, stable: the tallest picture, then the highest bitrate, then
/// the order the source gave. A manifest without a height adapts to the link
/// and counts as the best; a direct file without a height counts as nothing.
pub fn rank(formats: &mut [Format]) {
    let h = |f: &Format| match f.height {
        Some(h) => h,
        None if f.protocol == "hls" || f.protocol == "dash" => u32::MAX / 2,
        None => 0,
    };
    formats.sort_by(|a, b| {
        h(b).cmp(&h(a))
            .then(b.bitrate.unwrap_or(0).cmp(&a.bitrate.unwrap_or(0)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(url: &str, protocol: &str, height: Option<u32>, bitrate: Option<u64>) -> Format {
        Format {
            url: url.into(),
            height,
            protocol: protocol.into(),
            headers: vec![],
            audio_only: false,
            video_only: false,
            bitrate,
        }
    }

    #[test]
    fn bitrate_comes_from_tbr_in_bits_per_second() {
        let (v, _) = parse(
            br#"{"formats":[{"url":"https://a.invalid/a.mp4","tbr":1500.5},
                {"url":"https://a.invalid/b.mp4","tbr":-4},{"url":"https://a.invalid/c.mp4"}]}"#,
        )
        .unwrap();
        let by = |n: &str| v.iter().find(|x| x.url.ends_with(n)).unwrap().bitrate;
        assert_eq!(by("a.mp4"), Some(1_500_500));
        assert_eq!(by("b.mp4"), None);
        assert_eq!(by("c.mp4"), None);
    }

    #[test]
    fn entries_are_capped_and_a_plain_document_is_one_entry() {
        let many: Vec<_> = (0..80)
            .map(|i| serde_json::json!({"url": format!("https://a.invalid/{i}.mp4"), "title": "t"}))
            .collect();
        let doc = serde_json::json!({"entries": many});
        let e = parse_entries(&serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(e.len(), MAX_ENTRIES);
        assert!(e.iter().all(|x| !x.formats.is_empty()));
        let one = parse_entries(br#"{"url":"https://a.invalid/x.mp4","title":"x"}"#).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].meta.title.as_deref(), Some("x"));
        assert!(parse_entries(br#"{"entries":[{"url":"file:///x"}]}"#).is_err());
        assert!(parse_entries(b"nope").is_err());
    }

    #[test]
    fn rank_is_height_then_bitrate_then_original_order() {
        let mut v = vec![
            f("https://a.invalid/1", "https", None, None),
            f("https://a.invalid/2", "https", Some(720), Some(1000)),
            f("https://a.invalid/3", "https", Some(720), Some(2000)),
            f("https://a.invalid/4", "hls", None, None),
            f("https://a.invalid/5", "https", Some(1080), None),
            f("https://a.invalid/6", "https", Some(720), Some(2000)),
        ];
        rank(&mut v);
        let order: Vec<_> = v
            .iter()
            .map(|x| x.url.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(order, ["4", "5", "3", "6", "2", "1"]);
    }

    #[test]
    fn scripted_fault_round_trips_and_ranks_with_unsupported() {
        assert_eq!(Fault::Scripted.keyword(), "page needs scripts");
        assert_eq!(
            Fault::from_keyword("page needs scripts"),
            Some(Fault::Scripted)
        );
        assert_eq!(Fault::Scripted.rank(), 3);
    }
}
