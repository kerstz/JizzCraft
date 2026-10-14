//! MPEG-DASH manifest parser (static, single period). Pure: bytes in,
//! structure out, no I/O.
//!
//! Output URLs are relative references already combined along the `BaseURL`
//! chain; the caller resolves them against the manifest address. Anything
//! protected, live or multi-period is refused with a fixed reason.

use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use std::time::Duration;

pub const MAX_TEXT: usize = 4 << 20;
pub const MAX_SEGMENTS: usize = 50_000;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 32;

pub type Range = (u64, u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SegSpec {
    List {
        init: Option<(String, Option<Range>)>,
        media: Vec<(String, Option<Range>, Duration)>,
    },
    /// One file with a `sidx` index at `index_range` (inclusive bytes).
    Base {
        url: String,
        index_range: Range,
        init_range: Option<Range>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Representation {
    pub id: String,
    pub bandwidth: u64,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    pub segs: SegSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdaptationSet {
    pub kind: Kind,
    pub lang: Option<String>,
    pub reps: Vec<Representation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mpd {
    pub duration: Option<Duration>,
    pub sets: Vec<AdaptationSet>,
}

// ---- tiny DOM -------------------------------------------------------------

#[derive(Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    kids: Vec<Node>,
    text: String,
}

impl Node {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
    fn kids<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.kids.iter().filter(move |k| k.name == name)
    }
    fn kid(&self, name: &str) -> Option<&Node> {
        self.kids.iter().find(|k| k.name == name)
    }
    fn num<T: std::str::FromStr>(&self, k: &str) -> Option<T> {
        self.attr(k).and_then(|v| v.trim().parse().ok())
    }
}

fn local(n: &[u8]) -> String {
    let s = String::from_utf8_lossy(n);
    s.rsplit(':').next().unwrap_or("").to_owned()
}

fn node_of(e: &BytesStart<'_>) -> Node {
    Node {
        name: local(e.name().as_ref()),
        attrs: e
            .attributes()
            .flatten()
            .filter_map(|a| {
                let v = a.normalized_value(XmlVersion::Implicit1_0).ok()?;
                Some((local(a.key.as_ref()), v.into_owned()))
            })
            .collect(),
        ..Default::default()
    }
}

fn dom(bytes: &[u8]) -> Result<Node, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "no media")?;
    let mut r = Reader::from_str(text);
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut count = 0usize;
    loop {
        match r.read_event().map_err(|_| "no media")? {
            Event::Start(e) => {
                count += 1;
                if count > MAX_NODES || stack.len() >= MAX_DEPTH {
                    return Err("too large");
                }
                stack.push(node_of(&e));
            }
            Event::Empty(e) => {
                count += 1;
                if count > MAX_NODES {
                    return Err("too large");
                }
                let n = node_of(&e);
                match stack.last_mut() {
                    Some(p) => p.kids.push(n),
                    None => root = Some(n),
                }
            }
            Event::End(_) => {
                let n = stack.pop().ok_or("no media")?;
                match stack.last_mut() {
                    Some(p) => p.kids.push(n),
                    None => root = Some(n),
                }
            }
            Event::Text(t) => {
                if let (Some(top), Ok(v)) = (stack.last_mut(), t.decode()) {
                    top.text.push_str(&v);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.filter(|n| n.name == "MPD").ok_or("no media")
}

// ---- helpers --------------------------------------------------------------

/// `PT1H2M3.5S`, `P1DT2S` -> seconds.
fn iso_duration(s: &str) -> Option<Duration> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut secs = 0f64;
    let mut num = String::new();
    for c in date.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'D' => {
                secs += num.parse::<f64>().ok()? * 86400.0;
                num.clear();
            }
            // Years and months have no fixed length: refuse rather than guess.
            _ => return None,
        }
    }
    for c in time.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'H' | 'M' | 'S' => {
                let v: f64 = num.parse().ok()?;
                secs += v * match c {
                    'H' => 3600.0,
                    'M' => 60.0,
                    _ => 1.0,
                };
                num.clear();
            }
            _ => return None,
        }
    }
    (num.is_empty() && secs.is_finite() && (0.0..1e7).contains(&secs))
        .then(|| Duration::from_secs_f64(secs))
}

