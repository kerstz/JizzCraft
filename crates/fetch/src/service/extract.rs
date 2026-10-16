//! The built-in page reader: a generic reader for standard markup, and the
//! glue that tries it next to the user's resolver tools.
//!
//! Rules, in one place. GET only; no cookie is stored or sent; no form is
//! submitted, no age gate clicked, no meta refresh followed, no script run.
//! The only request headers are `User-Agent`, `Accept` and, for an embedded
//! player, `Referer`. Every request path asks `proxy_blocked()`; every address
//! taken from a page goes through `resolve_candidate` (which applies
//! `inward_ok`). A page is read up to a size cap and a time limit, and what
//! arrived by then is what is scanned. Errors are fixed keywords.

use super::open::{fault_rank, open_direct};
use super::{
    FetchService, SEARCH_TIMEOUT, StreamOpts, StreamTicket, current_proxy, proxy_blocked,
    same_origin_policy,
};
use crate::config::{ExtractorMode, FetchConfig};
use crate::extract::resolve::resolve_candidate;
use crate::extract::{
    self, MAX_CANDIDATES, MAX_IFRAMES, MAX_PAGE, PageScan, Proto, RawItem, Signals, rules,
};
use crate::formats::{Entry, Fault, Format, Meta, muxed, rank};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Total time one page may take to arrive. What came by then is scanned.
const DEFAULT_PAGE_DEADLINE: Duration = Duration::from_secs(30);
static PAGE_DEADLINE_MS: AtomicU64 = AtomicU64::new(30_000);

/// Tests only: shorten the page deadline. `None` restores the default.
#[doc(hidden)]
pub fn set_page_deadline(d: Option<Duration>) {
    PAGE_DEADLINE_MS.store(
        d.unwrap_or(DEFAULT_PAGE_DEADLINE).as_millis() as u64,
        Ordering::SeqCst,
    );
}

/// Page reads at once.
static PAGE_READS: Semaphore = Semaphore::const_new(2);

/// How an extraction got its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The address itself is a media file or a manifest.
    Direct,
    /// The built-in reader; `embed` when the video sat in an embedded player.
    Builtin { embed: bool },
    /// The user's resolver with this id.
    Resolver(u32),
}

/// What a page holds. Memory only: the addresses in it are never logged.
#[derive(Clone)]
pub struct Extraction {
    pub via: Via,
    /// The address that was asked about.
    pub page: String,
    /// At least one, each with at least one format, best first.
    pub entries: Vec<Entry>,
}

impl std::fmt::Debug for Extraction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extraction")
            .field("via", &self.via)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractMode {
    /// As the `extractor` setting says.
    Auto,
    Builtin,
    Resolvers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reader {
    Builtin,
    Resolvers,
}

pub(super) fn reader_order(m: ExtractorMode) -> &'static [Reader] {
    match m {
        ExtractorMode::First => &[Reader::Builtin, Reader::Resolvers],
        ExtractorMode::Fallback => &[Reader::Resolvers, Reader::Builtin],
        ExtractorMode::Off => &[Reader::Resolvers],
    }
}

impl FetchService {
    /// Read the page at `url` and list the videos in it. Cancelled by
    /// `cancel_streams`.
    pub fn extract(&self, url: String, mode: ExtractMode) -> Receiver<Result<Extraction, String>> {
        let cfg = self.cfg.clone();
        self.spawn_open(extract_task(cfg, url, mode))
    }
}

enum Fetched {
    Media(Proto),
    Page(reqwest::Url, Vec<u8>),
}

fn status_error(code: u16) -> String {
    match code {
        401 => Fault::Login.keyword().into(),
        403 => "forbidden".into(),
        404 | 410 => Fault::Gone.keyword().into(),
        429 => Fault::RateLimit.keyword().into(),
        451 => Fault::Geo.keyword().into(),
        503 => format!("server busy (HTTP {code})"),
        300..=399 => format!("redirect refused (HTTP {code})"),
        c => format!("HTTP {c}"),
    }
}

