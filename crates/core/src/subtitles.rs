//! Local subtitle sidecars: detection, decoding and a forgiving SRT / WebVTT
//! parser. Read-only: nothing here ever writes next to the media, and no file
//! name leaves this module (labels come from the language tag only).

use std::path::{Path, PathBuf};

/// Sidecars bigger than this are ignored (real ones are well under 1 MiB).
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Hard cap on cues per track, against absurd or hostile files.
pub const MAX_CUES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Plain text, tags removed, lines joined by `\n`.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Srt,
    Vtt,
}

/// A sidecar found next to a media file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    pub path: PathBuf,
    /// Lower-case language tag from `name.fr.srt`, if any.
    pub lang: Option<String>,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub lang: Option<String>,
    pub format: Format,
    /// Sorted by start time.
    pub cues: Vec<Cue>,
}

impl Track {
    /// Texts to show at `ms` (overlapping cues stack, oldest first).
    pub fn active_at(&self, ms: u64) -> Vec<&str> {
        let upto = self.cues.partition_point(|c| c.start_ms <= ms);
        let mut out: Vec<&str> = self.cues[..upto]
            .iter()
            .rev()
            .take(6)
            .filter(|c| c.end_ms > ms)
            .map(|c| c.text.as_str())
            .collect();
        out.reverse();
        out
    }
}