fn parse_range(s: &str) -> Option<Range> {
    let (a, b) = s.split_once('-')?;
    let (a, b): (u64, u64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a <= b).then_some((a, b))
}

fn join(parent: &str, child: &str) -> String {
    if child.contains("://") || child.starts_with('/') || parent.is_empty() {
        return child.to_owned();
    }
    let dir = parent.rfind('/').map_or("", |i| &parent[..=i]);
    format!("{dir}{child}")
}

fn base_of(n: &Node, parent: &str) -> String {
    match n.kid("BaseURL") {
        Some(b) => join(parent, b.text.trim()),
        None => parent.to_owned(),
    }
}

fn height_from(rep: &Node, set: &Node) -> Option<u32> {
    rep.num("height").or_else(|| set.num("height"))
}

fn kind_of(set: &Node, rep: Option<&Node>) -> Kind {
    let s = [
        set.attr("contentType"),
        set.attr("mimeType"),
        rep.and_then(|r| r.attr("mimeType")),
        rep.and_then(|r| r.attr("contentType")),
    ];
    for v in s.into_iter().flatten() {
        if v.starts_with("video") {
            return Kind::Video;
        }
        if v.starts_with("audio") {
            return Kind::Audio;
        }
    }
    Kind::Other
}

/// Expand `$Number$`, `$Time$`, `$RepresentationID$`, `$Bandwidth$`, with
/// optional `%0Nd` padding and `$$`.
fn expand(tpl: &str, id: &str, bw: u64, number: u64, time: u64) -> String {
    let mut out = String::new();
    let mut parts = tpl.split('$');
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    let mut inside = true;
    for p in parts {
        if inside {
            if p.is_empty() {
                out.push('$');
            } else {
                let (name, fmt) = p.split_once('%').map_or((p, None), |(n, f)| (n, Some(f)));
                let val = match name {
                    "Number" => Some(number),
                    "Time" => Some(time),
                    "Bandwidth" => Some(bw),
                    "RepresentationID" => None,
                    _ => {
                        out.push('$');
                        out.push_str(p);
                        out.push('$');
                        inside = !inside;
                        continue;
                    }
                };
                match val {
                    None => out.push_str(id),
                    Some(v) => {
                        let width = fmt
                            .and_then(|f| f.strip_prefix('0'))
                            .and_then(|f| f.strip_suffix('d'))
                            .and_then(|w| w.parse::<usize>().ok())
                            .unwrap_or(0);
                        out.push_str(&format!("{v:0width$}"));
                    }
                }
            }
        } else {
            out.push_str(p);
        }
        inside = !inside;
    }
    out
}

struct Ctx {
    total: Option<Duration>,
    count: usize,
}

impl Ctx {
    fn add(&mut self, n: usize) -> Result<(), &'static str> {
        self.count += n;
        if self.count > MAX_SEGMENTS {
            return Err("too large");
        }
        Ok(())
    }
}

/// Timeline entries as (start time, duration) in timescale units.
fn timeline(
    tl: &Node,
    total_units: Option<u64>,
    ctx: &mut Ctx,
) -> Result<Vec<(u64, u64)>, &'static str> {
    let mut out = Vec::new();
    let mut t = 0u64;
    for s in tl.kids("S") {
        let d: u64 = s.num("d").ok_or("no media")?;
        if d == 0 {
            return Err("no media");
        }
        if let Some(st) = s.num::<u64>("t") {
            t = st;
        }
        let r: i64 = s.num("r").unwrap_or(0);
        let reps = if r < 0 {
            let end = total_units.ok_or("unsupported stream")?;
            end.saturating_sub(t).div_ceil(d).max(1)
        } else {
            r as u64 + 1
        };
        ctx.add(reps.min(MAX_SEGMENTS as u64 + 1) as usize)?;
        for _ in 0..reps {
            out.push((t, d));
            t = t.saturating_add(d);
        }
    }
    Ok(out)
}

fn secs(units: u64, scale: u64) -> Duration {
    Duration::from_secs_f64(units as f64 / scale.max(1) as f64)
}