/// GET one page (or sniff it: with `probe` only the first block is read).
async fn fetch_page(
    cfg: &FetchConfig,
    client: &reqwest::Client,
    url: &str,
    referer: Option<&str>,
    probe: bool,
) -> Result<Fetched, String> {
    use crate::sniff::{Sniff, sniff};
    if proxy_blocked() {
        return Err("proxy required".into());
    }
    let mut req = client
        .get(url)
        .header(reqwest::header::USER_AGENT, cfg.save.user_agent())
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml");
    if let Some(r) = referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    let mut resp = crate::stream::send_ttfb(req).await?;
    let code = resp.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(status_error(code));
    }
    let final_url = resp.url().clone();
    let deadline = Instant::now() + Duration::from_millis(PAGE_DEADLINE_MS.load(Ordering::SeqCst));
    let mut body: Vec<u8> = Vec::new();
    let mut checked = false;
    let kind = |b: &[u8]| match sniff(b) {
        Sniff::Hls => Some(Proto::Hls),
        Sniff::Dash => Some(Proto::Dash),
        Sniff::Media => Some(Proto::Http),
        Sniff::Other => None,
    };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || body.len() >= MAX_PAGE || (probe && checked) {
            break;
        }
        match tokio::time::timeout(left.min(crate::stream::STALL_TIMEOUT), resp.chunk()).await {
            // A stall or the deadline: what arrived is what gets read.
            Err(_) => {
                if body.is_empty() {
                    return Err("server timeout".into());
                }
                break;
            }
            Ok(Err(e)) => {
                if body.is_empty() {
                    return Err(crate::stream::net_error(&e));
                }
                break;
            }
            Ok(Ok(None)) => break,
            Ok(Ok(Some(c))) => {
                let room = MAX_PAGE - body.len();
                body.extend_from_slice(&c[..c.len().min(room)]);
                if !checked && body.len() >= 4096 {
                    checked = true;
                    if let Some(p) = kind(&body) {
                        return Ok(Fetched::Media(p));
                    }
                }
            }
        }
    }
    // The rest of the response is dropped with the connection.
    drop(resp);
    if !checked && let Some(p) = kind(&body) {
        return Ok(Fetched::Media(p));
    }
    Ok(Fetched::Page(final_url, body))
}

/// A page address as a `Referer`: no credentials, no fragment.
fn referer_of(u: &reqwest::Url) -> String {
    let mut r = u.clone();
    let _ = r.set_username("");
    let _ = r.set_password(None);
    r.set_fragment(None);
    r.into()
}

fn proto_name(p: Proto) -> &'static str {
    match p {
        Proto::Http => "http",
        Proto::Hls => "hls",
        Proto::Dash => "dash",
    }
}

fn headers_for(ua: &str, referer: Option<&str>) -> Vec<(String, String)> {
    let mut h = vec![("User-Agent".to_owned(), ua.to_owned())];
    if let Some(r) = referer {
        h.push(("Referer".to_owned(), r.to_owned()));
    }
    h
}

/// Resolve and rank what a scan found. Candidates resolve against the
/// document's `<base href>` when that stays on the same host, else against
/// its own address.
fn entries_from(scan: &PageScan, doc: &reqwest::Url, ua: &str) -> Vec<Entry> {
    let doc_s = doc.as_str();
    let base = scan
        .base_href
        .as_deref()
        .and_then(|b| resolve_candidate(doc_s, doc_s, b))
        .filter(|b| {
            reqwest::Url::parse(b)
                .ok()
                .is_some_and(|u| u.host_str() == doc.host_str())
        })
        .unwrap_or_else(|| doc_s.to_owned());
    let referer = referer_of(doc);
    let mut out = Vec::new();
    for item in &scan.items {
        let mut seen = HashSet::new();
        let mut formats: Vec<Format> = item
            .candidates
            .iter()
            .filter_map(|c| {
                let u = resolve_candidate(&base, doc_s, &c.url)?;
                seen.insert(u.clone()).then(|| Format {
                    url: u,
                    height: c.height,
                    protocol: proto_name(c.proto).into(),
                    headers: headers_for(ua, Some(&referer)),
                    audio_only: false,
                    video_only: false,
                    bitrate: c.bitrate,
                })
            })
            .collect();
        if formats.is_empty() {
            continue;
        }
        rank(&mut formats);
        out.push(Entry {
            formats,
            meta: Meta {
                thumbnail: item
                    .meta
                    .thumbnail
                    .as_deref()
                    .and_then(|t| resolve_candidate(&base, doc_s, t)),
                ..item.meta.clone()
            },
        });
    }
    out
}

/// Run the user's rules that name this document's host. Their addresses join
/// the scan (ranked first, they are the user's own knowledge of the page);
/// they cause no request, and are resolved and checked like any other.
fn apply_rules(cfg: &FetchConfig, doc: &reqwest::Url, body: &[u8], scan: &mut PageScan) {
    let Some(host) = doc.host_str() else { return };
    let active: Vec<&rules::ExtractRule> = cfg
        .extract_rules
        .iter()
        .filter(|r| rules::applies(r, host))
        .collect();
    if active.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(body);
    let (mut cands, mut title) = (Vec::new(), None);
    for r in active {
        let (c, t) = rules::apply(r, &text);
        cands.extend(c);
        title = title.or(t);
    }
    cands.truncate(MAX_CANDIDATES);
    if cands.is_empty() {
        return;
    }
    if scan.items.len() == 1 {
        let it = &mut scan.items[0];
        it.candidates.extend(cands);
        it.candidates.sort_by_key(|c| c.origin);
        it.candidates.truncate(MAX_CANDIDATES);
        if title.is_some() {
            it.meta.title = title;
        }
    } else {
        let meta = Meta {
            title,
            ..Meta::default()
        };
        scan.items.insert(
            0,
            RawItem {
                meta,
                candidates: cands,
            },
        );
    }
}

