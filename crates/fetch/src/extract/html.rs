//! A tolerant tag tokenizer. It is not a DOM and builds no tree: it walks the
//! text once, yields start tags, end tags, raw-text elements and text, and
//! never recurses. Every limit is a constant so hostile pages stay cheap.

/// Most attributes kept per tag.
pub const MAX_ATTRS: usize = 64;
/// Longest attribute value kept, in bytes.
pub const MAX_ATTR_VALUE: usize = 8 * 1024;
/// Most bytes one tag may span before it is cut where it stands.
pub const MAX_TAG: usize = 64 * 1024;
const MAX_ATTR_NAME: usize = 256;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tag {
    /// Lowercase.
    pub name: String,
    /// Names lowercase, values with the common entities decoded.
    pub attrs: Vec<(String, String)>,
    pub self_closing: bool,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }
}

#[derive(Debug)]
pub enum Tok<'a> {
    Start(Tag),
    End(String),
    /// `script`, `style`, `title`, `noscript` or `textarea`: the tag and its
    /// raw text, up to the closing tag or the end of the document.
    Raw {
        tag: Tag,
        text: &'a str,
    },
    /// Text between tags (never only white space).
    Text(&'a str),
}

pub struct Tokenizer<'a> {
    s: &'a str,
    pos: usize,
    /// No '>' exists before this index (a failed search is never repeated).
    no_gt_until: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(s: &'a str) -> Self {
        Self {
            s,
            pos: 0,
            no_gt_until: 0,
        }
    }

    /// The first '>' in `from..limit`. A search that fails is remembered, so
    /// a page of unclosed tags costs one pass, not one pass per tag.
    fn find_gt(&mut self, from: usize, limit: usize) -> Option<usize> {
        let b = self.s.as_bytes();
        let limit = limit.min(b.len());
        let from = from.max(self.no_gt_until);
        let r = find_byte(b, from, limit, b'>');
        if r.is_none() {
            self.no_gt_until = self.no_gt_until.max(limit);
        }
        r
    }
}

fn boundary(s: &str, mut p: usize) -> usize {
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p.min(s.len())
}

fn is_raw(name: &str) -> bool {
    matches!(name, "script" | "style" | "title" | "noscript" | "textarea")
}

/// Index of `byte` in `b[from..limit]`.
fn find_byte(b: &[u8], from: usize, limit: usize, byte: u8) -> Option<usize> {
    let end = limit.min(b.len());
    (from < end).then(|| {
        b[from..end]
            .iter()
            .position(|&x| x == byte)
            .map(|i| from + i)
    })?
}

fn find_close(b: &[u8], from: usize, name: &str) -> Option<usize> {
    let n = name.len();
    let mut i = from;
    while i + 2 + n <= b.len() {
        if b[i] == b'<'
            && b[i + 1] == b'/'
            && b[i + 2..i + 2 + n].eq_ignore_ascii_case(name.as_bytes())
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b':' | b'_')
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Tok<'a>;

    fn next(&mut self) -> Option<Tok<'a>> {
        let s = self.s;
        let b = s.as_bytes();
        loop {
            if self.pos >= b.len() {
                return None;
            }
            let start = self.pos;
            // The next '<' that really opens markup.
            let mut i = start;
            let lt = loop {
                match b[i..].iter().position(|&x| x == b'<') {
                    None => break None,
                    Some(k) => {
                        let j = i + k;
                        let n = b.get(j + 1).copied().unwrap_or(0);
                        let n2 = b.get(j + 2).copied().unwrap_or(0);
                        if n.is_ascii_alphabetic()
                            || n == b'!'
                            || n == b'?'
                            || (n == b'/' && n2.is_ascii_alphabetic())
                        {
                            break Some(j);
                        }
                        i = j + 1;
                    }
                }
            };
            let end_text = lt.unwrap_or(b.len());
            if end_text > start {
                let text = &s[start..end_text];
                self.pos = end_text;
                if !text.trim().is_empty() {
                    return Some(Tok::Text(text));
                }
            }
            let Some(j) = lt else {
                self.pos = b.len();
                return None;
            };
            // Markup at j.
            if b[j..].starts_with(b"<!--") {
                self.pos = match find_close_comment(b, j + 4) {
                    Some(e) => e + 3,
                    None => b.len(),
                };
                continue;
            }
            let n = b[j + 1];
            if n == b'!' || n == b'?' {
                self.pos = match self.find_gt(j, j + MAX_TAG) {
                    Some(e) => e + 1,
                    None => boundary(s, (j + MAX_TAG).min(b.len())),
                };
                continue;
            }
            if n == b'/' {
                let mut k = j + 2;
                while k < b.len() && name_byte(b[k]) {
                    k += 1;
                }
                let name = s[j + 2..k].to_ascii_lowercase();
                self.pos = match self.find_gt(k, j + MAX_TAG) {
                    Some(e) => e + 1,
                    None => boundary(s, k),
                };
                return Some(Tok::End(name));
            }
            let (tag, end) = parse_tag(s, j);
            self.pos = end;
            if is_raw(&tag.name) && !tag.self_closing {
                let from = end;
                let (text_end, after) = match find_close(b, from, &tag.name) {
                    Some(c) => {
                        let after = self.find_gt(c, c + MAX_TAG).map_or(c + 2, |e| e + 1);
                        (c, boundary(s, after))
                    }
                    None => (b.len(), b.len()),
                };
                self.pos = after;
                return Some(Tok::Raw {
                    tag,
                    text: &s[from..text_end],
                });
            }
            return Some(Tok::Start(tag));
        }
    }
}