fn representation(
    rep: &Node,
    set: &Node,
    base: &str,
    ctx: &mut Ctx,
) -> Result<Option<Representation>, &'static str> {
    let id = rep.attr("id").unwrap_or("").to_owned();
    let bandwidth: u64 = rep.num("bandwidth").unwrap_or(0);
    let base = base_of(rep, base);
    let mk = |segs| Representation {
        id: id.clone(),
        bandwidth,
        height: height_from(rep, set),
        codecs: rep
            .attr("codecs")
            .or_else(|| set.attr("codecs"))
            .map(str::to_owned),
        segs,
    };

    // SegmentTemplate: attributes merge, the representation wins.
    let tpls: Vec<&Node> = [rep.kid("SegmentTemplate"), set.kid("SegmentTemplate")]
        .into_iter()
        .flatten()
        .collect();
    if !tpls.is_empty() {
        let get = |k: &str| tpls.iter().find_map(|t| t.attr(k));
        let media = get("media").ok_or("unsupported stream")?;
        let scale: u64 = get("timescale")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1)
            .max(1);
        let start: u64 = get("startNumber").and_then(|v| v.parse().ok()).unwrap_or(1);
        let pto: u64 = get("presentationTimeOffset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let total_units = ctx
            .total
            .map(|t| (t.as_secs_f64() * scale as f64) as u64 + pto);
        let init =
            get("initialization").map(|i| (join(&base, &expand(i, &id, bandwidth, 0, 0)), None));
        let tl = tpls.iter().find_map(|t| t.kid("SegmentTimeline"));
        let mut list = Vec::new();
        if let Some(tl) = tl {
            let entries = timeline(tl, total_units, ctx)?;
            for (i, (t, d)) in entries.into_iter().enumerate() {
                let url = expand(media, &id, bandwidth, start + i as u64, t);
                list.push((join(&base, &url), None, secs(d, scale)));
            }
        } else {
            let d: u64 = get("duration")
                .and_then(|v| v.parse().ok())
                .filter(|d| *d > 0)
                .ok_or("unsupported stream")?;
            let total = ctx.total.ok_or("unsupported stream")?;
            let n = (total.as_secs_f64() * scale as f64 / d as f64).ceil() as u64;
            ctx.add(n.min(MAX_SEGMENTS as u64 + 1) as usize)?;
            let end_number: Option<u64> = get("endNumber").and_then(|v| v.parse().ok());
            for i in 0..n {
                let number = start + i;
                if end_number.is_some_and(|e| number > e) {
                    break;
                }
                let url = expand(media, &id, bandwidth, number, pto + i * d);
                list.push((join(&base, &url), None, secs(d, scale)));
            }
        }
        if list.is_empty() {
            return Ok(None);
        }
        return Ok(Some(mk(SegSpec::List { init, media: list })));
    }

    // SegmentList
    if let Some(sl) = rep.kid("SegmentList").or_else(|| set.kid("SegmentList")) {
        let scale: u64 = sl.num("timescale").unwrap_or(1);
        let dur: u64 = sl.num("duration").unwrap_or(0);
        let init = sl.kid("Initialization").map(|i| {
            (
                join(&base, i.attr("sourceURL").unwrap_or(&base)),
                i.attr("range").and_then(parse_range),
            )
        });
        let tl = sl
            .kid("SegmentTimeline")
            .map(|t| timeline(t, None, ctx))
            .transpose()?;
        let urls: Vec<&Node> = sl.kids("SegmentURL").collect();
        ctx.add(urls.len())?;
        let mut list = Vec::new();
        for (i, u) in urls.iter().enumerate() {
            let d = match &tl {
                Some(t) => t.get(i).map_or(0, |e| e.1),
                None => dur,
            };
            if d == 0 {
                return Err("unsupported stream");
            }
            let url = join(&base, u.attr("media").unwrap_or(&base));
            list.push((
                url,
                u.attr("mediaRange").and_then(parse_range),
                secs(d, scale),
            ));
        }
        if list.is_empty() {
            return Ok(None);
        }
        return Ok(Some(mk(SegSpec::List { init, media: list })));
    }

    // SegmentBase with a sidx index.
    if let Some(sb) = rep.kid("SegmentBase").or_else(|| set.kid("SegmentBase")) {
        let Some(ir) = sb.attr("indexRange").and_then(parse_range) else {
            return Ok(None);
        };
        if base.is_empty() {
            return Ok(None);
        }
        let init_range = sb
            .kid("Initialization")
            .and_then(|i| i.attr("range"))
            .and_then(parse_range);
        return Ok(Some(mk(SegSpec::Base {
            url: base,
            index_range: ir,
            init_range,
        })));
    }
    Ok(None)
}