/// Why a page gave nothing, most specific first.
fn decline(s: &Signals) -> String {
    if s.drm {
        Fault::Drm.keyword()
    } else if s.age_gate {
        Fault::AgeWall.keyword()
    } else if s.password_form {
        Fault::Login.keyword()
    } else if s.scripted {
        Fault::Scripted.keyword()
    } else {
        "no media"
    }
    .into()
}

fn direct_entry(url: &str, p: Proto, ua: &str) -> Entry {
    Entry {
        formats: vec![Format {
            url: url.to_owned(),
            height: None,
            protocol: proto_name(p).into(),
            headers: headers_for(ua, None),
            audio_only: false,
            video_only: false,
            bitrate: None,
        }],
        meta: Meta::default(),
    }
}

/// Read `page_url` with the built-in reader.
pub(super) async fn builtin(cfg: &FetchConfig, page_url: &str) -> Result<Extraction, String> {
    if !(page_url.starts_with("https://") || page_url.starts_with("http://")) {
        return Err("unsupported address".into());
    }
    if proxy_blocked() {
        return Err("proxy required".into());
    }
    let _permit = PAGE_READS
        .acquire()
        .await
        .map_err(|_| "cancelled".to_string())?;
    let client = FetchService::builder()?
        .connect_timeout(SEARCH_TIMEOUT)
        .redirect(same_origin_policy())
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let ua = cfg.save.user_agent();
    let (final_url, body) = match fetch_page(cfg, &client, page_url, None, false).await? {
        Fetched::Media(p) => {
            return Ok(Extraction {
                via: Via::Direct,
                page: page_url.to_owned(),
                entries: vec![direct_entry(page_url, p, ua)],
            });
        }
        Fetched::Page(u, b) => (u, b),
    };
    let mut scan = extract::scan(&body);
    apply_rules(cfg, &final_url, &body, &mut scan);
    drop(body);
    let entries = entries_from(&scan, &final_url, ua);
    if !entries.is_empty() {
        return Ok(Extraction {
            via: Via::Builtin { embed: false },
            page: page_url.to_owned(),
            entries,
        });
    }
    if cfg.save.follow_embeds {
        let referer = referer_of(&final_url);
        let parent_title = scan.items.first().and_then(|i| i.meta.title.clone());
        for raw in scan.iframes.iter().take(MAX_IFRAMES) {
            let Some(embed) = resolve_candidate(final_url.as_str(), final_url.as_str(), raw) else {
                continue;
            };
            if embed == final_url.as_str() {
                continue;
            }
            // One level only: an embed's own iframes are never read.
            let mut entries = match fetch_page(cfg, &client, &embed, Some(&referer), false).await {
                Ok(Fetched::Media(p)) => vec![direct_entry(&embed, p, ua)],
                Ok(Fetched::Page(u, b)) => entries_from(&extract::scan(&b), &u, ua),
                Err(e) if e == "proxy required" || e == "cancelled" => return Err(e),
                Err(_) => continue,
            };
            if entries.is_empty() {
                continue;
            }
            for e in &mut entries {
                if e.meta.title.is_none() {
                    e.meta.title.clone_from(&parent_title);
                }
            }
            return Ok(Extraction {
                via: Via::Builtin { embed: true },
                page: page_url.to_owned(),
                entries,
            });
        }
    }
    Err(decline(&scan.signals))
}

/// Ask each enabled resolver in turn; the first usable answer wins.
async fn resolvers(
    cfg: &FetchConfig,
    url: &str,
    blocked: &mut bool,
    best: &mut String,
) -> Option<Extraction> {
    for def in cfg.resolvers.iter().filter(|r| r.enabled) {
        if cfg.require_proxy && (current_proxy().is_none() || !def.proxy_aware()) {
            *blocked = true;
            continue;
        }
        let (d, p) = (def.clone(), url.to_owned());
        let r = tokio::task::spawn_blocking(move || crate::resolver::run_meta(&d, &p))
            .await
            .map_err(|_| "resolver failed".to_string())
            .and_then(|r| r)
            .and_then(|(f, meta)| {
                let mut formats = muxed(f)?;
                rank(&mut formats);
                Ok(Entry { formats, meta })
            });
        match r {
            Ok(entry) => {
                return Some(Extraction {
                    via: Via::Resolver(def.id),
                    page: url.to_owned(),
                    entries: vec![entry],
                });
            }
            Err(e) => {
                if fault_rank(&e) > fault_rank(best) {
                    *best = e;
                }
            }
        }
    }
    None
}

