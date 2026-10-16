//! The generic page reader, pure half: turn the bytes of a page into
//! candidate addresses and a few facts, with no I/O, no regex and no site
//! knowledge. It reads markup any page may carry: HTML5 video, Open Graph,
//! schema.org JSON-LD, manifests linked as alternates, and script text.
//!
//! Addresses come out as they were written (unresolved); resolving them
//! against the page, and checking them against the network rules, happens in
//! the network code. A page's scripts are read, never run; a meta refresh is
//! recorded, never followed.

pub mod html;
pub mod scripts;
pub mod signals;

use crate::formats::{Meta, sanitize_title};
use html::{Tag, Tok, Tokenizer};
use serde_json::Value;

/// Most page bytes looked at.
pub const MAX_PAGE: usize = 2 << 20;
pub const MAX_CANDIDATES: usize = 64;
pub const MAX_IFRAMES: usize = 3;
pub const MAX_URL: usize = 4096;

const MAX_LD_BLOCKS: usize = 16;
const MAX_LD_BYTES: usize = 256 * 1024;
const MAX_SCRIPT_TEXT: usize = 1 << 20;
const MAX_LD_NODES: usize = 1000;
const MAX_LD_DEPTH: usize = 8;

/// Where a candidate was found, most trusted first. Ordering is the ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    Rule,
    JsonLd,
    OpenGraph,
    Twitter,
    LinkAlternate,
    VideoTag,
    SourceTag,
    Script,
    /// A muted looping `<video>`: a hover preview, kept but ranked last.
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Http,
    Hls,
    Dash,
}

/// An address found in a page, not yet resolved against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCandidate {
    pub url: String,
    pub height: Option<u32>,
    /// Bits per second.
    pub bitrate: Option<u64>,
    pub proto: Proto,
    pub origin: Origin,
}

#[derive(Debug, Clone, Default)]
pub struct RawItem {
    pub meta: Meta,
    pub candidates: Vec<RawCandidate>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Signals {
    pub age_gate: bool,
    pub password_form: bool,
    pub scripted: bool,
    pub drm: bool,
    pub meta_refresh: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PageScan {
    /// At least one.
    pub items: Vec<RawItem>,
    /// Unresolved, at most [`MAX_IFRAMES`].
    pub iframes: Vec<String>,
    pub base_href: Option<String>,
    pub signals: Signals,
}

fn proto_from_type(t: &str) -> Option<Proto> {
    let t = t.to_ascii_lowercase();
    if t.contains("mpegurl") {
        Some(Proto::Hls)
    } else if t.contains("dash+xml") {
        Some(Proto::Dash)
    } else {
        None
    }
}

/// An address that can be resolved later: no control characters, no scheme
/// other than http(s), not absurdly long.
fn plausible(u: &str) -> Option<String> {
    let u = u.trim();
    if u.is_empty() || u.len() > MAX_URL || u.chars().any(char::is_control) {
        return None;
    }
    let scheme_end = u.find([':', '/', '?', '#']);
    if let Some(i) = scheme_end
        && u.as_bytes()[i] == b':'
        && !(u[..i].eq_ignore_ascii_case("http") || u[..i].eq_ignore_ascii_case("https"))
    {
        return None;
    }
    Some(u.to_owned())
}

fn proto_for(url: &str, hint: Option<Proto>) -> Proto {
    hint.unwrap_or_else(|| scripts::proto_of(url))
}

fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with(['P', 'p']) {
        return crate::dash::iso_duration(&s.to_ascii_uppercase()).map(|d| d.as_secs());
    }
    s.parse::<f64>()
        .ok()
        .filter(|d| d.is_finite() && *d >= 0.0 && *d < 1e7)
        .map(|d| d as u64)
}

fn bitrate_of(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => {
            let l = s.trim().to_ascii_lowercase();
            let digits: String = l.chars().take_while(|c| c.is_ascii_digit()).collect();
            let n: u64 = digits.parse().ok()?;
            Some(if l.contains("mbps") {
                n.saturating_mul(1_000_000)
            } else if l.contains("kbps") || l.contains("kbit") {
                n.saturating_mul(1000)
            } else {
                n
            })
        }
        _ => None,
    }
}

fn height_of(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|h| u32::try_from(h).ok()),
        Value::String(s) => scripts::parse_height(s),
        Value::Object(o) => o.get("value").and_then(height_of),
        _ => None,
    }
    .filter(|h| (100..=4320).contains(h))
}