fn has_protection(n: &Node) -> bool {
    n.name == "ContentProtection" || n.kids.iter().any(has_protection)
}

pub fn parse(bytes: &[u8]) -> Result<Mpd, &'static str> {
    if bytes.len() > MAX_TEXT {
        return Err("too large");
    }
    let root = dom(bytes)?;
    if has_protection(&root) {
        return Err("protected");
    }
    if root.attr("type").is_some_and(|t| t == "dynamic") {
        return Err("unsupported stream");
    }
    let periods: Vec<&Node> = root.kids("Period").collect();
    if periods.len() != 1 {
        return Err(if periods.is_empty() {
            "no media"
        } else {
            "unsupported stream"
        });
    }
    let period = periods[0];
    let duration = period.attr("duration").and_then(iso_duration).or_else(|| {
        root.attr("mediaPresentationDuration")
            .and_then(iso_duration)
    });
    let mut ctx = Ctx {
        total: duration,
        count: 0,
    };
    let root_base = base_of(&root, "");
    let period_base = base_of(period, &root_base);
    let mut sets = Vec::new();
    for set in period.kids("AdaptationSet") {
        let set_base = base_of(set, &period_base);
        let mut reps = Vec::new();
        for rep in set.kids("Representation") {
            if let Some(r) = representation(rep, set, &set_base, &mut ctx)? {
                reps.push(r);
            }
        }
        if reps.is_empty() {
            continue;
        }
        let kind = kind_of(set, set.kid("Representation"));
        sets.push(AdaptationSet {
            kind,
            lang: set.attr("lang").map(str::to_owned),
            reps,
        });
    }
    if sets.is_empty() {
        return Err("no media");
    }
    Ok(Mpd { duration, sets })
}