pub(super) async fn extract_task(
    cfg: Arc<FetchConfig>,
    url: String,
    mode: ExtractMode,
) -> Result<Extraction, String> {
    if proxy_blocked() {
        return Err("proxy required".into());
    }
    let order: &[Reader] = match mode {
        ExtractMode::Builtin => &[Reader::Builtin],
        ExtractMode::Resolvers => &[Reader::Resolvers],
        ExtractMode::Auto => reader_order(cfg.save.extractor),
    };
    let (mut blocked, mut best) = (false, String::from("no media"));
    for (i, r) in order.iter().enumerate() {
        match r {
            Reader::Builtin => match builtin(&cfg, &url).await {
                Ok(x) => return Ok(x),
                Err(e) => {
                    blocked |= e == "proxy required";
                    if fault_rank(&e) > fault_rank(&best) {
                        best = e;
                    }
                }
            },
            Reader::Resolvers => {
                // A media address is not a page: a tool is not asked about it.
                if mode == ExtractMode::Auto
                    && i == 0
                    && order.contains(&Reader::Builtin)
                    && cfg.resolvers.iter().any(|r| r.enabled)
                    && let Some(x) = probe_direct(&cfg, &url).await
                {
                    return Ok(x);
                }
                if let Some(x) = resolvers(&cfg, &url, &mut blocked, &mut best).await {
                    return Ok(x);
                }
            }
        }
    }
    Err(if fault_rank(&best) > 0 {
        best
    } else if blocked {
        "proxy required".into()
    } else {
        "no media".into()
    })
}

/// Is the address itself a media file or a manifest? Reads one block.
async fn probe_direct(cfg: &FetchConfig, url: &str) -> Option<Extraction> {
    probe_inner(cfg, url).await.ok()
}

async fn probe_inner(cfg: &FetchConfig, url: &str) -> Result<Extraction, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) || proxy_blocked() {
        return Err("no media".into());
    }
    let _permit = PAGE_READS
        .acquire()
        .await
        .map_err(|_| "cancelled".to_string())?;
    let client = FetchService::builder()?
        .connect_timeout(SEARCH_TIMEOUT)
        .redirect(same_origin_policy())
        .build()
        .map_err(|e| e.without_url().to_string())?;
    match fetch_page(cfg, &client, url, None, true).await? {
        Fetched::Media(p) => Ok(Extraction {
            via: Via::Direct,
            page: url.to_owned(),
            entries: vec![direct_entry(url, p, cfg.save.user_agent())],
        }),
        Fetched::Page(..) => Err("no media".into()),
    }
}

/// Play what the built-in reader found: the first entry, its best formats in
/// turn (at most four). Mirrors the resolver path.
pub(super) async fn extract_and_open(
    cfg: Arc<FetchConfig>,
    handle: tokio::runtime::Handle,
    page_url: String,
    opts: &StreamOpts,
) -> Result<StreamTicket, String> {
    let ex = builtin(&cfg, &page_url).await?;
    let entry = ex.entries.into_iter().next().ok_or("no media")?;
    let title = entry.meta.title.unwrap_or_else(|| "Video".to_owned());
    let mut last = "no media".to_string();
    for (i, f) in muxed(entry.formats)?.into_iter().take(4).enumerate() {
        let mut o = opts.clone();
        o.headers.extend(f.headers);
        // A signed address that expires mid-stream is renewed by reading the
        // page again; the reader verifies it is the same file.
        let (c2, h2, p2) = (cfg.clone(), handle.clone(), page_url.clone());
        let reopen: crate::stream::ReopenFn = Arc::new(move || {
            let (tx, rx) = channel();
            let (c, p) = (c2.clone(), p2.clone());
            h2.spawn(async move {
                let r = builtin(&c, &p)
                    .await
                    .ok()
                    .and_then(|x| x.entries.into_iter().next())
                    .and_then(|e| muxed(e.formats).ok())
                    .and_then(|v| v.into_iter().nth(i).map(|f| f.url))
                    .ok_or_else(|| "expired".to_string());
                let _ = tx.send(r);
            });
            rx
        });
        match open_direct(&cfg, handle.clone(), f.url, &o, Some(reopen), Some(&title)).await {
            Ok(Some(t)) => return Ok(t),
            Ok(None) => {}
            Err(e) => {
                if fault_rank(&e) > fault_rank(&last) {
                    last = e;
                }
            }
        }
    }
    Err(last)
}