fn find_close_comment(b: &[u8], from: usize) -> Option<usize> {
    b.get(from..)?
        .windows(3)
        .position(|w| w == b"-->")
        .map(|i| from + i)
}

/// Parse the start tag at `j` (`s[j] == '<'`). Returns the tag and the index
/// after it. A tag that outgrows [`MAX_TAG`] is cut where it stands and
/// parsing resumes there, so a hostile page costs a single pass.
fn parse_tag(s: &str, j: usize) -> (Tag, usize) {
    let b = s.as_bytes();
    let limit = (j + MAX_TAG).min(b.len());
    let mut i = j + 1;
    while i < b.len() && name_byte(b[i]) {
        i += 1;
    }
    let mut tag = Tag {
        name: s[j + 1..i].to_ascii_lowercase(),
        ..Tag::default()
    };
    loop {
        while i < limit && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return (tag, b.len());
        }
        if i >= limit {
            return (tag, boundary(s, limit));
        }
        match b[i] {
            b'>' => return (tag, i + 1),
            b'/' => {
                if b.get(i + 1) == Some(&b'>') {
                    tag.self_closing = true;
                    return (tag, i + 2);
                }
                i += 1;
                continue;
            }
            _ => {}
        }
        let ns = i;
        while i < limit && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == ns {
            // A stray '=' or similar: step over it.
            i += 1;
            continue;
        }
        let name = boundary_slice(s, ns, i);
        while i < limit && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        let mut keep = true;
        if i < limit && b[i] == b'=' {
            i += 1;
            while i < limit && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < limit && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                match find_byte(b, i + 1, limit, q) {
                    Some(e) => {
                        if e - (i + 1) <= MAX_ATTR_VALUE {
                            value = decode(boundary_slice(s, i + 1, e));
                        } else {
                            keep = false;
                        }
                        i = e + 1;
                    }
                    None => return (tag, boundary(s, limit)),
                }
            } else {
                let vs = i;
                while i < limit && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                if i - vs <= MAX_ATTR_VALUE {
                    value = decode(boundary_slice(s, vs, i));
                } else {
                    keep = false;
                }
            }
        }
        if keep && tag.attrs.len() < MAX_ATTRS && name.len() <= MAX_ATTR_NAME {
            tag.attrs.push((name.to_ascii_lowercase(), value));
        }
    }
}

/// `s[a..b]` with both ends moved to character boundaries.
fn boundary_slice(s: &str, a: usize, b: usize) -> &str {
    let (a, b) = (boundary(s, a), boundary(s, b));
    &s[a..b.max(a)]
}

/// Decode the entities that matter in attribute values.
pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(k, _)| k);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                "lt" => '<',
                "gt" => '>',
                _ => {
                    let num = name.strip_prefix('#')?;
                    let n = match num.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => num.parse::<u32>().ok()?,
                    };
                    char::from_u32(n)?
                }
            };
            Some((c, e + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_attributes_and_entities() {
        let t: Vec<_> = Tokenizer::new(
            r#"<Video SRC="a.mp4?x=1&amp;y=2" muted loop data-x='q"r'><source src=b.mp4 /><!-- <x> --></video>"#,
        )
        .collect();
        let Tok::Start(v) = &t[0] else { panic!() };
        assert_eq!(v.name, "video");
        assert_eq!(v.attr("src"), Some("a.mp4?x=1&y=2"));
        assert!(v.has("muted") && v.has("loop"));
        assert_eq!(v.attr("data-x"), Some("q\"r"));
        let Tok::Start(s) = &t[1] else { panic!() };
        assert_eq!(s.attr("src"), Some("b.mp4"));
        assert!(matches!(&t[2], Tok::End(n) if n == "video"));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn raw_text_elements_keep_their_markup() {
        let t: Vec<_> =
            Tokenizer::new("<script>if (a<b) { x = '</p>' }</SCRIPT>hello<title>T &amp; U</title>")
                .collect();
        let Tok::Raw { tag, text } = &t[0] else {
            panic!()
        };
        assert_eq!(tag.name, "script");
        assert!(text.contains("a<b"));
        assert!(matches!(&t[1], Tok::Text("hello")));
        assert!(matches!(
            &t[2],
            Tok::Raw {
                text: "T &amp; U",
                ..
            }
        ));
    }

    #[test]
    fn numeric_entities_and_junk() {
        assert_eq!(decode("&#39;&#x41;&lt;&gt;&bogus; &"), "'A<>&bogus; &");
        assert_eq!(decode("&#1114112;"), "&#1114112;");
    }
}
