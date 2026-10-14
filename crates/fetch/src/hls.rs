//! HLS playlist parser. Pure: text in, structure out, no I/O.
//!
//! Only what playback needs. Unknown tags are ignored. Content that cannot be
//! played honestly (sample-level encryption, key systems, low-latency parts)
//! is refused with a fixed reason that `errors::classify` understands.

use std::collections::HashMap;

pub const MAX_TEXT: usize = 4 << 20;
pub const MAX_SEGMENTS: usize = 50_000;
pub const MAX_VARIANTS: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub uri: String,
    pub bandwidth: u64,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    pub audio_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendition {
    pub group: String,
    pub uri: Option<String>,
    pub default: bool,
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapRef {
    pub uri: String,
    pub range: Option<ByteRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRef {
    None,
    Aes128 { uri: String, iv: Option<[u8; 16]> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub uri: String,
    pub duration: f64,
    pub range: Option<ByteRange>,
    pub map: Option<MapRef>,
    pub key: KeyRef,
    pub discontinuity: bool,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Master {
        variants: Vec<Variant>,
        audio: Vec<Rendition>,
    },
    Media {
        target: f64,
        media_seq: u64,
        segments: Vec<Segment>,
        ended: bool,
        vod: bool,
    },
}

/// `KEY=VALUE,KEY="quoted, value"` -> map (keys upper case, quotes removed).
fn attrs(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] == b',' || b[i] == b' ') {
            i += 1;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            continue;
        }
        let key = s[ks..i].trim().to_ascii_uppercase();
        i += 1;
        let val;
        if i < b.len() && b[i] == b'"' {
            i += 1;
            let vs = i;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            val = s[vs..i].to_owned();
            i = (i + 1).min(b.len());
        } else {
            let vs = i;
            while i < b.len() && b[i] != b',' {
                i += 1;
            }
            val = s[vs..i].trim().to_owned();
        }
        out.insert(key, val);
    }
    out
}