fn first_str(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        Value::Array(a) => a.iter().find_map(first_str),
        Value::Object(o) => o
            .get("url")
            .or_else(|| o.get("contentUrl"))
            .and_then(first_str),
        _ => None,
    }
}

#[derive(Default)]
struct LdVideo {
    content: Option<String>,
    name: Option<String>,
    duration: Option<u64>,
    thumb: Option<String>,
    height: Option<u32>,
    bitrate: Option<u64>,
}

fn is_video_object(o: &serde_json::Map<String, Value>) -> bool {
    match o.get("@type") {
        Some(Value::String(t)) => t == "VideoObject",
        Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("VideoObject")),
        _ => false,
    }
}

fn walk_ld(
    v: &Value,
    depth: usize,
    nodes: &mut usize,
    out: &mut Vec<LdVideo>,
    embeds: &mut Vec<String>,
) {
    *nodes += 1;
    if depth > MAX_LD_DEPTH || *nodes > MAX_LD_NODES {
        return;
    }
    match v {
        Value::Array(a) => {
            for x in a {
                walk_ld(x, depth + 1, nodes, out, embeds);
            }
        }
        Value::Object(o) => {
            if is_video_object(o) {
                let s = |k: &str| o.get(k).and_then(first_str);
                if let Some(e) = s("embedUrl").and_then(plausible) {
                    embeds.push(e);
                }
                out.push(LdVideo {
                    content: s("contentUrl").and_then(plausible),
                    name: s("name").and_then(sanitize_title),
                    duration: s("duration").and_then(parse_duration),
                    thumb: s("thumbnailUrl").and_then(plausible),
                    height: o.get("height").and_then(height_of),
                    bitrate: o.get("bitrate").and_then(bitrate_of),
                });
            }
            for k in [
                "@graph",
                "itemListElement",
                "item",
                "mainEntity",
                "video",
                "hasPart",
            ] {
                if let Some(x) = o.get(k) {
                    walk_ld(x, depth + 1, nodes, out, embeds);
                }
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct PageMeta {
    og_title: Option<String>,
    tw_title: Option<String>,
    html_title: Option<String>,
    duration: Option<u64>,
    thumb: Option<String>,
    og_height: Option<u32>,
    og_type: Option<String>,
    og_urls: Vec<String>,
    tw_stream: Option<String>,
    prop_content: Option<String>,
}

/// Scan a page. Never panics, whatever the bytes; reads at most
/// [`MAX_PAGE`] of them.
pub fn scan(html_bytes: &[u8]) -> PageScan {
    let bytes = &html_bytes[..html_bytes.len().min(MAX_PAGE)];
    let text = String::from_utf8_lossy(bytes);
    let mut pm = PageMeta::default();
    let mut facts = signals::Facts::default();
    let mut page_cands: Vec<RawCandidate> = Vec::new();
    let mut iframes: Vec<String> = Vec::new();
    let mut base_href: Option<String> = None;
    let mut ld_blocks: Vec<&str> = Vec::new();
    let mut script_budget = MAX_SCRIPT_TEXT;
    let mut script_cands: Vec<RawCandidate> = Vec::new();
    let mut poster: Option<String> = None;
    // The video element being read, if any: (preview, nth source).
    let mut in_video: Option<bool> = None;
    let mut root_pending = false;

    let push = |cands: &mut Vec<RawCandidate>, url: &str, h, hint, origin| {
        if let Some(u) = plausible(url) {
            cands.push(RawCandidate {
                proto: proto_for(&u, hint),
                url: u,
                height: h,
                bitrate: None,
                origin,
            });
        }
    };

    for tok in Tokenizer::new(&text) {
        let was_pending = std::mem::take(&mut root_pending);
        match tok {
            Tok::Text(t) => facts.add_text(t),
            Tok::End(name) => {
                if was_pending {
                    facts.empty_root = true;
                }
                if name == "video" {
                    in_video = None;
                }
            }
            Tok::Raw { tag, text: body } => match tag.name.as_str() {
                "script" => {
                    facts.script_tags += 1;
                    let ty = tag.attr("type").unwrap_or("").to_ascii_lowercase();
                    if ty.contains("ld+json") {
                        if ld_blocks.len() < MAX_LD_BLOCKS && body.len() <= MAX_LD_BYTES {
                            ld_blocks.push(body);
                        }
                    } else if tag.has("src") && body.trim().is_empty() {
                        // An external script: not fetched, not run.
                    } else if script_budget > 0 {
                        let mut end = body.len().min(script_budget);
                        while !body.is_char_boundary(end) {
                            end -= 1;
                        }
                        let chunk = &body[..end];
                        script_budget -= chunk.len();
                        let room = MAX_CANDIDATES.saturating_sub(script_cands.len());
                        scripts::scan_script(chunk, &mut script_cands, room);
                        facts.add_script(chunk);
                    }
                }
                "title" => {
                    if pm.html_title.is_none() {
                        pm.html_title = sanitize_title(&html::decode(body));
                    }
                }
                "noscript" => facts.noscript = true,
                _ => {}
            },
            Tok::Start(tag) => {
                // A start tag right after the root marker: it has content.
                if let Some(id) = tag.attr("id")
                    && matches!(id, "app" | "root" | "__next")
                {
                    root_pending = true;
                }
                match tag.name.as_str() {
                    "base" => {
                        if base_href.is_none() {
                            base_href = tag.attr("href").and_then(plausible);
                        }
                    }
                    "meta" => meta_tag(&tag, &mut pm, &mut facts),
                    "link" => {
                        let rel = tag.attr("rel").unwrap_or("").to_ascii_lowercase();
                        if rel.split_whitespace().any(|r| r == "alternate")
                            && let Some(p) = tag.attr("type").and_then(proto_from_type)
                            && let Some(h) = tag.attr("href")
                        {
                            push(&mut page_cands, h, None, Some(p), Origin::LinkAlternate);
                        }
                    }
                    "iframe" => {
                        if let Some(u) = tag.attr("src").and_then(plausible) {
                            iframes.push(u);
                        }
                    }
                    "video" => {
                        let preview = tag.has("muted") && tag.has("loop") && !tag.has("controls");
                        in_video = (!tag.self_closing).then_some(preview);
                        if let Some(p) = tag.attr("poster").and_then(plausible) {
                            poster.get_or_insert(p);
                        }
                        if let Some(src) = tag.attr("src") {
                            let o = if preview {
                                Origin::Preview
                            } else {
                                Origin::VideoTag
                            };
                            push(&mut page_cands, src, None, None, o);
                        }
                    }
                    "source" => {
                        if let Some(preview) = in_video
                            && let Some(src) = tag.attr("src")
                        {
                            let ty = tag.attr("type").unwrap_or("");
                            let l = ty.to_ascii_lowercase();
                            if !l.starts_with("audio/") && !l.starts_with("image/") {
                                let h = ["res", "data-res", "height", "size", "label", "title"]
                                    .iter()
                                    .find_map(|k| tag.attr(k).and_then(scripts::parse_height));
                                let o = if preview {
                                    Origin::Preview
                                } else {
                                    Origin::SourceTag
                                };
                                push(&mut page_cands, src, h, proto_from_type(ty), o);
                            }
                        }
                    }
                    "form" | "button" => facts.form_or_button = true,
                    "input" => {
                        let ty = tag.attr("type").unwrap_or("").to_ascii_lowercase();
                        match ty.as_str() {
                            "password" => facts.password = true,
                            "submit" | "button" => facts.form_or_button = true,
                            _ => {}
                        }
                        if let Some(v) = tag.attr("value") {
                            facts.add_text(v);
                        }
                    }
                    _ => {}
                }
                for k in ["title", "alt", "placeholder", "aria-label"] {
                    if let Some(v) = tag.attr(k) {
                        facts.add_text(v);
                    }
                }
            }
        }
    }
    if root_pending {
        facts.empty_root = true;
    }

    // JSON-LD.
    let mut ld: Vec<LdVideo> = Vec::new();
    let mut nodes = 0;
    let mut embeds = Vec::new();
    for block in ld_blocks {
        if let Ok(v) = serde_json::from_str::<Value>(block) {
            walk_ld(&v, 0, &mut nodes, &mut ld, &mut embeds);
        }
    }
    iframes.extend(embeds);

    // Open Graph and Twitter.
    let og_hint = pm.og_type.as_deref();
    let embed_type = og_hint.is_some_and(|t| {
        let l = t.to_ascii_lowercase();
        l.contains("html") || l.contains("shockwave")
    });
    let og_proto = og_hint.and_then(proto_from_type);
    for u in std::mem::take(&mut pm.og_urls) {
        if embed_type {
            if let Some(p) = plausible(&u) {
                iframes.push(p);
            }
        } else {
            push(
                &mut page_cands,
                &u,
                pm.og_height,
                og_proto,
                Origin::OpenGraph,
            );
        }
    }
    if let Some(u) = pm.tw_stream.take() {
        push(&mut page_cands, &u, None, None, Origin::Twitter);
    }
    if let Some(u) = pm.prop_content.take() {
        push(&mut page_cands, &u, None, None, Origin::OpenGraph);
    }
    page_cands.extend(script_cands);

    // Page-level metadata. Title order: og:title, JSON-LD name, twitter:title, <title>.
    let ld_with: Vec<&LdVideo> = ld.iter().filter(|v| v.content.is_some()).collect();
    let first_ld = ld.first();
    let page_meta = Meta {
        title: pm
            .og_title
            .take()
            .or_else(|| first_ld.and_then(|v| v.name.clone()))
            .or_else(|| pm.tw_title.take())
            .or_else(|| pm.html_title.take()),
        duration_secs: pm.duration.or_else(|| first_ld.and_then(|v| v.duration)),
        thumbnail: pm
            .thumb
            .take()
            .or_else(|| first_ld.and_then(|v| v.thumb.clone()))
            .or(poster),
    };

    let ld_cand = |v: &LdVideo| RawCandidate {
        url: v.content.clone().unwrap_or_default(),
        height: v.height,
        bitrate: v.bitrate,
        proto: scripts::proto_of(v.content.as_deref().unwrap_or("")),
        origin: Origin::JsonLd,
    };

    let mut items: Vec<RawItem> = Vec::new();
    if ld_with.len() >= 2 {
        for v in &ld_with {
            items.push(RawItem {
                meta: Meta {
                    title: v.name.clone(),
                    duration_secs: v.duration,
                    thumbnail: v.thumb.clone(),
                },
                candidates: vec![ld_cand(v)],
            });
        }
        page_cands.retain(|c| !ld_with.iter().any(|v| v.content.as_deref() == Some(&c.url)));
        if !page_cands.is_empty() {
            items.push(RawItem {
                meta: page_meta,
                candidates: page_cands,
            });
        }
    } else {
        if let Some(v) = ld_with.first() {
            page_cands.push(ld_cand(v));
        }
        items.push(RawItem {
            meta: page_meta,
            candidates: page_cands,
        });
    }

    // Order, dedupe and cap the candidates; keep the cap across items.
    let mut room = MAX_CANDIDATES;
    for it in &mut items {
        it.candidates.sort_by_key(|c| c.origin);
        let mut seen = std::collections::HashSet::new();
        it.candidates.retain(|c| seen.insert(c.url.clone()));
        it.candidates.truncate(room);
        room -= it.candidates.len();
    }
    if items.is_empty() {
        items.push(RawItem::default());
    }

    let mut seen = std::collections::HashSet::new();
    iframes.retain(|u| seen.insert(u.clone()));
    iframes.truncate(MAX_IFRAMES);

    let sig = facts.evaluate();
    PageScan {
        items,
        iframes,
        base_href,
        signals: sig,
    }
}

fn meta_tag(tag: &Tag, pm: &mut PageMeta, facts: &mut signals::Facts) {
    if tag
        .attr("http-equiv")
        .is_some_and(|v| v.eq_ignore_ascii_case("refresh"))
    {
        facts.meta_refresh = true;
    }
    let key = tag
        .attr("property")
        .or_else(|| tag.attr("name"))
        .or_else(|| tag.attr("itemprop"))
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(content) = tag.attr("content") else {
        return;
    };
    if content.len() > MAX_URL * 2 {
        return;
    }
    match key.as_str() {
        "og:video" | "og:video:url" | "og:video:secure_url" => pm.og_urls.push(content.to_owned()),
        "og:video:height" => pm.og_height = scripts::parse_height(content),
        "og:video:type" => pm.og_type = Some(content.to_owned()),
        "og:title" => pm.og_title = pm.og_title.take().or_else(|| sanitize_title(content)),
        "og:image" | "thumbnailurl" => {
            if pm.thumb.is_none() {
                pm.thumb = plausible(content);
            }
        }
        "video:duration" | "og:video:duration" | "duration" => {
            pm.duration = pm.duration.or_else(|| parse_duration(content));
        }
        "twitter:player:stream" => pm.tw_stream = Some(content.to_owned()),
        "twitter:title" => pm.tw_title = pm.tw_title.take().or_else(|| sanitize_title(content)),
        "contenturl" => {
            pm.prop_content = pm.prop_content.take().or_else(|| Some(content.to_owned()))
        }
        _ => {}
    }
    if key.starts_with("description") || key == "og:description" {
        facts.add_text(content);
    }
}

#[cfg(test)]
mod tests;
