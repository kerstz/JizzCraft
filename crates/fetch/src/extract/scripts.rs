//! Looking for media addresses inside script text, without running it.
//!
//! A hand-written pass over quoted literals: no regex, no evaluation. A
//! literal that is an address of a media file (or of a manifest), or that sits
//! next to a key such as `file` or `hlsUrl`, becomes a candidate. The height is
//! read from the object around it. Everything is bounded.

use super::{MAX_URL, Origin, Proto, RawCandidate};

/// Longest literal looked at, in bytes of source text.
const MAX_LITERAL: usize = MAX_URL;
const LOOK_BACK: usize = 48;
const HEIGHT_WINDOW: usize = 512;

const MEDIA_EXT: &[&str] = &[
    "mp4", "m4v", "webm", "mkv", "mov", "flv", "ts", "m3u8", "mpd",
];
/// Keys that make an address a candidate even without a media extension.
const STRONG_KEYS: &[&str] = &[
    "file",
    "video_url",
    "videoUrl",
    "contentUrl",
    "hls",
    "hlsUrl",
    "manifest",
    "mp4",
    "high",
    "low",
];
/// Keys that only count when the address also ends like a media file.
const WEAK_KEYS: &[&str] = &["src", "source", "url"];
const STATIC_EXT: &[&str] = &[
    "js", "css", "json", "png", "jpg", "jpeg", "gif", "svg", "ico", "webp", "woff", "woff2", "ttf",
    "html", "htm", "vtt", "srt", "xml", "txt", "map",
];

pub fn proto_of(url: &str) -> Proto {
    match path_ext(url).as_deref() {
        Some("m3u8") => Proto::Hls,
        Some("mpd") => Proto::Dash,
        _ => Proto::Http,
    }
}

/// Lowercase extension of the path part of an address.
pub fn path_ext(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next()?;
    let (_, ext) = last.rsplit_once('.')?;
    (!ext.is_empty() && ext.len() <= 6).then(|| ext.to_ascii_lowercase())
}

fn starts_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|h| h.eq_ignore_ascii_case(prefix.as_bytes()))
}

fn is_url_start(s: &str) -> bool {
    starts_ci(s, "http://") || starts_ci(s, "https://") || s.starts_with("//")
}

fn media_ext(url: &str) -> bool {
    path_ext(url).is_some_and(|e| MEDIA_EXT.contains(&e.as_str()))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Undo JavaScript string escapes (`\/`, `\uXXXX`, `\xNN`, `\\`, quotes).
pub fn js_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('u') => {
                let h: String = (0..4)
                    .filter_map(|_| it.next_if(char::is_ascii_hexdigit))
                    .collect();
                match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    Some(ch) if h.len() == 4 => out.push(ch),
                    _ => {}
                }
            }
            Some('x') => {
                let h: String = (0..2)
                    .filter_map(|_| it.next_if(char::is_ascii_hexdigit))
                    .collect();
                if let (2, Ok(n)) = (h.len(), u8::from_str_radix(&h, 16)) {
                    out.push(n as char);
                }
            }
            Some('n' | 'r' | 't' | 'b' | 'f' | 'v' | '0') => {}
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Heights written as `720`, `720p`, `1280x720`, `"HD 1080"`.
pub fn parse_height(s: &str) -> Option<u32> {
    let nums: Vec<u32> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| (3..=4).contains(&t.len()))
        .filter_map(|t| t.parse().ok())
        .collect();
    let h = if s.contains(['x', 'X']) && nums.len() >= 2 {
        nums[1]
    } else {
        *nums.first()?
    };
    (100..=4320).contains(&h).then_some(h)
}

