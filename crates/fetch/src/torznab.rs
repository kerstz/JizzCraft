//! Torznab (Jackett / Prowlarr compatible) request building and response
//! parsing. No I/O here.

use crate::config::Indexer;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub size: Option<u64>,
    pub seeders: Option<u32>,
    pub peers: Option<u32>,
    pub magnet: Option<String>,
    pub link: Option<String>,
    pub pub_date: Option<String>,
    pub indexer_id: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TorznabError {
    /// `<error code=".." description=".."/>` from the indexer.
    Indexer {
        code: i32,
        description: String,
    },
    Malformed,
}

impl std::fmt::Display for TorznabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Indexer { code, description } => write!(f, "indexer error {code}: {description}"),
            Self::Malformed => f.write_str("malformed indexer response"),
        }
    }
}

impl std::error::Error for TorznabError {}

pub(crate) fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn base(ix: &Indexer) -> String {
    let u = ix.url.trim_end_matches('/');
    format!("{u}/api?apikey={}", pct_encode(ix.api_key.expose()))
}

pub fn build_caps_url(ix: &Indexer) -> String {
    format!("{}&t=caps", base(ix))
}

pub fn build_search_url(ix: &Indexer, query: &str, cats: &[u32], limit: u32) -> String {
    let mut u = format!(
        "{}&t=search&q={}&limit={limit}",
        base(ix),
        pct_encode(query)
    );
    if !cats.is_empty() {
        let c: Vec<String> = cats.iter().map(u32::to_string).collect();
        u.push_str("&cat=");
        u.push_str(&c.join(","));
    }
    u
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name.as_bytes())
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Parse a Torznab/RSS search response.
pub fn parse_results(xml: &str, indexer_id: u32) -> Result<Vec<SearchResult>, TorznabError> {
    let mut r = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<SearchResult> = None;
    let mut text_target: Option<&'static str> = None;
    let mut saw_root = false;
    let mut text = String::new();
    loop {
        match r.read_event().map_err(|_| TorznabError::Malformed)? {
            Event::Start(e) | Event::Empty(e) => {
                text.clear();
                let name = e.name();
                match name.as_ref() {
                    b"rss" | b"feed" => saw_root = true,
                    b"error" => {
                        return Err(TorznabError::Indexer {
                            code: attr(&e, "code").and_then(|c| c.parse().ok()).unwrap_or(0),
                            description: attr(&e, "description").unwrap_or_default(),
                        });
                    }
                    b"item" => {
                        cur = Some(SearchResult {
                            indexer_id,
                            ..Default::default()
                        });
                    }
                    b"title" if cur.is_some() => text_target = Some("title"),
                    b"link" if cur.is_some() => text_target = Some("link"),
                    b"pubDate" if cur.is_some() => text_target = Some("pubDate"),
                    b"size" if cur.is_some() => text_target = Some("size"),
                    b"enclosure" => {
                        if let (Some(c), Some(u)) = (cur.as_mut(), attr(&e, "url")) {
                            if u.starts_with("magnet:") {
                                c.magnet.get_or_insert(u);
                            } else {
                                c.link.get_or_insert(u);
                            }
                            if c.size.is_none() {
                                c.size = attr(&e, "length").and_then(|l| l.parse().ok());
                            }
                        }
                    }
                    b"torznab:attr" | b"newznab:attr" => {
                        if let (Some(c), Some(n), Some(v)) =
                            (cur.as_mut(), attr(&e, "name"), attr(&e, "value"))
                        {
                            match n.as_str() {
                                "seeders" => c.seeders = v.parse().ok(),
                                "peers" => c.peers = v.parse().ok(),
                                "size" => c.size = v.parse().ok(),
                                "magneturl" => c.magnet = Some(v),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                let v = t.decode().map_err(|_| TorznabError::Malformed)?;
                text.push_str(&v);
            }
            Event::CData(t) => {
                let v = t.decode().map_err(|_| TorznabError::Malformed)?;
                text.push_str(&v);
            }
            Event::GeneralRef(g) => {
                if let Ok(Some(c)) = g.resolve_char_ref() {
                    text.push(c);
                } else {
                    match g.decode().map_err(|_| TorznabError::Malformed)?.as_ref() {
                        "amp" => text.push('&'),
                        "lt" => text.push('<'),
                        "gt" => text.push('>'),
                        "quot" => text.push('"'),
                        "apos" => text.push('\''),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                if let (Some(c), Some(k)) = (cur.as_mut(), text_target.take()) {
                    let v = std::mem::take(&mut text);
                    let v = v.trim().to_owned();
                    match k {
                        "title" => c.title = v,
                        "link" => {
                            if v.starts_with("magnet:") {
                                c.magnet.get_or_insert(v);
                            } else {
                                c.link.get_or_insert(v);
                            }
                        }
                        "pubDate" => c.pub_date = Some(v),
                        "size" => c.size = v.parse().ok(),
                        _ => {}
                    }
                }
                text.clear();
                if e.name().as_ref() == b"item"
                    && let Some(c) = cur.take()
                    && (c.magnet.is_some() || c.link.is_some())
                {
                    out.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root {
        return Err(TorznabError::Malformed);
    }
    Ok(out)
}

/// Capabilities check: success means the endpoint and key are accepted.
pub fn parse_caps(xml: &str) -> Result<(), TorznabError> {
    let mut r = Reader::from_str(xml);
    let mut ok = false;
    loop {
        match r.read_event().map_err(|_| TorznabError::Malformed)? {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"caps" => ok = true,
                b"error" => {
                    return Err(TorznabError::Indexer {
                        code: attr(&e, "code").and_then(|c| c.parse().ok()).unwrap_or(0),
                        description: attr(&e, "description").unwrap_or_default(),
                    });
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if ok {
        Ok(())
    } else {
        Err(TorznabError::Malformed)
    }
}