/// Bytes to text: BOM aware, UTF-16 with a BOM, UTF-8, else Latin-1.
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], le: bool| {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| {
                if le {
                    u16::from_le_bytes([p[0], p[1]])
                } else {
                    u16::from_be_bytes([p[0], p[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// `HH:MM:SS,mmm`, `HH:MM:SS.mmm`, `MM:SS.mmm`; milliseconds optional.
fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim();
    let (clock, frac) = match s.find([',', '.']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let num = |p: &str| -> Option<u64> {
        let p = p.trim();
        (!p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (num(h)?, num(m)?, num(s)?),
        [m, s] => (0, num(m)?, num(s)?),
        _ => return None,
    };
    let ms = if frac.is_empty() {
        0
    } else if frac.bytes().all(|b| b.is_ascii_digit()) {
        let mut digits: String = frac.chars().take(3).collect();
        while digits.len() < 3 {
            digits.push('0');
        }
        digits.parse::<u64>().ok()?
    } else {
        return None;
    };
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

/// `start --> end [settings]`.
fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let (a, b) = line.split_once("-->")?;
    let end = b.split_whitespace().next()?;
    Some((parse_time(a)?, parse_time(end)?))
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&lrm;", "")
        .replace("&rlm;", "")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Remove `<i>`, `</b>`, `<font ...>`, `<c.x>`, `<v Name>`, `<00:01.000>` and
/// ASS-style `{\an8}` overrides; a lone `<` ("a < b", "<3") stays.
fn clean_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find(['<', '{']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (close, is_tag) = if let Some(after) = tail.strip_prefix('<') {
            let next = after.chars().next();
            let tag = match next {
                Some(c) if c.is_alphabetic() || c == '/' => true,
                Some(c) if c.is_ascii_digit() => {
                    tail.find('>').is_some_and(|e| tail[..e].contains(':'))
                }
                _ => false,
            };
            ('>', tag)
        } else {
            ('}', tail[1..].starts_with('\\'))
        };
        match tail.find(close) {
            Some(e) if is_tag && e <= 200 => rest = &tail[e + 1..],
            _ => {
                out.push_str(&tail[..1]);
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out)
}

fn finish(cue: Option<(u64, u64, Vec<String>)>, dropped_index: bool, out: &mut Vec<Cue>) {
    let Some((start, end, mut lines)) = cue else {
        return;
    };
    // Without a blank line, the next cue's number lands in this cue's text.
    if dropped_index
        && lines
            .last()
            .is_some_and(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
    {
        lines.pop();
    }
    let text = lines
        .iter()
        .map(|l| clean_line(l).trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() && out.len() < MAX_CUES {
        out.push(Cue {
            start_ms: start,
            end_ms: end.max(start),
            text,
        });
    }
}

/// Parse SRT or WebVTT text (the two are told apart by nothing: both are
/// "timing line, then text until a blank line"). Garbage yields no cues.
pub fn parse_str(text: &str) -> Vec<Cue> {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = Vec::new();
    let mut cur: Option<(u64, u64, Vec<String>)> = None;
    for raw in text.split('\n') {
        let line = raw.trim_end();
        if let Some((s, e)) = parse_timing(line) {
            finish(cur.take(), true, &mut out);
            cur = Some((s, e, Vec::new()));
        } else if line.trim().is_empty() {
            finish(cur.take(), false, &mut out);
        } else if let Some((_, _, lines)) = cur.as_mut() {
            lines.push(line.to_owned());
        }
    }
    finish(cur.take(), false, &mut out);
    out.sort_by_key(|c| c.start_ms);
    out
}

/// Parse raw file bytes (any supported encoding).
pub fn parse(bytes: &[u8]) -> Vec<Cue> {
    parse_str(&decode(bytes))
}

fn is_lang_tag(s: &str) -> bool {
    (1..=12).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Sidecars of `media`: same directory, same base name (case-insensitive),
/// `.srt` or `.vtt`, with an optional language tag (`name.fr.srt`).
/// Untagged first, then by tag. Not recursive.
pub fn find_sidecars(media: &Path) -> Vec<Sidecar> {
    let (Some(dir), Some(stem)) = (media.parent(), media.file_stem().and_then(|s| s.to_str()))
    else {
        return Vec::new();
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let stem = stem.to_lowercase();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let lower = name.to_lowercase();
        let (base, format) = if let Some(b) = lower.strip_suffix(".srt") {
            (b, Format::Srt)
        } else if let Some(b) = lower.strip_suffix(".vtt") {
            (b, Format::Vtt)
        } else {
            continue;
        };
        let lang = if base == stem {
            None
        } else if let Some(tag) = base
            .strip_prefix(&stem)
            .and_then(|r| r.strip_prefix('.'))
            .filter(|t| is_lang_tag(t))
        {
            Some(tag.to_owned())
        } else {
            continue;
        };
        found.push(Sidecar {
            path: entry.path(),
            lang,
            format,
        });
    }
    found.sort_by(|a, b| {
        (a.lang.is_some(), &a.lang, a.format as u8).cmp(&(
            b.lang.is_some(),
            &b.lang,
            b.format as u8,
        ))
    });
    found
}

/// Read and parse one sidecar. None when unreadable, too big or no cue.
pub fn load(sc: &Sidecar) -> Option<Track> {
    let meta = std::fs::metadata(&sc.path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let cues = parse(&std::fs::read(&sc.path).ok()?);
    (!cues.is_empty()).then(|| Track {
        lang: sc.lang.clone(),
        format: sc.format,
        cues,
    })
}

/// Every usable track next to `media`.
pub fn load_all(media: &Path) -> Vec<Track> {
    find_sidecars(media).iter().filter_map(load).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_basic_multiline_and_html() {
        let src = "1\n00:00:01,000 --> 00:00:03,500\nHello <i>there</i>\n<font color=\"#fff\">second</font> line\n\n2\n00:01:02,250 --> 00:01:04,000\nBye &amp; thanks\n";
        let c = parse_str(src);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].start_ms, c[0].end_ms), (1_000, 3_500));
        assert_eq!(c[0].text, "Hello there\nsecond line");
        assert_eq!((c[1].start_ms, c[1].end_ms), (62_250, 64_000));
        assert_eq!(c[1].text, "Bye & thanks");
    }

    #[test]
    fn crlf_cr_and_bom() {
        let src = "\u{feff}1\r\n00:00:00,500 --> 00:00:01,000\r\nOne\r\n\r\n2\r\n00:00:02,000 --> 00:00:03,000\r\nTwo\r\n";
        let c = parse_str(src);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "One");
        let old_mac =
            "1\r00:00:00,000 --> 00:00:01,000\rA\r\r2\r00:00:02,000 --> 00:00:03,000\rB\r";
        assert_eq!(parse_str(old_mac).len(), 2);
    }

    #[test]
    fn vtt_header_notes_settings_and_short_times() {
        let src = "WEBVTT - synthetic\n\nSTYLE\n::cue { color: red }\n\nNOTE a comment\n--> not a cue\n\nintro\n00:01.000 --> 00:02.500 align:start position:0%\n<v Narrator>Hi <c.yellow>you</c> <00:01.500>all\n\n01:00:00.000 --> 01:00:01.000\nLate\n";
        let c = parse_str(src);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].start_ms, c[0].end_ms), (1_000, 2_500));
        assert_eq!(c[0].text, "Hi you all");
        assert_eq!(c[1].start_ms, 3_600_000);
    }

    #[test]
    fn tolerant_of_dot_millis_missing_blank_lines_and_bad_blocks() {
        let src = "1\n00:00:01.000 --> 00:00:02.000\nA\n2\n00:00:03,000 --> 00:00:04,000\nB\n\nnonsense\n\n3\n00:00:xx,000 --> 00:00:06,000\nbroken\n\n4\n00:00:07,5 --> 00:00:08,000\nC\n";
        let c = parse_str(src);
        let texts: Vec<_> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["A", "B", "C"]);
        assert_eq!(c[2].start_ms, 7_500);
    }

    #[test]
    fn lone_angle_bracket_and_ass_override() {
        let src = "00:00:00,000 --> 00:00:01,000\n{\\an8}2 < 3 and <3\n";
        assert_eq!(parse_str(src)[0].text, "2 < 3 and <3");
    }

    #[test]
    fn output_is_sorted_and_empty_cues_dropped() {
        let src = "00:00:05,000 --> 00:00:06,000\nlate\n\n00:00:01,000 --> 00:00:02,000\nearly\n\n00:00:03,000 --> 00:00:04,000\n<i></i>\n";
        let c = parse_str(src);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "early");
    }

    #[test]
    fn garbage_yields_nothing() {
        assert!(parse(b"\x00\x01\x02 not subtitles").is_empty());
        assert!(parse(b"").is_empty());
    }

    #[test]
    fn decoding_utf8_latin1_and_utf16() {
        let body = "00:00:00,000 --> 00:00:01,000\ncaf\u{e9}\n";
        assert_eq!(parse(body.as_bytes())[0].text, "café");
        let latin1: Vec<u8> = body.chars().map(|c| c as u8).collect();
        assert!(std::str::from_utf8(&latin1).is_err());
        assert_eq!(parse(&latin1)[0].text, "café");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(body.as_bytes());
        assert_eq!(parse(&bom)[0].text, "café");
        let mut u16le = vec![0xFF, 0xFE];
        for u in body.encode_utf16() {
            u16le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(parse(&u16le)[0].text, "café");
    }

    #[test]
    fn active_cues_follow_time_and_overlaps() {
        let t = Track {
            lang: None,
            format: Format::Srt,
            cues: parse_str(
                "00:00:01,000 --> 00:00:05,000\nlong\n\n00:00:02,000 --> 00:00:03,000\nshort\n",
            ),
        };
        assert!(t.active_at(500).is_empty());
        assert_eq!(t.active_at(1_000), ["long"]);
        assert_eq!(t.active_at(2_500), ["long", "short"]);
        assert_eq!(t.active_at(4_000), ["long"]);
        assert!(t.active_at(5_000).is_empty());
    }

    #[test]
    fn sidecars_by_base_name_with_language_tags() {
        let d = tempfile::tempdir().unwrap();
        let p = |n: &str| d.path().join(n);
        let srt = "1\n00:00:01,000 --> 00:00:02,000\nhi\n";
        for n in [
            "clip.mp4",
            "clip.srt",
            "Clip.FR.SRT",
            "clip.pt-BR.vtt",
            "clip.en.forced.srt",
            "clip2.srt",
            "other.srt",
            "clipx.fr.srt",
        ] {
            std::fs::write(p(n), srt).unwrap();
        }
        std::fs::create_dir(p("clip.de.srt")).unwrap();
        let langs: Vec<_> = find_sidecars(&p("clip.mp4"))
            .into_iter()
            .map(|s| s.lang)
            .collect();
        assert_eq!(
            langs,
            [None, Some("fr".into()), Some("pt-br".into())],
            "{langs:?}"
        );
        assert_eq!(load_all(&p("clip.mp4")).len(), 3);
        // Oversized and empty files are skipped, never fatal.
        std::fs::write(p("big.srt"), vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        std::fs::write(p("big.mp4"), b"").unwrap();
        assert!(load_all(&p("big.mp4")).is_empty());
        assert!(find_sidecars(&p("nowhere/none.mp4")).is_empty());
    }
}