fn parse_iv(s: &str) -> Option<[u8; 16]> {
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    if h.is_empty() || h.len() > 32 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let padded = format!("{h:0>32}");
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&padded[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn height_of(res: &str) -> Option<u32> {
    let (_, h) = res.split_once(['x', 'X'])?;
    h.trim().parse().ok()
}

/// `len[@offset]`
fn parse_range(s: &str) -> Option<(u64, Option<u64>)> {
    let (l, o) = match s.split_once('@') {
        Some((l, o)) => (l, Some(o.trim().parse().ok()?)),
        None => (s, None),
    };
    let len: u64 = l.trim().parse().ok()?;
    (len > 0).then_some((len, o))
}

pub fn parse(text: &str) -> Result<Playlist, &'static str> {
    if text.len() > MAX_TEXT {
        return Err("too large");
    }
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if !lines.next().is_some_and(|l| l.starts_with("#EXTM3U")) {
        return Err("no media");
    }

    let mut variants: Vec<Variant> = Vec::new();
    let mut audio: Vec<Rendition> = Vec::new();
    let mut pending_variant: Option<Variant> = None;

    let mut target = 0.0f64;
    let mut media_seq = 0u64;
    let mut segments: Vec<Segment> = Vec::new();
    let mut ended = false;
    let mut vod = false;
    let mut pending_dur: Option<f64> = None;
    let mut pending_range: Option<(u64, Option<u64>)> = None;
    let mut pending_disc = false;
    let mut cur_map: Option<MapRef> = None;
    let mut cur_key = KeyRef::None;
    // (uri, end offset) of the previous ranged segment, for implicit offsets.
    let mut last_end: Option<(String, u64)> = None;

    for line in lines {
        if let Some(tag) = line.strip_prefix('#') {
            let (name, val) = tag.split_once(':').unwrap_or((tag, ""));
            match name {
                "EXT-X-SESSION-KEY" => return Err("protected"),
                "EXT-X-PART" => return Err("unsupported stream"),
                "EXT-X-KEY" => {
                    let a = attrs(val);
                    let method = a.get("METHOD").map_or("NONE", String::as_str);
                    let fmt_ok = a
                        .get("KEYFORMAT")
                        .is_none_or(|f| f.eq_ignore_ascii_case("identity"));
                    match method {
                        "NONE" => cur_key = KeyRef::None,
                        "AES-128" if fmt_ok => {
                            let uri = a.get("URI").ok_or("no media")?.clone();
                            let iv = match a.get("IV") {
                                Some(v) => Some(parse_iv(v).ok_or("no media")?),
                                None => None,
                            };
                            cur_key = KeyRef::Aes128 { uri, iv };
                        }
                        _ => return Err("protected"),
                    }
                }
                "EXT-X-STREAM-INF" => {
                    let a = attrs(val);
                    pending_variant = Some(Variant {
                        uri: String::new(),
                        bandwidth: a
                            .get("BANDWIDTH")
                            .or_else(|| a.get("AVERAGE-BANDWIDTH"))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0),
                        height: a.get("RESOLUTION").and_then(|r| height_of(r)),
                        codecs: a.get("CODECS").cloned(),
                        audio_group: a.get("AUDIO").cloned(),
                    });
                }
                "EXT-X-MEDIA" => {
                    let a = attrs(val);
                    if a.get("TYPE").is_some_and(|t| t == "AUDIO") {
                        audio.push(Rendition {
                            group: a.get("GROUP-ID").cloned().unwrap_or_default(),
                            uri: a.get("URI").cloned(),
                            default: a.get("DEFAULT").is_some_and(|d| d == "YES"),
                            lang: a.get("LANGUAGE").cloned(),
                        });
                    }
                }
                "EXT-X-TARGETDURATION" => target = val.trim().parse().unwrap_or(0.0),
                "EXT-X-MEDIA-SEQUENCE" => media_seq = val.trim().parse().unwrap_or(0),
                "EXT-X-PLAYLIST-TYPE" => vod = val.trim() == "VOD",
                "EXT-X-ENDLIST" => ended = true,
                "EXT-X-DISCONTINUITY" => pending_disc = true,
                "EXTINF" => {
                    let d = val.split(',').next().unwrap_or("").trim();
                    pending_dur = Some(d.parse().map_err(|_| "no media")?);
                }
                "EXT-X-BYTERANGE" => pending_range = parse_range(val.trim()),
                "EXT-X-MAP" => {
                    let a = attrs(val);
                    let uri = a.get("URI").ok_or("no media")?.clone();
                    let range =
                        a.get("BYTERANGE")
                            .and_then(|r| parse_range(r))
                            .map(|(len, off)| ByteRange {
                                offset: off.unwrap_or(0),
                                len,
                            });
                    cur_map = Some(MapRef { uri, range });
                }
                _ => {}
            }
            continue;
        }
        // A URI line.
        if let Some(mut v) = pending_variant.take() {
            if variants.len() < MAX_VARIANTS {
                v.uri = line.to_owned();
                variants.push(v);
            }
            continue;
        }
        let Some(duration) = pending_dur.take() else {
            continue;
        };
        if segments.len() >= MAX_SEGMENTS {
            return Err("too large");
        }
        let range = pending_range.take().map(|(len, off)| {
            let offset = off.unwrap_or_else(|| match &last_end {
                Some((u, e)) if u == line => *e,
                _ => 0,
            });
            last_end = Some((line.to_owned(), offset + len));
            ByteRange { offset, len }
        });
        if range.is_none() {
            last_end = None;
        }
        segments.push(Segment {
            uri: line.to_owned(),
            duration,
            range,
            map: cur_map.clone(),
            key: cur_key.clone(),
            discontinuity: std::mem::take(&mut pending_disc),
            seq: media_seq + segments.len() as u64,
        });
    }

    if !variants.is_empty() {
        return Ok(Playlist::Master { variants, audio });
    }
    if segments.is_empty() {
        return Err("no media");
    }
    Ok(Playlist::Media {
        target,
        media_seq,
        segments,
        ended,
        vod: vod || ended,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_with_and_without_codecs() {
        let t = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"a/a.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"aud\"\n\
v360.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720\n\
v720.m3u8\n";
        let Playlist::Master { variants, audio } = parse(t).unwrap() else {
            panic!()
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].height, Some(360));
        assert_eq!(variants[0].bandwidth, 800_000);
        assert_eq!(variants[0].codecs.as_deref(), Some("avc1.4d401e,mp4a.40.2"));
        assert_eq!(variants[0].audio_group.as_deref(), Some("aud"));
        assert_eq!(variants[1].codecs, None);
        assert_eq!(variants[1].uri, "v720.m3u8");
        assert_eq!(audio.len(), 1);
        assert!(audio[0].default);
        assert_eq!(audio[0].uri.as_deref(), Some("a/a.m3u8"));
        assert_eq!(audio[0].lang.as_deref(), Some("en"));
    }

    #[test]
    fn byterange_chain_continues_same_uri() {
        let t = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:7\n\
#EXTINF:4.0,\n#EXT-X-BYTERANGE:100@0\none.ts\n\
#EXTINF:4.0,\n#EXT-X-BYTERANGE:50\none.ts\n\
#EXTINF:4.0,\n#EXT-X-BYTERANGE:10\ntwo.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media {
            segments,
            ended,
            vod,
            media_seq,
            target,
        } = parse(t).unwrap()
        else {
            panic!()
        };
        assert!(ended && vod);
        assert_eq!((media_seq, target), (7, 4.0));
        assert_eq!(
            segments[0].range,
            Some(ByteRange {
                offset: 0,
                len: 100
            })
        );
        assert_eq!(
            segments[1].range,
            Some(ByteRange {
                offset: 100,
                len: 50
            })
        );
        assert_eq!(segments[2].range, Some(ByteRange { offset: 0, len: 10 }));
        assert_eq!(segments[2].seq, 9);
    }

    #[test]
    fn map_key_and_discontinuity() {
        let t = "#EXTM3U\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n\
#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"\n\
#EXTINF:2.5,\ns0.m4s\n\
#EXT-X-DISCONTINUITY\n\
#EXT-X-KEY:METHOD=NONE\n\
#EXT-X-MAP:URI=\"init2.mp4\"\n\
#EXTINF:2.5,\ns1.m4s\n\
#EXTINF:2.5,\ns2.m4s\n#EXT-X-ENDLIST\n";
        let Playlist::Media { segments, .. } = parse(t).unwrap() else {
            panic!()
        };
        assert!(!segments[0].discontinuity && segments[1].discontinuity);
        assert!(!segments[2].discontinuity);
        let m0 = segments[0].map.as_ref().unwrap();
        assert_eq!(m0.uri, "init.mp4");
        assert_eq!(
            m0.range,
            Some(ByteRange {
                offset: 0,
                len: 720
            })
        );
        assert_eq!(segments[1].map.as_ref().unwrap().uri, "init2.mp4");
        let KeyRef::Aes128 { uri, iv } = &segments[0].key else {
            panic!()
        };
        assert_eq!(uri, "k.bin");
        let iv = iv.unwrap();
        assert_eq!(iv[0], 0);
        assert_eq!(iv[15], 15);
        assert_eq!(segments[1].key, KeyRef::None);
    }

    #[test]
    fn live_is_not_vod() {
        let t = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n";
        let Playlist::Media { ended, vod, .. } = parse(t).unwrap() else {
            panic!()
        };
        assert!(!ended && !vod);
        let t = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2,\na.ts\n";
        let Playlist::Media { vod, .. } = parse(t).unwrap() else {
            panic!()
        };
        assert!(vod);
    }

    #[test]
    fn short_iv_is_left_padded_and_missing_iv_is_none() {
        let t = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x10\n#EXTINF:1,\na.ts\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:1,\nb.ts\n";
        let Playlist::Media { segments, .. } = parse(t).unwrap() else {
            panic!()
        };
        let KeyRef::Aes128 { iv, .. } = &segments[0].key else {
            panic!()
        };
        assert_eq!(iv.unwrap()[15], 0x10);
        let KeyRef::Aes128 { iv, .. } = &segments[1].key else {
            panic!()
        };
        assert!(iv.is_none());
    }

    #[test]
    fn protected_and_unsupported_are_refused() {
        let s = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:1,\na.ts\n";
        assert_eq!(parse(s), Err("protected"));
        let s = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"k\"\n#EXTINF:1,\na.ts\n";
        assert_eq!(parse(s), Err("protected"));
        let s = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",KEYFORMAT=\"com.example.drm\"\n#EXTINF:1,\na.ts\n";
        assert_eq!(parse(s), Err("protected"));
        let s = "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n";
        assert_eq!(parse(s), Err("protected"));
        let s = "#EXTM3U\n#EXTINF:1,\na.ts\n#EXT-X-PART:DURATION=0.5,URI=\"p.ts\"\n";
        assert_eq!(parse(s), Err("unsupported stream"));
    }

    #[test]
    fn caps_and_junk() {
        assert_eq!(parse("<html>nope</html>"), Err("no media"));
        assert_eq!(parse(""), Err("no media"));
        assert_eq!(parse("#EXTM3U\n"), Err("no media"));
        let big = format!("#EXTM3U\n#{}\n", "x".repeat(MAX_TEXT));
        assert_eq!(parse(&big), Err("too large"));
        let mut many = String::from("#EXTM3U\n");
        for i in 0..=MAX_SEGMENTS {
            many.push_str(&format!("#EXTINF:1,\ns{i}.ts\n"));
        }
        assert_eq!(parse(&many), Err("too large"));
        let mut vs = String::from("#EXTM3U\n");
        for i in 0..100 {
            vs.push_str(&format!(
                "#EXT-X-STREAM-INF:BANDWIDTH={}\nv{i}.m3u8\n",
                i + 1
            ));
        }
        let Playlist::Master { variants, .. } = parse(&vs).unwrap() else {
            panic!()
        };
        assert_eq!(variants.len(), MAX_VARIANTS);
    }

    #[test]
    fn unknown_tags_are_ignored() {
        let t = "\u{feff}#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-WHATEVER:A=B\n#EXTINF:1,t\na.ts\n";
        assert!(matches!(parse(t), Ok(Playlist::Media { .. })));
    }
}