/// Parse a `sidx` box found in `bytes`. `anchor` is the absolute file offset
/// of the first byte after the box (`index_range.1 + 1`). Returns the byte
/// range (inclusive, absolute) and duration of each media subsegment.
pub fn parse_sidx(bytes: &[u8], anchor: u64) -> Vec<((u64, u64), Duration)> {
    let be32 = |o: usize| -> Option<u32> {
        bytes
            .get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let be64 = |o: usize| -> Option<u64> { Some(((be32(o)? as u64) << 32) | be32(o + 4)? as u64) };
    // Walk top-level boxes to find the sidx.
    let mut pos = 0usize;
    let body = loop {
        let Some(size) = be32(pos) else {
            return Vec::new();
        };
        let Some(kind) = bytes.get(pos + 4..pos + 8) else {
            return Vec::new();
        };
        let size = if size == 1 {
            match be64(pos + 8) {
                Some(s) => s as usize,
                None => return Vec::new(),
            }
        } else {
            size as usize
        };
        if kind == b"sidx" {
            let hdr = if be32(pos) == Some(1) { 16 } else { 8 };
            break pos + hdr;
        }
        if size < 8 {
            return Vec::new();
        }
        pos += size;
    };
    let Some(vf) = be32(body) else {
        return Vec::new();
    };
    let version = (vf >> 24) as u8;
    let Some(timescale) = be32(body + 8) else {
        return Vec::new();
    };
    if timescale == 0 {
        return Vec::new();
    }
    let (first_offset, mut p) = if version == 0 {
        (be32(body + 16).map(u64::from), body + 20)
    } else {
        (be64(body + 20), body + 28)
    };
    let Some(first_offset) = first_offset else {
        return Vec::new();
    };
    let Some(count) = bytes
        .get(p + 2..p + 4)
        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    else {
        return Vec::new();
    };
    p += 4;
    let mut out = Vec::new();
    let mut off = anchor.saturating_add(first_offset);
    for i in 0..count {
        let (Some(r), Some(d)) = (be32(p + i * 12), be32(p + i * 12 + 4)) else {
            break;
        };
        let size = u64::from(r & 0x7fff_ffff);
        if r >> 31 == 1 || size == 0 {
            // Hierarchical index: not supported, give up cleanly.
            return Vec::new();
        }
        out.push((
            (off, off + size - 1),
            Duration::from_secs_f64(f64::from(d) / f64::from(timescale)),
        ));
        off += size;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpd(period_attrs: &str, body: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" \
mediaPresentationDuration=\"PT20S\"><BaseURL>https://cdn.example.invalid/v/</BaseURL>\
<Period {period_attrs}>{body}</Period></MPD>"
        )
    }

    #[test]
    fn template_number_with_padding_and_inheritance() {
        let x = mpd(
            "",
            r#"<AdaptationSet contentType="video" mimeType="video/mp4">
<SegmentTemplate timescale="1000" duration="4000" startNumber="3" initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/s$Number%05d$.m4s"/>
<Representation id="v1" bandwidth="500000" height="360" codecs="avc1.42c01e"/>
</AdaptationSet>
<AdaptationSet contentType="audio"><Representation id="a1" bandwidth="64000">
<SegmentTemplate timescale="1" duration="10" media="a/$Bandwidth$-$Time$.m4s"/></Representation></AdaptationSet>"#,
        );
        let m = parse(x.as_bytes()).unwrap();
        assert_eq!(m.duration, Some(Duration::from_secs(20)));
        assert_eq!(m.sets.len(), 2);
        assert_eq!(m.sets[0].kind, Kind::Video);
        let r = &m.sets[0].reps[0];
        assert_eq!((r.height, r.bandwidth), (Some(360), 500_000));
        let SegSpec::List { init, media } = &r.segs else {
            panic!()
        };
        assert_eq!(
            init.as_ref().unwrap().0,
            "https://cdn.example.invalid/v/v1/init.mp4"
        );
        assert_eq!(media.len(), 5);
        assert_eq!(media[0].0, "https://cdn.example.invalid/v/v1/s00003.m4s");
        assert_eq!(media[0].2, Duration::from_secs(4));
        assert_eq!(m.sets[1].kind, Kind::Audio);
        let SegSpec::List { media, .. } = &m.sets[1].reps[0].segs else {
            panic!()
        };
        assert_eq!(media.len(), 2);
        assert_eq!(media[1].0, "https://cdn.example.invalid/v/a/64000-10.m4s");
    }

    #[test]
    fn timeline_with_repeat_and_open_ended_repeat() {
        let x = mpd(
            "",
            r#"<AdaptationSet mimeType="video/mp4"><Representation id="r" bandwidth="1">
<SegmentTemplate timescale="1" media="$Time$.m4s" initialization="i.mp4"><SegmentTimeline>
<S t="0" d="5" r="1"/><S d="10" r="-1"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet>"#,
        );
        let m = parse(x.as_bytes()).unwrap();
        let SegSpec::List { media, .. } = &m.sets[0].reps[0].segs else {
            panic!()
        };
        let urls: Vec<&str> = media.iter().map(|m| m.0.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://cdn.example.invalid/v/0.m4s",
                "https://cdn.example.invalid/v/5.m4s",
                "https://cdn.example.invalid/v/10.m4s"
            ]
        );
        assert_eq!(media[2].2, Duration::from_secs(10));
    }

    #[test]
    fn segment_list_and_base_url_override() {
        let x = mpd(
            "",
            r#"<AdaptationSet mimeType="video/mp4"><Representation id="r" bandwidth="1" height="480">
<BaseURL>https://other.example.invalid/x/</BaseURL>
<SegmentList timescale="1" duration="6"><Initialization sourceURL="init.mp4" range="0-99"/>
<SegmentURL media="a.m4s"/><SegmentURL media="b.m4s" mediaRange="100-199"/></SegmentList>
</Representation></AdaptationSet>"#,
        );
        let m = parse(x.as_bytes()).unwrap();
        let SegSpec::List { init, media } = &m.sets[0].reps[0].segs else {
            panic!()
        };
        let init = init.as_ref().unwrap();
        assert_eq!(init.0, "https://other.example.invalid/x/init.mp4");
        assert_eq!(init.1, Some((0, 99)));
        assert_eq!(media[0].0, "https://other.example.invalid/x/a.m4s");
        assert_eq!(media[1].1, Some((100, 199)));
        assert_eq!(media[1].2, Duration::from_secs(6));
    }

    #[test]
    fn segment_base_with_index_range() {
        let x = mpd(
            "",
            r#"<AdaptationSet mimeType="video/mp4"><Representation id="r" bandwidth="1">
<BaseURL>file.mp4</BaseURL><SegmentBase indexRange="800-1199"><Initialization range="0-799"/></SegmentBase>
</Representation></AdaptationSet>"#,
        );
        let m = parse(x.as_bytes()).unwrap();
        assert_eq!(
            m.sets[0].reps[0].segs,
            SegSpec::Base {
                url: "https://cdn.example.invalid/v/file.mp4".into(),
                index_range: (800, 1199),
                init_range: Some((0, 799)),
            }
        );
    }

    #[test]
    fn refusals() {
        let drm = mpd(
            "",
            r#"<AdaptationSet mimeType="video/mp4"><ContentProtection schemeIdUri="urn:x"/>
<Representation id="r" bandwidth="1"><SegmentTemplate duration="1" media="x"/></Representation></AdaptationSet>"#,
        );
        assert_eq!(parse(drm.as_bytes()), Err("protected"));
        let live = "<MPD type=\"dynamic\"><Period/></MPD>";
        assert_eq!(parse(live.as_bytes()), Err("unsupported stream"));
        let two = "<MPD type=\"static\"><Period/><Period/></MPD>";
        assert_eq!(parse(two.as_bytes()), Err("unsupported stream"));
        assert_eq!(parse(b"not xml at all"), Err("no media"));
        assert_eq!(parse(b"<html/>"), Err("no media"));
    }

    #[test]
    fn caps() {
        let x = String::from(
            "<MPD type=\"static\" mediaPresentationDuration=\"PT100000S\"><Period>\
<AdaptationSet mimeType=\"video/mp4\"><Representation id=\"r\" bandwidth=\"1\">\
<SegmentTemplate timescale=\"1\" duration=\"1\" media=\"$Number$\"/></Representation></AdaptationSet></Period></MPD>",
        );
        assert_eq!(parse(x.as_bytes()), Err("too large"));
        let big = vec![b' '; MAX_TEXT + 1];
        assert_eq!(parse(&big), Err("too large"));
    }

    #[test]
    fn iso_durations() {
        assert_eq!(
            iso_duration("PT1H2M3.5S"),
            Some(Duration::from_millis(3_723_500))
        );
        assert_eq!(iso_duration("P1DT1S"), Some(Duration::from_secs(86_401)));
        assert_eq!(iso_duration("P1Y"), None);
        assert_eq!(iso_duration("junk"), None);
    }

    fn sidx(version: u8, timescale: u32, first_offset: u32, refs: &[(u32, u32)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(b"sidx");
        b.extend_from_slice(&[version, 0, 0, 0]);
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&timescale.to_be_bytes());
        if version == 0 {
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&first_offset.to_be_bytes());
        } else {
            b.extend_from_slice(&0u64.to_be_bytes());
            b.extend_from_slice(&u64::from(first_offset).to_be_bytes());
        }
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&(refs.len() as u16).to_be_bytes());
        for (size, dur) in refs {
            b.extend_from_slice(&size.to_be_bytes());
            b.extend_from_slice(&dur.to_be_bytes());
            b.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let n = b.len() as u32;
        b[..4].copy_from_slice(&n.to_be_bytes());
        b
    }

    #[test]
    fn sidx_ranges_for_both_versions() {
        for v in [0u8, 1] {
            let b = sidx(v, 1000, 0, &[(500, 2000), (300, 1500)]);
            let anchor = 1200;
            let out = parse_sidx(&b, anchor);
            assert_eq!(out.len(), 2);
            assert_eq!(out[0], ((1200, 1699), Duration::from_secs(2)));
            assert_eq!(out[1], ((1700, 1999), Duration::from_millis(1500)));
        }
        let b = sidx(0, 1000, 10, &[(5, 1000)]);
        assert_eq!(parse_sidx(&b, 100)[0].0, (110, 114));
        assert!(parse_sidx(&b[..12], 0).is_empty());
        assert!(parse_sidx(b"", 0).is_empty());
    }
}