/// A `720p`-style token in the path (`/720p/`, `_1080p.mp4`).
fn height_from_path(url: &str) -> Option<u32> {
    let path = url.split(['?', '#']).next()?;
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if (3..=4).contains(&(j - i))
                && b.get(j) == Some(&b'p')
                && b.get(j + 1).is_none_or(|c| !c.is_ascii_alphanumeric())
            {
                return path[i..j].parse().ok().filter(|h| (100..=4320).contains(h));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

fn key_before(b: &[u8], start: usize) -> Option<&str> {
    let from = start.saturating_sub(LOOK_BACK);
    let mut e = start;
    // Skip the glue between a key and its literal.
    while e > from
        && matches!(
            b[e - 1],
            b' ' | b'\t' | b'\n' | b'\r' | b':' | b'=' | b'"' | b'\''
        )
    {
        e -= 1;
    }
    let mut s = e;
    while s > from && (b[s - 1].is_ascii_alphanumeric() || matches!(b[s - 1], b'_' | b'-' | b'$')) {
        s -= 1;
    }
    (s < e)
        .then(|| std::str::from_utf8(&b[s..e]).ok())
        .flatten()
}

fn height_near(text: &str, start: usize, end: usize) -> Option<u32> {
    let b = text.as_bytes();
    let mut lo = start.saturating_sub(HEIGHT_WINDOW);
    for k in (lo..start).rev() {
        if matches!(b[k], b'{' | b'}') {
            lo = k + 1;
            break;
        }
    }
    let mut hi = (end + HEIGHT_WINDOW).min(b.len());
    for (k, c) in b.iter().enumerate().take(hi).skip(end) {
        if matches!(c, b'{' | b'}') {
            hi = k;
            break;
        }
    }
    let lo = lo.min(start);
    for (a, z) in [(lo, start), (end, hi.max(end))] {
        let w = String::from_utf8_lossy(&b[a..z]);
        if let Some(h) = height_in(&w) {
            return Some(h);
        }
    }
    None
}

/// `label: "720p"`, `res=480`, `quality:'1080'`, `height: 720`.
fn height_in(w: &str) -> Option<u32> {
    let l = w.to_ascii_lowercase();
    for key in ["label", "res", "quality", "height"] {
        let mut from = 0;
        while let Some(i) = l[from..].find(key) {
            let at = from + i;
            from = at + key.len();
            let prev_ok = at == 0 || !l.as_bytes()[at - 1].is_ascii_alphanumeric();
            if !prev_ok {
                continue;
            }
            let rest = l[from..].trim_start_matches(['"', '\'', ' ', '\t']);
            let Some(rest) = rest.strip_prefix([':', '=']) else {
                continue;
            };
            let rest = rest.trim_start_matches(['"', '\'', ' ', '\t']);
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() || digits.len() > 4 {
                continue;
            }
            if let Ok(h) = digits.parse::<u32>()
                && (100..=4320).contains(&h)
            {
                return Some(h);
            }
        }
    }
    None
}

fn candidate(url: String, height: Option<u32>) -> RawCandidate {
    RawCandidate {
        proto: proto_of(&url),
        height: height.or_else(|| height_from_path(&url)),
        bitrate: None,
        origin: Origin::Script,
        url,
    }
}

fn plausible(u: &str) -> bool {
    !u.is_empty() && u.len() <= MAX_URL && !u.chars().any(char::is_control)
}

/// Scan one script's text, appending to `out` (at most `room` candidates).
pub fn scan_script(text: &str, out: &mut Vec<RawCandidate>, room: usize) {
    let b = text.as_bytes();
    let mut i = 0;
    let start_len = out.len();
    while i < b.len() && out.len() - start_len < room {
        let q = b[i];
        if q != b'"' && q != b'\'' {
            i += 1;
            continue;
        }
        // Find the end of the literal: honour escapes, stop at a line break
        // (not allowed in a JavaScript string) or at the length limit.
        let mut j = i + 1;
        let mut ok = false;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                c if c == q => {
                    ok = true;
                    break;
                }
                b'\n' | b'\r' => break,
                _ => j += 1,
            }
        }
        if !ok {
            i = j.max(i + 1).min(b.len());
            continue;
        }
        let Some(raw) = text.get(i + 1..j) else {
            i = j + 1;
            continue;
        };
        let lit_start = i + 1;
        i = j + 1;
        if raw.len() < 6 || raw.len() > MAX_LITERAL {
            continue;
        }
        let mut s = js_unescape(raw);
        if starts_ci(&s, "http%3a") || starts_ci(&s, "https%3a") {
            s = percent_decode(&s);
        }
        if is_url_start(&s) {
            if !plausible(&s) {
                continue;
            }
            let media = media_ext(&s);
            let key = key_before(b, lit_start - 1);
            let strong = key.is_some_and(|k| STRONG_KEYS.contains(&k))
                && !path_ext(&s).is_some_and(|e| STATIC_EXT.contains(&e.as_str()));
            let weak_ok = key.is_some_and(|k| WEAK_KEYS.contains(&k)) && media;
            if media || strong || weak_ok {
                let h = height_near(text, lit_start, j);
                out.push(candidate(s, h));
            }
        } else if s.contains("=http%3A") || s.contains("=https%3A") || s.contains("=http%3a") {
            // flashvars: key=value&key=value with percent-encoded addresses.
            for pair in s.split('&') {
                let Some((_, v)) = pair.split_once('=') else {
                    continue;
                };
                let v = percent_decode(v);
                if is_url_start(&v) && plausible(&v) && media_ext(&v) {
                    out.push(candidate(v, None));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t: &str) -> Vec<RawCandidate> {
        let mut v = Vec::new();
        scan_script(t, &mut v, 64);
        v
    }

    #[test]
    fn sources_array_with_labels_and_escapes() {
        let v = run(
            r#"var p = {sources:[{file:"https:\/\/c.invalid\/a/v720.mp4", label:"720p"},
               {"file":'https://c.invalid/b.mp4',"res":480}], hls: "https://c.invalid/m.m3u8"};"#,
        );
        assert_eq!(v.len(), 3, "{v:?}");
        assert_eq!(v[0].url, "https://c.invalid/a/v720.mp4");
        assert_eq!(v[0].height, Some(720));
        assert_eq!(v[1].height, Some(480));
        assert_eq!(v[2].proto, Proto::Hls);
    }

    #[test]
    fn keys_and_extensions_decide() {
        let v = run(
            r#"a = "https://c.invalid/app.js"; b = {url: "https://c.invalid/track?id=1"};
               c = {file: "https://c.invalid/stream?id=2"}; d = {src: "https://c.invalid/x.webm?t=1"};"#,
        );
        let urls: Vec<_> = v.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://c.invalid/stream?id=2",
                "https://c.invalid/x.webm?t=1"
            ]
        );
    }

    #[test]
    fn percent_encoded_and_flashvars() {
        let v = run(r#"u = "http%3A%2F%2Fc.invalid%2Fq%2Fclip.mp4";
               fv = "id=3&file=http%3A%2F%2Fc.invalid%2Ff.flv&autoplay=1";"#);
        assert_eq!(v.len(), 2, "{v:?}");
        assert_eq!(v[0].url, "http://c.invalid/q/clip.mp4");
        assert_eq!(v[1].url, "http://c.invalid/f.flv");
    }

    #[test]
    fn path_token_is_the_last_resort_height() {
        let v = run(r#"x = "https://c.invalid/v/1080p/clip.mp4";"#);
        assert_eq!(v[0].height, Some(1080));
        assert_eq!(parse_height("1280x720"), Some(720));
        assert_eq!(parse_height("HD"), None);
        assert_eq!(parse_height("9999"), None);
    }

    #[test]
    fn broken_literals_do_not_stall() {
        let long = format!(
            "x = \"{}\"; y = \"https://c.invalid/z.mp4\";",
            "a".repeat(100_000)
        );
        assert_eq!(run(&long).len(), 1);
        assert!(run(&"'".repeat(100_000)).is_empty());
        assert!(run("\"unterminated https://c.invalid/z.mp4").is_empty());
    }
}
