//! Segmented VOD streams (HLS and DASH) as a [`TimedSource`].
//!
//! The reader downloads segments ahead of the consumer (at most 60 s, 64 MiB
//! and four requests at once), decrypts AES-128 in memory, and hands out
//! container bytes in presentation order. It never touches the disk and never
//! gives a URL to anyone: the consumer only sees bytes.
//!
//! Rules every request obeys: http(s) only; a public manifest may not point
//! at a loopback/private target (`sources::inward_ok`); the proxy kill switch
//! is checked before each request; only User-Agent, Referer and Origin are
//! sent on top of the defaults, never a source key.
//!
//! Limits for now: muxed representations only (a stream whose audio is a
//! separate rendition is refused with `separate tracks`; a master that also
//! offers a muxed variant plays that one). Live is HLS only (see below); a
//! dynamic DASH manifest is refused with `live`.
//!
//! Live HLS (a media playlist without `ENDLIST`): a refresh task reloads the
//! playlist every target duration (half of it while nothing changed, never
//! under [`LiveTuning::floor`]) and appends the segments whose media sequence
//! it has not seen yet. Playback starts three segments from the live edge.
//! A hole in the sequence, a numbering restart or `EXT-X-DISCONTINUITY` are
//! timeline breaks (`discont: true`); a segment that already left the window
//! is skipped, a missing one is skipped too (five in a row end the stream).
//! The track keeps only a short tail behind the playhead, so memory is
//! bounded by the window plus the prefetch limits. The stream ends cleanly on
//! `ENDLIST` or after [`LiveTuning::max_listen`]; it ends with `live ended`
//! when the playlist cannot be reloaded three times in a row or does not
//! change for [`LiveTuning::stale`]. The duration is `None` (no seeking), the
//! quality is fixed (no variants are offered) and key systems stay refused.
//!
//! Timeouts are stall based, never a total: 20 s to the first byte of the
//! answer, 15 s without a body byte; a body that keeps moving is never cut.
//! (The 15 s connect timeout belongs to the client the caller builds.)
//!
//! Quality: the throughput estimate is the aggregate of all concurrent
//! downloads per wall second of activity. [`AbrPolicy`] decides; the switch
//! happens at a segment boundary inside the reader when the codecs agree, and
//! only otherwise is a pipeline rebuild requested through `wants_downgrade`.
//!
//! A segment that still fails after its retries is skipped with a
//! discontinuity when it is under 5 % of the duration (and all skips together
//! stay under 5 %); otherwise the stream ends with the raw error.
//!
//! Chunk contract: the first chunk after a seek or a discontinuity carries
//! `discont: true`; when an init segment goes first it is that chunk (pts
//! `None`) and the media chunk that follows has `discont: false`.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use jizzcraft_core::manifest::looks_like_manifest;
use jizzcraft_core::timed::{Chunk, TimedSource, VariantInfo};
use reqwest::Url;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::pick::{Quality, pick_variant};
use crate::sources::inward_ok;
use crate::{dash, hls};

/// Prefetch window ahead of the playback position.
const WINDOW: Duration = Duration::from_secs(60);
const MAX_BUFFER: usize = 64 << 20;
const MAX_CONCURRENT: usize = 4;
const MAX_SEGMENT: usize = 64 << 20;
const MAX_KEY_READ: usize = 64;
/// Servers that ignore `Range` get the first N bytes read and no more.
const MAX_RANGE_SKIP: u64 = 256 << 20;
// Five tries; after that the server and we agree to disagree.
const ATTEMPTS: u32 = 5;
/// Documented for the client builder; the client is built by the caller.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const TTFB_TIMEOUT: Duration = Duration::from_secs(20);
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest share of the duration that may be skipped after failures.
const SKIP_SHARE: u32 = 20;
const MAX_INITS: usize = 8;
const MIN_SAMPLE: Duration = Duration::from_millis(500);
const WAIT_SLICE: Duration = Duration::from_millis(100);
/// Live playback starts this many segments from the live edge.
const LIVE_EDGE: usize = 3;
/// Segments kept behind the playhead on a live track.
const LIVE_KEEP_BEHIND: usize = 16;
/// Consecutive live segments that may be skipped before the stream ends.
const LIVE_MAX_SKIPS: u32 = 5;
/// Consecutive playlist reloads that may fail before the stream ends.
const LIVE_MAX_RELOAD_FAILS: u32 = 3;
const LIVE_MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Live limits; the defaults are the product values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveTuning {
    /// Shortest reload interval (guards against a target duration of 0).
    pub floor: Duration,
    /// End with `live ended` when the playlist brings nothing new for this
    /// long (or six target durations, whichever is longer).
    pub stale: Duration,
    /// Longest listening session; the stream then ends cleanly.
    pub max_listen: Duration,
}

impl Default for LiveTuning {
    fn default() -> Self {
        LiveTuning {
            floor: Duration::from_secs(1),
            stale: Duration::from_secs(120),
            max_listen: Duration::from_secs(12 * 3600),
        }
    }
}

/// Live playlist state; present only for an HLS stream without `ENDLIST`.
struct LiveState {
    target: Duration,
    /// Media sequence number of the last segment appended.
    last_seq: u64,
    /// First track index of the current numbering (moves on a restart).
    epoch_idx: usize,
    /// `ENDLIST` seen (or the listening limit reached): drain, then end.
    ended: bool,
    /// The stream cannot go on; played out after what is buffered.
    fatal: Option<String>,
    /// Segments skipped in a row.
    fail_run: u32,
    task: Option<AbortHandle>,
}

/// Inclusive byte range.
type Range = (u64, u64);

#[derive(Clone, PartialEq, Eq)]
struct Part {
    url: String,
    range: Option<Range>,
}

#[derive(Clone)]
struct KeyDesc {
    url: String,
    iv: Option<[u8; 16]>,
}

#[derive(Clone)]
struct Seg {
    part: Part,
    dur: Duration,
    start: Duration,
    init: Option<Part>,
    key: Option<KeyDesc>,
    seq: u64,
    discont: bool,
}

/// The segments of one stream. Indices are absolute: a live track drops old
/// segments from the front (`base` grows), so `segs[0]` is index `base`.
#[derive(Clone)]
struct Track {
    base: usize,
    segs: Vec<Seg>,
    total: Duration,
}

impl Track {
    fn new(mut segs: Vec<Seg>) -> Self {
        let mut t = Duration::ZERO;
        for s in &mut segs {
            s.start = t;
            t += s.dur;
        }
        Track {
            base: 0,
            segs,
            total: t,
        }
    }

    /// One past the last index.
    fn end(&self) -> usize {
        self.base + self.segs.len()
    }

    fn get(&self, i: usize) -> Option<&Seg> {
        self.segs.get(i.checked_sub(self.base)?)
    }

    /// Index of the segment containing `t` (the last one if past the end).
    fn index_at(&self, t: Duration) -> usize {
        self.base
            + self
                .segs
                .partition_point(|s| s.start <= t)
                .saturating_sub(1)
    }

    /// Live: append a segment at the end of the timeline.
    fn push(&mut self, mut s: Seg) {
        s.start = self.total;
        self.total += s.dur;
        self.segs.push(s);
    }

    /// Live: forget every segment before absolute index `i`.
    fn trim_before(&mut self, i: usize) {
        let n = i.saturating_sub(self.base).min(self.segs.len());
        self.segs.drain(..n);
        self.base += n;
    }
}

enum VariantSrc {
    Playlist(String),
    Dash(Box<dash::Representation>),
}

struct Ready {
    bytes: Vec<u8>,
    init: Option<Arc<Vec<u8>>>,
}

// ---- throughput and quality policy -----------------------------------------

/// Aggregate throughput: bytes received per second during which at least one
/// download was active (concurrent downloads share the same seconds).
#[derive(Default)]
pub(crate) struct Meter {
    n: usize,
    since: Option<Instant>,
    active: Duration,
    bytes: u64,
    mark_active: Duration,
    mark_bytes: u64,
    ema: Option<f64>,
}

impl Meter {
    fn begin(&mut self, now: Instant) {
        if self.n == 0 {
            self.since = Some(now);
        }
        self.n += 1;
    }

    fn end(&mut self, now: Instant) {
        self.n = self.n.saturating_sub(1);
        if self.n == 0
            && let Some(s) = self.since.take()
        {
            self.active += now.saturating_duration_since(s);
        }
    }

    fn add(&mut self, n: usize) {
        self.bytes += n as u64;
    }

    /// Smoothed bits per second; folds in a new window once enough has
    /// been observed.
    fn sample(&mut self, now: Instant) -> Option<f64> {
        let act = self.active
            + self
                .since
                .map_or(Duration::ZERO, |s| now.saturating_duration_since(s));
        let d = act.saturating_sub(self.mark_active);
        let moved = self.bytes - self.mark_bytes;
        // Half a second of activity, or a quarter megabyte of it on a fast
        // link, makes a window worth believing.
        if d >= MIN_SAMPLE || (moved >= (256 << 10) && d >= Duration::from_millis(20)) {
            let bps = moved as f64 * 8.0 / d.as_secs_f64();
            self.mark_active = act;
            self.mark_bytes = self.bytes;
            self.ema = Some(self.ema.map_or(bps, |e| e * 0.5 + bps * 0.5));
        }
        self.ema
    }
}

/// Counts a download as active until dropped (also when it is aborted).
struct Active(Arc<Shared>);

impl Active {
    fn new(sh: &Arc<Shared>) -> Self {
        sh.meter().begin(Instant::now());
        Active(Arc::clone(sh))
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.meter().end(Instant::now());
    }
}

/// Tunable thresholds of [`AbrPolicy`]; the defaults are the product values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbrConfig {
    /// Lower when less than this is buffered ahead and throughput is short.
    pub down_buffer: Duration,
    /// ...and throughput is under this multiple of the current bitrate.
    pub down_ratio: f64,
    /// Never lower again within this time of the last switch.
    pub down_gap: Duration,
    /// Raise only with at least this much buffered ahead...
    pub up_buffer: Duration,
    /// ...and throughput at this multiple of the next variant's bitrate...
    pub up_ratio: f64,
    /// ...held for this long...
    pub up_hold: Duration,
    /// ...and at most once in this time (counted from any switch).
    pub up_gap: Duration,
}

impl Default for AbrConfig {
    fn default() -> Self {
        AbrConfig {
            down_buffer: Duration::from_secs(10),
            down_ratio: 1.2,
            down_gap: Duration::from_secs(4),
            up_buffer: Duration::from_secs(30),
            up_ratio: 1.5,
            up_hold: Duration::from_secs(20),
            up_gap: Duration::from_secs(60),
        }
    }
}

/// What the policy sees. `cands` = (variant index, bandwidth in bit/s) of
/// every variant that may be chosen, the current one included.
pub struct AbrInput<'a> {
    /// Monotonic time since the stream opened.
    pub now: Duration,
    pub buffer_ahead: Duration,
    /// Aggregate throughput, bit/s, once measured.
    pub bps: Option<f64>,
    pub current: usize,
    pub cands: &'a [(usize, u64)],
    /// A segment is already on its third failed try.
    pub retry_pressure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbrChoice {
    Stay,
    Switch(usize),
}

/// Pure quality policy: no clock, no I/O; the caller supplies the time.
#[derive(Debug, Clone, Default)]
pub struct AbrPolicy {
    cfg: AbrConfig,
    good_since: Option<Duration>,
    last_switch: Option<Duration>,
}

impl AbrPolicy {
    pub fn new(cfg: AbrConfig) -> Self {
        AbrPolicy {
            cfg,
            good_since: None,
            last_switch: None,
        }
    }

    pub fn decide(&mut self, i: &AbrInput<'_>) -> AbrChoice {
        let cfg = self.cfg;
        let Some(cur_bw) = i.cands.iter().find(|c| c.0 == i.current).map(|c| c.1) else {
            return AbrChoice::Stay;
        };
        let lower = || {
            i.cands
                .iter()
                .copied()
                .filter(|c| c.1 < cur_bw || (c.1 == cur_bw && c.0 < i.current))
        };
        let since_switch = self.last_switch.map(|t| i.now.saturating_sub(t));
        let short = i.buffer_ahead < cfg.down_buffer
            && i.bps.is_some_and(|b| b < cfg.down_ratio * cur_bw as f64);
        if (short || i.retry_pressure) && since_switch.is_none_or(|d| d >= cfg.down_gap) {
            // The best lower variant the measured speed carries, else the
            // lowest there is.
            let fits = |c: &(usize, u64)| i.bps.is_some_and(|b| c.1 as f64 * cfg.down_ratio <= b);
            let pick = lower()
                .filter(fits)
                .max_by_key(|c| (c.1, c.0))
                .or_else(|| lower().min_by_key(|c| (c.1, c.0)));
            if let Some((v, _)) = pick {
                self.good_since = None;
                self.last_switch = Some(i.now);
                return AbrChoice::Switch(v);
            }
        }
        // Next variant up, if any.
        let next = i
            .cands
            .iter()
            .copied()
            .filter(|c| c.1 > cur_bw || (c.1 == cur_bw && c.0 > i.current))
            .min_by_key(|c| (c.1, c.0));
        let good = next.is_some_and(|(_, bw)| {
            i.buffer_ahead >= cfg.up_buffer && i.bps.is_some_and(|b| b >= cfg.up_ratio * bw as f64)
        });
        if !good {
            self.good_since = None;
            return AbrChoice::Stay;
        }
        let since = *self.good_since.get_or_insert(i.now);
        if i.now.saturating_sub(since) >= cfg.up_hold
            && since_switch.is_none_or(|d| d >= cfg.up_gap)
        {
            self.good_since = None;
            self.last_switch = Some(i.now);
            return AbrChoice::Switch(next.map_or(i.current, |c| c.0));
        }
        AbrChoice::Stay
    }
}

fn families(c: &str) -> Vec<String> {
    let mut f: Vec<String> = c
        .split(',')
        .map(|p| {
            p.trim()
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        })
        .filter(|p| !p.is_empty())
        .collect();
    f.sort();
    f.dedup();
    f
}

/// Can the reader move between these two variants without a new pipeline?
/// Known codec families must agree; unknown ones are trusted only for HLS
/// (transport streams carry their own tables).
fn compatible(a: Option<&str>, b: Option<&str>, hls: bool) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => families(a) == families(b),
        (None, None) => hls,
        _ => false,
    }
}

struct State {
    track: Arc<Track>,
    epoch: u64,
    next: usize,
    ready: HashMap<usize, Ready>,
    inflight: HashMap<usize, AbortHandle>,
    failed: HashMap<usize, String>,
    outbox: VecDeque<Chunk>,
    keys: HashMap<String, Arc<[u8; 16]>>,
    last_init: Option<Part>,
    need_discont: bool,
    avg_bytes: usize,
    abr: AbrPolicy,
    /// A segment is on a late retry: lower before it gives up.
    pressure: bool,
    /// A lower quality needs a new pipeline; `wants_downgrade` says so once.
    rebuild: bool,
    /// Time skipped after failed segments.
    skipped: Duration,
    inits: HashMap<(String, Option<Range>), Arc<Vec<u8>>>,
    auto: bool,
    variant: usize,
    switching: Option<usize>,
    switched: Option<(usize, Track)>,
    switch_task: Option<AbortHandle>,
    live: Option<LiveState>,
}

struct Shared {
    client: reqwest::Client,
    /// The address the user gave; every other address is judged against it.
    origin: String,
    headers: Vec<(String, String)>,
    variants: Vec<VariantInfo>,
    srcs: Vec<VariantSrc>,
    codecs: Vec<Option<String>>,
    quality: Quality,
    t0: Instant,
    tuning: LiveTuning,
    closed: AtomicBool,
    meter: Mutex<Meter>,
    state: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn meter(&self) -> MutexGuard<'_, Meter> {
        self.meter.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct SegmentedReader {
    handle: Handle,
    sh: Arc<Shared>,
}

impl SegmentedReader {
    /// Replace the quality thresholds (tests and tuning; defaults are right).
    #[must_use]
    pub fn with_abr(self, cfg: AbrConfig) -> Self {
        self.sh.lock().abr = AbrPolicy::new(cfg);
        self
    }
}

// ---- requests ---------------------------------------------------------------

pub(crate) fn allowed_headers(h: Vec<(String, String)>) -> Vec<(String, String)> {
    h.into_iter()
        .filter(|(k, v)| {
            ["user-agent", "referer", "origin"].contains(&k.to_ascii_lowercase().as_str())
                && v.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
        })
        .collect()
}

fn resolve(base: &str, rel: &str, origin: &str) -> Result<String, String> {
    let u = Url::parse(base)
        .and_then(|b| b.join(rel))
        .map_err(|_| "no media".to_owned())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("unsupported stream".into());
    }
    if !inward_ok(origin, u.as_str()) {
        return Err("unsupported stream".into());
    }
    Ok(u.into())
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let s = resp.headers().get("retry-after")?.to_str().ok()?;
    s.trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n.min(30)))
}

enum BodyErr {
    Final(String),
    Retry(String),
}

/// Read at most `cap` bytes, after discarding `skip` leading bytes.
async fn read_body(
    sh: &Shared,
    mut resp: reqwest::Response,
    cap: usize,
    skip: u64,
    take: Option<u64>,
) -> Result<Vec<u8>, BodyErr> {
    if take.is_none() && resp.content_length().is_some_and(|l| l > cap as u64) {
        return Err(BodyErr::Final("too large".into()));
    }
    let hint = resp
        .content_length()
        .unwrap_or(0)
        .min(cap as u64)
        .min(8 << 20);
    let mut out = Vec::with_capacity(hint as usize);
    let mut seen = 0u64;
    loop {
        let c = match tokio::time::timeout(STALL_TIMEOUT, resp.chunk()).await {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => break,
            _ => return Err(BodyErr::Retry("stalled".into())),
        };
        sh.meter().add(c.len());
        let mut piece: &[u8] = &c;
        if seen < skip {
            let drop_n = ((skip - seen) as usize).min(piece.len());
            piece = &piece[drop_n..];
        }
        seen += c.len() as u64;
        if let Some(t) = take {
            let room = (t as usize).saturating_sub(out.len());
            piece = &piece[..piece.len().min(room)];
        }
        if out.len() + piece.len() > cap {
            return Err(BodyErr::Final("too large".into()));
        }
        out.extend_from_slice(piece);
        if take.is_some_and(|t| out.len() as u64 >= t) {
            break;
        }
    }
    Ok(out)
}

/// One GET with retries. Errors are fixed strings (`http 404`, `stalled`...).
async fn get_bytes(
    sh: &Shared,
    url: &str,
    range: Option<Range>,
    cap: usize,
) -> Result<Vec<u8>, String> {
    let mut last = String::from("stalled");
    for attempt in 0..ATTEMPTS {
        if sh.closed.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        // The kill switch applies to a stream that is already open.
        if crate::service::proxy_blocked() {
            return Err("proxy required".into());
        }
        if !inward_ok(&sh.origin, url) {
            return Err("unsupported stream".into());
        }
        let mut req = sh.client.get(url);
        for (k, v) in &sh.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some((a, b)) = range {
            req = req.header("Range", format!("bytes={a}-{b}"));
        }
        let mut wait = Duration::from_millis(250 << attempt);
        match tokio::time::timeout(TTFB_TIMEOUT, req.send()).await {
            Err(_) => last = "stalled".into(),
            Ok(Ok(resp)) => {
                let code = resp.status().as_u16();
                match code {
                    200 | 206 => {
                        let (skip, take) = match (range, code) {
                            // The server ignored the range: cut it ourselves.
                            (Some((a, b)), 200) => {
                                if b >= MAX_RANGE_SKIP.max(MAX_SEGMENT as u64) + MAX_SEGMENT as u64
                                {
                                    return Err("too large".into());
                                }
                                (a, Some(b - a + 1))
                            }
                            _ => (0, None),
                        };
                        match read_body(sh, resp, cap, skip, take).await {
                            Ok(b) => return Ok(b),
                            Err(BodyErr::Final(e)) => return Err(e),
                            Err(BodyErr::Retry(e)) => last = e,
                        }
                    }
                    // Final on VOD: the file is not there.
                    404 | 410 => return Err(format!("http {code}")),
                    429 | 503 => {
                        if let Some(d) = retry_after(&resp) {
                            wait = d;
                        }
                        last = format!("http {code}");
                    }
                    408 | 500..=599 => last = format!("http {code}"),
                    c => return Err(format!("http {c}")),
                }
            }
            Ok(Err(e)) => {
                last = if e.is_timeout() {
                    "stalled".into()
                } else {
                    "error sending request".into()
                };
            }
        }
        if attempt >= 2 {
            // Third failure: tell the quality policy before we give up.
            sh.lock().pressure = true;
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(wait).await;
        }
    }
    Err(last)
}

fn retain_muxed<T>(v: &mut Vec<T>, separate: &[bool]) {
    let mut i = 0;
    v.retain(|_| {
        i += 1;
        !separate[i - 1]
    });
}

/// Failures worth skipping a segment for; policy refusals are final.
fn skippable(e: &str) -> bool {
    e == "stalled" || e == "error sending request" || e.starts_with("http ")
}

fn decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    if !data.len().is_multiple_of(16) {
        return Err("unsupported stream".into());
    }
    cbc::Decryptor::<Aes128>::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| "unsupported stream".to_owned())
}

async fn get_key(sh: &Shared, url: &str) -> Result<Arc<[u8; 16]>, String> {
    if let Some(k) = sh.lock().keys.get(url) {
        return Ok(Arc::clone(k));
    }
    let raw = get_bytes(sh, url, None, MAX_KEY_READ).await?;
    let key: [u8; 16] = raw
        .as_slice()
        .try_into()
        .map_err(|_| "unsupported stream".to_owned())?;
    let key = Arc::new(key);
    sh.lock().keys.insert(url.to_owned(), Arc::clone(&key));
    Ok(key)
}

fn head_ok(b: &[u8]) -> Result<(), String> {
    // A playlist where media should be is somebody else's address book.
    if looks_like_manifest(&b[..b.len().min(1024)]) {
        return Err("no media".into());
    }
    Ok(())
}

/// Init segments are shared by many media segments: fetch each once.
async fn get_init(sh: &Shared, p: &Part) -> Result<Arc<Vec<u8>>, String> {
    let key = (p.url.clone(), p.range);
    if let Some(i) = sh.lock().inits.get(&key) {
        return Ok(Arc::clone(i));
    }
    let b = get_bytes(sh, &p.url, p.range, MAX_SEGMENT).await?;
    head_ok(&b)?;
    let b = Arc::new(b);
    let mut st = sh.lock();
    if st.inits.len() >= MAX_INITS {
        st.inits.clear();
    }
    st.inits.insert(key, Arc::clone(&b));
    Ok(b)
}

async fn download(sh: &Shared, seg: &Seg) -> Result<(Ready, usize, Duration), String> {
    let t0 = Instant::now();
    let mut body = get_bytes(sh, &seg.part.url, seg.part.range, MAX_SEGMENT).await?;
    let net = t0.elapsed();
    let wire = body.len();
    if let Some(k) = &seg.key {
        let key = get_key(sh, &k.url).await?;
        let iv = k.iv.unwrap_or_else(|| (u128::from(seg.seq)).to_be_bytes());
        body = decrypt(&key, &iv, &body)?;
    }
    head_ok(&body)?;
    let init = match &seg.init {
        Some(p) => Some(get_init(sh, p).await?),
        None => None,
    };
    Ok((Ready { bytes: body, init }, wire, net))
}

async fn fetch_task(sh: Arc<Shared>, epoch: u64, idx: usize, seg: Seg) {
    let active = Active::new(&sh);
    let res = download(&sh, &seg).await;
    drop(active);
    let mut st = sh.lock();
    if st.epoch != epoch {
        return;
    }
    st.inflight.remove(&idx);
    match res {
        Ok((r, wire, _net)) => {
            st.avg_bytes = if st.avg_bytes == 0 {
                wire
            } else {
                (st.avg_bytes * 7 + wire * 3) / 10
            };
            st.ready.insert(idx, r);
        }
        Err(e) => {
            st.failed.insert(idx, e);
        }
    }
    drop(st);
    sh.cv.notify_all();
}

// ---- tracks -----------------------------------------------------------------

fn hls_segs(base: &str, origin: &str, segments: &[hls::Segment]) -> Result<Vec<Seg>, String> {
    let part_of = |uri: &str, r: Option<hls::ByteRange>| -> Result<Part, String> {
        Ok(Part {
            url: resolve(base, uri, origin)?,
            range: r.map(|r| (r.offset, r.offset + r.len - 1)),
        })
    };
    let mut segs = Vec::with_capacity(segments.len());
    for s in segments {
        let init = match &s.map {
            Some(m) => Some(part_of(&m.uri, m.range)?),
            None => None,
        };
        let key = match &s.key {
            hls::KeyRef::None => None,
            hls::KeyRef::Aes128 { uri, iv } => Some(KeyDesc {
                url: resolve(base, uri, origin)?,
                iv: *iv,
            }),
        };
        let dur = if s.duration.is_finite() && s.duration > 0.0 {
            Duration::from_secs_f64(s.duration)
        } else {
            Duration::ZERO
        };
        segs.push(Seg {
            part: part_of(&s.uri, s.range)?,
            dur,
            start: Duration::ZERO,
            init,
            key,
            seq: s.seq,
            discont: s.discontinuity,
        });
    }
    Ok(segs)
}

fn hls_track(base: &str, origin: &str, segments: &[hls::Segment]) -> Result<Track, String> {
    Ok(Track::new(hls_segs(base, origin, segments)?))
}

/// What a media playlist turned out to be.
enum Hls {
    Vod(Track),
    Live {
        /// The last [`LIVE_EDGE`] segments of the window.
        track: Track,
        target: Duration,
        last_seq: u64,
    },
}

fn hls_load(base: &str, origin: &str, text: &str) -> Result<Hls, String> {
    match hls::parse(text)? {
        hls::Playlist::Media {
            segments,
            ended,
            vod,
            target,
            ..
        } => {
            if ended || vod {
                return Ok(Hls::Vod(hls_track(base, origin, &segments)?));
            }
            let mut segs = hls_segs(base, origin, &segments)?;
            let last_seq = segs.last().map_or(0, |s| s.seq);
            let skip = segs.len().saturating_sub(LIVE_EDGE);
            segs.drain(..skip);
            let target = if target.is_finite() && target > 0.0 {
                Duration::from_secs_f64(target.min(LIVE_MAX_INTERVAL.as_secs_f64()))
            } else {
                Duration::ZERO
            };
            Ok(Hls::Live {
                track: Track::new(segs),
                target,
                last_seq,
            })
        }
        hls::Playlist::Master { .. } => Err("unsupported stream".into()),
    }
}

fn hls_media(base: &str, origin: &str, text: &str) -> Result<Track, String> {
    match hls::parse(text)? {
        hls::Playlist::Media {
            segments,
            ended,
            vod,
            ..
        } => {
            // A live window has no end to seek in; later.
            if !(ended || vod) {
                return Err("live stream".into());
            }
            hls_track(base, origin, &segments)
        }
        hls::Playlist::Master { .. } => Err("unsupported stream".into()),
    }
}

async fn load_track(sh: &Shared, src: &VariantSrc) -> Result<Track, String> {
    match src {
        VariantSrc::Playlist(url) => {
            let raw = get_bytes(sh, url, None, hls::MAX_TEXT).await?;
            let text = String::from_utf8(raw).map_err(|_| "no media".to_owned())?;
            hls_media(url, &sh.origin, &text)
        }
        VariantSrc::Dash(rep) => {
            let origin = &sh.origin;
            let part = |u: &str, r: Option<Range>| -> Result<Part, String> {
                Ok(Part {
                    url: resolve(origin, u, origin)?,
                    range: r,
                })
            };
            let mut segs = Vec::new();
            let mk = |part: Part, dur: Duration, init: Option<Part>| Seg {
                part,
                dur,
                start: Duration::ZERO,
                init,
                key: None,
                seq: 0,
                discont: false,
            };
            match &rep.segs {
                dash::SegSpec::List { init, media } => {
                    let init = match init {
                        Some((u, r)) => Some(part(u, *r)?),
                        None => None,
                    };
                    for (u, r, d) in media {
                        segs.push(mk(part(u, *r)?, *d, init.clone()));
                    }
                }
                dash::SegSpec::Base {
                    url,
                    index_range,
                    init_range,
                } => {
                    let url = resolve(origin, url, origin)?;
                    let idx = get_bytes(sh, &url, Some(*index_range), MAX_SEGMENT).await?;
                    let init = init_range
                        .or_else(|| (index_range.0 > 0).then(|| (0, index_range.0 - 1)))
                        .map(|r| Part {
                            url: url.clone(),
                            range: Some(r),
                        });
                    for ((a, b), d) in dash::parse_sidx(&idx, index_range.1 + 1) {
                        let p = Part {
                            url: url.clone(),
                            range: Some((a, b)),
                        };
                        segs.push(mk(p, d, init.clone()));
                    }
                }
            }
            if segs.is_empty() {
                return Err("no media".into());
            }
            Ok(Track::new(segs))
        }
    }
}

fn new_state(track: Track, variant: usize, auto: bool) -> State {
    State {
        track: Arc::new(track),
        epoch: 0,
        next: 0,
        ready: HashMap::new(),
        inflight: HashMap::new(),
        failed: HashMap::new(),
        outbox: VecDeque::new(),
        keys: HashMap::new(),
        last_init: None,
        need_discont: false,
        avg_bytes: 0,
        abr: AbrPolicy::default(),
        pressure: false,
        rebuild: false,
        skipped: Duration::ZERO,
        inits: HashMap::new(),
        auto,
        variant,
        switching: None,
        switched: None,
        switch_task: None,
        live: None,
    }
}

type VariantSet = (Vec<VariantInfo>, Vec<VariantSrc>, Vec<Option<String>>);

fn finish(
    handle: Handle,
    client: reqwest::Client,
    origin: String,
    headers: Vec<(String, String)>,
    (variants, srcs, codecs): VariantSet,
    (track, variant): (Track, usize),
    (quality, tuning): (Quality, LiveTuning),
) -> SegmentedReader {
    let sh = Arc::new(Shared {
        client,
        origin,
        headers: allowed_headers(headers),
        variants,
        srcs,
        codecs,
        quality,
        t0: Instant::now(),
        tuning,
        closed: AtomicBool::new(false),
        meter: Mutex::new(Meter::default()),
        state: Mutex::new(new_state(track, variant, true)),
        cv: Condvar::new(),
    });
    SegmentedReader { handle, sh }
}

fn probe(
    client: reqwest::Client,
    url: &str,
    headers: Vec<(String, String)>,
    tuning: LiveTuning,
) -> Shared {
    Shared {
        client,
        origin: url.to_owned(),
        headers: allowed_headers(headers),
        variants: Vec::new(),
        srcs: Vec::new(),
        codecs: Vec::new(),
        quality: Quality::default(),
        t0: Instant::now(),
        tuning,
        closed: AtomicBool::new(false),
        meter: Mutex::new(Meter::default()),
        state: Mutex::new(new_state(Track::new(Vec::new()), 0, true)),
        cv: Condvar::new(),
    }
}

/// Start a live stream: the reader plus the task that reloads the playlist.
fn finish_live(
    handle: Handle,
    client: reqwest::Client,
    origin: String,
    playlist: String,
    headers: Vec<(String, String)>,
    live: (Track, Duration, u64),
    (quality, tuning): (Quality, LiveTuning),
) -> SegmentedReader {
    let (track, target, last_seq) = live;
    // No variants: the quality of a live stream is fixed.
    let r = finish(
        handle.clone(),
        client,
        origin,
        headers,
        (Vec::new(), Vec::new(), Vec::new()),
        (track, 0),
        (quality, tuning),
    );
    let task = handle.spawn(live_refresh(handle.clone(), Arc::clone(&r.sh), playlist));
    r.sh.lock().live = Some(LiveState {
        target,
        last_seq,
        epoch_idx: 0,
        ended: false,
        fatal: None,
        fail_run: 0,
        task: Some(task.abort_handle()),
    });
    r
}

/// Open an HLS stream (a master or a media playlist), on demand or live.
pub async fn open_hls(
    handle: Handle,
    client: reqwest::Client,
    url: String,
    quality: Quality,
    headers: Vec<(String, String)>,
) -> Result<SegmentedReader, String> {
    open_hls_with(handle, client, url, quality, headers, LiveTuning::default()).await
}

/// [`open_hls`] with explicit live limits (tests and tuning).
pub async fn open_hls_with(
    handle: Handle,
    client: reqwest::Client,
    url: String,
    quality: Quality,
    headers: Vec<(String, String)>,
    tuning: LiveTuning,
) -> Result<SegmentedReader, String> {
    let p = probe(client.clone(), &url, headers.clone(), tuning);
    let raw = get_bytes(&p, &url, None, hls::MAX_TEXT).await?;
    let text = String::from_utf8(raw).map_err(|_| "no media".to_owned())?;
    match hls::parse(&text)? {
        hls::Playlist::Media { .. } => match hls_load(&url, &url, &text)? {
            Hls::Vod(track) => Ok(finish(
                handle,
                client,
                url,
                headers,
                (Vec::new(), Vec::new(), Vec::new()),
                (track, 0),
                (quality, tuning),
            )),
            Hls::Live {
                track,
                target,
                last_seq,
            } => Ok(finish_live(
                handle,
                client,
                url.clone(),
                url,
                headers,
                (track, target, last_seq),
                (quality, tuning),
            )),
        },
        hls::Playlist::Master { variants, audio } => {
            let mut info = Vec::new();
            let mut srcs = Vec::new();
            let mut codecs = Vec::new();
            let mut separate_audio = Vec::new();
            for v in &variants {
                // A variant pointing inward from a public master is dropped.
                let Ok(u) = resolve(&url, &v.uri, &url) else {
                    continue;
                };
                info.push(VariantInfo {
                    height: v.height,
                    bandwidth: v.bandwidth,
                });
                separate_audio.push(
                    v.audio_group
                        .as_ref()
                        .is_some_and(|g| audio.iter().any(|a| &a.group == g && a.uri.is_some())),
                );
                codecs.push(v.codecs.clone());
                srcs.push(VariantSrc::Playlist(u));
            }
            if info.is_empty() {
                return Err("unsupported stream".into());
            }
            // Muxed variants win when the master offers any; a master that
            // only has video plus separate audio cannot be played yet.
            if separate_audio.iter().all(|s| *s) {
                return Err("separate tracks".into());
            }
            retain_muxed(&mut info, &separate_audio);
            retain_muxed(&mut srcs, &separate_audio);
            retain_muxed(&mut codecs, &separate_audio);
            let pairs: Vec<(Option<u32>, u64)> =
                info.iter().map(|i| (i.height, i.bandwidth)).collect();
            let idx = pick_variant(&pairs, quality, None);
            let VariantSrc::Playlist(pl) = &srcs[idx] else {
                return Err("unsupported stream".into());
            };
            let pl = pl.clone();
            let raw = get_bytes(&p, &pl, None, hls::MAX_TEXT).await?;
            let text = String::from_utf8(raw).map_err(|_| "no media".to_owned())?;
            match hls_load(&pl, &url, &text)? {
                Hls::Vod(track) => Ok(finish(
                    handle,
                    client,
                    url,
                    headers,
                    (info, srcs, codecs),
                    (track, idx),
                    (quality, tuning),
                )),
                Hls::Live {
                    track,
                    target,
                    last_seq,
                } => Ok(finish_live(
                    handle,
                    client,
                    url,
                    pl,
                    headers,
                    (track, target, last_seq),
                    (quality, tuning),
                )),
            }
        }
    }
}

/// Open a static single-period DASH stream (list, template or sidx based).
pub async fn open_dash(
    handle: Handle,
    client: reqwest::Client,
    url: String,
    quality: Quality,
    headers: Vec<(String, String)>,
) -> Result<SegmentedReader, String> {
    let tuning = LiveTuning::default();
    let p = probe(client.clone(), &url, headers.clone(), tuning);
    let raw = get_bytes(&p, &url, None, dash::MAX_TEXT).await?;
    let mpd = dash::parse(&raw)?;
    let has = |k: dash::Kind| mpd.sets.iter().any(|s| s.kind == k);
    if has(dash::Kind::Video) && has(dash::Kind::Audio) {
        // Separate audio is not muxed in yet.
        return Err("separate tracks".into());
    }
    let set = mpd
        .sets
        .iter()
        .find(|s| s.kind == dash::Kind::Video)
        .or_else(|| mpd.sets.first())
        .ok_or_else(|| "no media".to_owned())?;
    let info: Vec<VariantInfo> = set
        .reps
        .iter()
        .map(|r| VariantInfo {
            height: r.height,
            bandwidth: r.bandwidth,
        })
        .collect();
    let codecs: Vec<Option<String>> = set.reps.iter().map(|r| r.codecs.clone()).collect();
    let pairs: Vec<(Option<u32>, u64)> = info.iter().map(|i| (i.height, i.bandwidth)).collect();
    let idx = pick_variant(&pairs, quality, None);
    let srcs: Vec<VariantSrc> = set
        .reps
        .iter()
        .map(|r| VariantSrc::Dash(Box::new(r.clone())))
        .collect();
    let track = load_track(&p, &srcs[idx]).await?;
    Ok(finish(
        handle,
        client,
        url,
        headers,
        (info, srcs, codecs),
        (track, idx),
        (quality, tuning),
    ))
}

// ---- live ----------------------------------------------------------------

impl Shared {
    fn end_live(&self, fatal: Option<String>) {
        let mut st = self.lock();
        if let Some(l) = st.live.as_mut() {
            match fatal {
                Some(e) => l.fatal = Some(e),
                None => l.ended = true,
            }
        }
        drop(st);
        self.cv.notify_all();
    }
}

/// Append the segments of a reloaded window that were not seen yet and move
/// the playhead past what the server no longer has. Returns how many were
/// new. `segs` carries media sequence numbers; `media_seq` is the window's
/// first.
fn live_merge(st: &mut State, media_seq: u64, segs: Vec<Seg>) -> usize {
    let Some(live) = st.live.as_mut() else {
        return 0;
    };
    let Some(window_last) = segs.last().map(|s| s.seq) else {
        return 0;
    };
    let last = live.last_seq;
    // Numbering went backwards: the origin restarted.
    let restart = window_last < last;
    let mut fresh: Vec<Seg> = if restart {
        segs
    } else {
        segs.into_iter().filter(|s| s.seq > last).collect()
    };
    if fresh.is_empty() {
        return 0;
    }
    // Segments lost between two reloads, or a restart: a timeline break.
    if restart || fresh[0].seq > last + 1 {
        fresh[0].discont = true;
    }
    let n = fresh.len();
    let track = Arc::make_mut(&mut st.track);
    let first_new = track.end();
    if restart {
        live.epoch_idx = first_new;
    }
    for s in fresh {
        live.last_seq = s.seq;
        track.push(s);
    }
    // First index the server still lists. Older ones that are not in memory
    // are gone: step over them.
    let window_first = if restart {
        first_new
    } else {
        let from = live.epoch_idx.max(track.base);
        from + track.segs[from - track.base..].partition_point(|s| s.seq < media_seq)
    };
    let mut next = st.next.max(track.base);
    while next < window_first && !st.ready.contains_key(&next) {
        next += 1;
    }
    if next > st.next {
        st.next = next;
        st.need_discont = true;
        st.last_init = None;
    }
    let keep_from = st.next.saturating_sub(LIVE_KEEP_BEHIND);
    if keep_from > track.base {
        track.trim_before(keep_from);
        let base = track.base;
        st.failed.retain(|i, _| *i >= base);
        st.ready.retain(|i, _| *i >= base);
        st.inflight.retain(|i, h| {
            let keep = *i >= base;
            if !keep {
                h.abort();
            }
            keep
        });
    }
    n
}

/// Reload the playlist until the stream ends, is closed or fails. Prefetch
/// goes on here too, so a consumer that is slow to ask does not lose the
/// segments the window is about to drop.
async fn live_refresh(handle: Handle, sh: Arc<Shared>, url: String) {
    let started = Instant::now();
    let mut last_new = Instant::now();
    let mut fails = 0u32;
    let mut changed = true;
    loop {
        let target = sh.lock().live.as_ref().map_or(Duration::ZERO, |l| l.target);
        let every = target.clamp(Duration::ZERO, LIVE_MAX_INTERVAL);
        let every = if changed { every } else { every / 2 };
        let wait = every.max(sh.tuning.floor);
        let due = Instant::now() + wait;
        while Instant::now() < due {
            if sh.closed.load(Ordering::SeqCst) {
                return;
            }
            let left = due.saturating_duration_since(Instant::now());
            tokio::time::sleep(left.min(Duration::from_millis(50))).await;
        }
        if sh.closed.load(Ordering::SeqCst) {
            return;
        }
        if started.elapsed() >= sh.tuning.max_listen {
            sh.end_live(None);
            return;
        }
        let raw = match get_bytes(&sh, &url, None, hls::MAX_TEXT).await {
            Ok(r) => r,
            Err(e) if e == "cancelled" => return,
            Err(e) if !skippable(&e) => return sh.end_live(Some(e)),
            Err(_) => {
                fails += 1;
                changed = false;
                if fails >= LIVE_MAX_RELOAD_FAILS {
                    return sh.end_live(Some("live ended".into()));
                }
                continue;
            }
        };
        let parsed = String::from_utf8(raw)
            .map_err(|_| "no media")
            .and_then(|t| hls::parse(&t));
        let (media_seq, segments, ended) = match parsed {
            Ok(hls::Playlist::Media {
                media_seq,
                segments,
                ended,
                ..
            }) => (media_seq, segments, ended),
            // An empty or garbled reload counts like a failed request.
            Err("no media") => {
                fails += 1;
                changed = false;
                if fails >= LIVE_MAX_RELOAD_FAILS {
                    return sh.end_live(Some("live ended".into()));
                }
                continue;
            }
            // Key systems and policy refusals are never retried.
            Err(e) => return sh.end_live(Some(e.to_owned())),
            Ok(hls::Playlist::Master { .. }) => {
                return sh.end_live(Some("unsupported stream".into()));
            }
        };
        fails = 0;
        let segs = match hls_segs(&url, &sh.origin, &segments) {
            Ok(s) => s,
            Err(e) => return sh.end_live(Some(e)),
        };
        let added = {
            let mut st = sh.lock();
            let n = live_merge(&mut st, media_seq, segs);
            top_up(&handle, &sh, &mut st);
            n
        };
        if ended {
            sh.end_live(None);
            return;
        }
        if added > 0 {
            last_new = Instant::now();
            changed = true;
            sh.cv.notify_all();
        } else {
            changed = false;
            let limit = sh.tuning.stale.max(target * 6);
            if last_new.elapsed() >= limit {
                return sh.end_live(Some("live ended".into()));
            }
        }
    }
}

/// Start downloads for the window ahead of `next`.
fn top_up(handle: &Handle, sh: &Arc<Shared>, st: &mut State) {
    let track = Arc::clone(&st.track);

    let Some(first) = track.get(st.next) else {
        return;
    };
    let base = first.start;
    let mut bytes: usize = st.ready.values().map(|r| r.bytes.len()).sum();
    for idx in st.next..track.end() {
        let Some(seg) = track.get(idx) else {
            continue;
        };
        let wanted_now = idx == st.next;
        if !wanted_now && seg.start.saturating_sub(base) >= WINDOW {
            break;
        }
        if st.ready.contains_key(&idx)
            || st.inflight.contains_key(&idx)
            || st.failed.contains_key(&idx)
        {
            continue;
        }
        if st.inflight.len() >= MAX_CONCURRENT {
            break;
        }
        if !wanted_now && bytes + st.inflight.len() * st.avg_bytes > MAX_BUFFER {
            break;
        }
        bytes += st.avg_bytes;
        let h = handle.spawn(fetch_task(Arc::clone(sh), st.epoch, idx, seg.clone()));
        st.inflight.insert(idx, h.abort_handle());
    }
}

// ---- reader -----------------------------------------------------------------

impl SegmentedReader {
    fn top_up(&self, st: &mut State) {
        top_up(&self.handle, &self.sh, st);
    }

    /// Keep one segment behind and nothing beyond the window.
    fn evict(st: &mut State) {
        let track = Arc::clone(&st.track);
        if track.segs.is_empty() {
            return;
        }
        let lo = st.next.saturating_sub(1);
        let hi_t = track.get(st.next).map_or(track.total, |s| s.start) + WINDOW;
        let keep = |i: usize| i >= lo && track.get(i).is_some_and(|s| s.start <= hi_t);
        st.ready.retain(|i, _| keep(*i));
        let stale: Vec<usize> = st.inflight.keys().copied().filter(|i| !keep(*i)).collect();
        for i in stale {
            if let Some(h) = st.inflight.remove(&i) {
                h.abort();
            }
        }
    }

    fn start_switch(&self, st: &mut State, i: usize) {
        st.switching = Some(i);
        st.switched = None;
        if let Some(h) = st.switch_task.take() {
            h.abort();
        }
        let sh = Arc::clone(&self.sh);
        let task = self.handle.spawn(async move {
            let res = load_track(&sh, &sh.srcs[i]).await;
            let mut st = sh.lock();
            if st.switching != Some(i) {
                return;
            }
            match res {
                Ok(t) => st.switched = Some((i, t)),
                // Stay where we are; the current variant still plays.
                Err(_) => st.switching = None,
            }
            drop(st);
            sh.cv.notify_all();
        });
        st.switch_task = Some(task.abort_handle());
    }

    /// Automatic quality: measure, ask the policy, switch in the reader when
    /// the codecs agree, otherwise ask for a rebuild (downgrades only).
    fn abr_tick(&self, st: &mut State) {
        let sh = &self.sh;
        let measured = sh.meter().sample(Instant::now());
        let pressure = std::mem::take(&mut st.pressure);
        if !st.auto
            || sh.variants.len() < 2
            || st.switching.is_some()
            || st.switched.is_some()
            || st.rebuild
        {
            return;
        }
        let mut buffer = Duration::ZERO;
        let mut i = st.next;
        while st.ready.contains_key(&i) {
            buffer += st.track.get(i).map_or(Duration::ZERO, |s| s.dur);
            i += 1;
        }
        let cap = sh.quality.max_height;
        let cands: Vec<(usize, u64)> = (0..sh.variants.len())
            .filter(|&v| {
                v == st.variant
                    || match (cap, sh.variants[v].height) {
                        (Some(m), Some(h)) => h <= m,
                        _ => true,
                    }
            })
            .map(|v| (v, sh.variants[v].bandwidth))
            .collect();
        let input = AbrInput {
            now: sh.t0.elapsed(),
            buffer_ahead: buffer,
            bps: measured,
            current: st.variant,
            cands: &cands,
            retry_pressure: pressure,
        };
        let AbrChoice::Switch(to) = st.abr.decide(&input) else {
            return;
        };
        let hls = matches!(sh.srcs[to], VariantSrc::Playlist(_));
        let same = compatible(
            sh.codecs.get(st.variant).and_then(|c| c.as_deref()),
            sh.codecs.get(to).and_then(|c| c.as_deref()),
            hls,
        );
        if same {
            self.start_switch(st, to);
        } else if sh.variants[to].bandwidth < sh.variants[st.variant].bandwidth {
            st.rebuild = true;
        }
    }

    fn apply_switch(&self, st: &mut State) {
        let Some((v, track)) = st.switched.take() else {
            return;
        };
        let at = st.track.get(st.next).map_or(st.track.total, |s| s.start);
        let idx = if track.segs.is_empty() || at >= track.total {
            track.end()
        } else {
            track.index_at(at)
        };
        st.epoch += 1;
        for (_, h) in st.inflight.drain() {
            h.abort();
        }
        st.ready.clear();
        st.failed.clear();
        st.track = Arc::new(track);
        st.next = idx;
        st.variant = v;
        st.switching = None;
    }

    fn emit(st: &mut State, idx: usize) {
        let Some(seg) = st.track.get(idx).cloned() else {
            return;
        };
        // The segment's bytes move into the chunk; nothing is copied.
        let Some(Ready { bytes, init }) = st.ready.remove(&idx) else {
            return;
        };
        let mut flag = st.need_discont || seg.discont;
        let mut first = true;
        if let (Some(p), Some(i)) = (&seg.init, init)
            && st.last_init.as_ref() != Some(p)
        {
            // A new init after a quality switch breaks the timeline too.
            flag |= st.last_init.is_some();
            st.outbox.push_back(Chunk {
                bytes: Arc::try_unwrap(i).unwrap_or_else(|a| a.as_ref().clone()),
                pts: None,
                discont: flag,
            });
            first = false;
        }
        st.outbox.push_back(Chunk {
            bytes,
            pts: Some(seg.start),
            discont: flag && first,
        });
        st.last_init = seg.init.clone();
        st.need_discont = false;
        st.next = idx + 1;
        if let Some(l) = st.live.as_mut() {
            l.fail_run = 0;
        }
    }
}

impl Drop for SegmentedReader {
    fn drop(&mut self) {
        // Closing the player cancels every request still in flight.
        self.sh.closed.store(true, Ordering::SeqCst);
        let mut st = self.sh.lock();
        st.epoch += 1;
        for (_, h) in st.inflight.drain() {
            h.abort();
        }
        if let Some(h) = st.switch_task.take() {
            h.abort();
        }
        if let Some(h) = st.live.as_mut().and_then(|l| l.task.take()) {
            h.abort();
        }
        drop(st);
        self.sh.cv.notify_all();
    }
}

impl TimedSource for SegmentedReader {
    fn duration(&self) -> Option<Duration> {
        let st = self.sh.lock();
        st.live.is_none().then_some(st.track.total)
    }

    fn seek(&mut self, t: Duration) -> Duration {
        let mut st = self.sh.lock();
        let track = Arc::clone(&st.track);
        if st.live.is_some() {
            // A live stream has no timeline to move on.
            return track.get(st.next).map_or(track.total, |s| s.start);
        }
        if track.segs.is_empty() || t >= track.total {
            st.next = track.end();
            st.outbox.clear();
            return track.total;
        }
        let idx = track.index_at(t);
        st.next = idx;
        st.need_discont = true;
        st.last_init = None;
        st.outbox.clear();
        st.failed.clear();
        Self::evict(&mut st);
        self.top_up(&mut st);
        track.get(idx).map_or(track.total, |s| s.start)
    }

    fn pull(&mut self) -> io::Result<Option<Chunk>> {
        let deadline = Instant::now() + WAIT_SLICE;
        let sh = Arc::clone(&self.sh);
        let mut st = sh.lock();
        loop {
            if let Some(c) = st.outbox.pop_front() {
                return Ok(Some(c));
            }
            self.apply_switch(&mut st);
            self.abr_tick(&mut st);
            if st.next >= st.track.end() {
                // Everything listed so far is out. A live stream waits for
                // the next reload unless it is over.
                match st.live.as_ref() {
                    None => return Ok(None),
                    Some(l) if l.fatal.is_some() => {
                        let e = l.fatal.clone().unwrap_or_default();
                        return Err(io::Error::other(e));
                    }
                    Some(l) if l.ended => return Ok(None),
                    Some(_) => {}
                }
            } else {
                self.top_up(&mut st);
                let idx = st.next;
                if st.ready.contains_key(&idx) {
                    Self::emit(&mut st, idx);
                    Self::evict(&mut st);
                    continue;
                }
                if let Some(e) = st.failed.get(&idx).cloned() {
                    // One lost segment is a glitch, not the end of the film.
                    let dur = st.track.get(idx).map_or(Duration::ZERO, |s| s.dur);
                    let total = st.track.total;
                    let live_run = st.live.as_ref().map(|l| l.fail_run);
                    let affordable = match live_run {
                        // Live: a few in a row, then the stream is gone.
                        Some(run) => run < LIVE_MAX_SKIPS,
                        None => (st.skipped + dur) * SKIP_SHARE < total,
                    };
                    if skippable(&e) && affordable {
                        match st.live.as_mut() {
                            Some(l) => l.fail_run += 1,
                            None => st.skipped += dur,
                        }
                        st.failed.remove(&idx);
                        st.next = idx + 1;
                        st.need_discont = true;
                        st.last_init = None;
                        Self::evict(&mut st);
                        continue;
                    }
                    let e = if live_run.is_some() && skippable(&e) {
                        "live ended".to_owned()
                    } else {
                        e
                    };
                    return Err(io::Error::other(e));
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            st = sh
                .cv
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn buffered_until(&self) -> Option<Duration> {
        let st = self.sh.lock();
        let track = &st.track;
        let first = track.get(st.next)?;
        let mut t = first.start;
        let mut i = st.next;
        while st.ready.contains_key(&i) {
            let Some(s) = track.get(i) else { break };
            t = s.start + s.dur;
            i += 1;
        }
        Some(t)
    }

    fn variants(&self) -> Vec<VariantInfo> {
        self.sh.variants.clone()
    }

    fn current_variant(&self) -> Option<usize> {
        (!self.sh.variants.is_empty()).then(|| self.sh.lock().variant)
    }

    fn select_variant(&mut self, v: Option<usize>) {
        let mut st = self.sh.lock();
        let Some(i) = v else {
            st.auto = true;
            return;
        };
        st.auto = false;
        if i >= self.sh.variants.len() || i == st.variant || st.switching == Some(i) {
            return;
        }
        st.rebuild = false;
        self.start_switch(&mut st, i);
    }

    fn wants_downgrade(&mut self) -> bool {
        let mut st = self.sh.lock();
        std::mem::take(&mut st.rebuild)
    }
}

#[allow(dead_code)]
fn _assert_send(r: SegmentedReader) -> impl Send {
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn abr_policy_table() {
        // variants: 0 = 500k, 1 = 1M, 2 = 2.5M
        let all = [(0usize, 500_000u64), (1, 1_000_000), (2, 2_500_000)];
        struct C {
            now: u64,
            buf: u64,
            bps: Option<f64>,
            cur: usize,
            pressure: bool,
            want: AbrChoice,
        }
        let c = |now, buf, bps: Option<f64>, cur, pressure, want| C {
            now,
            buf,
            bps,
            cur,
            pressure,
            want,
        };
        use AbrChoice::{Stay, Switch};
        let cases = [
            // Short buffer and speed under 1.2x the bitrate: lower.
            c(10, 5, Some(2.0e6), 2, false, Switch(1)),
            // The best lower one the speed carries (1.2 * 500k fits 800k).
            c(10, 5, Some(0.8e6), 2, false, Switch(0)),
            // Speed fine for the current one: stay.
            c(10, 5, Some(3.1e6), 2, false, Stay),
            // Plenty buffered: stay even when slow.
            c(10, 15, Some(1.0e6), 2, false, Stay),
            // Lowest already: nothing to do.
            c(10, 1, Some(1.0e5), 0, false, Stay),
            // Retry pressure lowers before the segment gives up.
            c(10, 40, Some(9.0e6), 2, true, Switch(1)),
            // No estimate yet: no speed-based decision.
            c(10, 1, None, 2, false, Stay),
        ];
        for (n, k) in cases.iter().enumerate() {
            let mut p = AbrPolicy::default();
            let got = p.decide(&AbrInput {
                now: secs(k.now),
                buffer_ahead: secs(k.buf),
                bps: k.bps,
                current: k.cur,
                cands: &all,
                retry_pressure: k.pressure,
            });
            assert_eq!(got, k.want, "case {n}");
        }
    }

    #[test]
    fn abr_raises_only_after_hold_and_at_most_once_a_minute() {
        let all = [(0usize, 500_000u64), (1, 1_000_000)];
        let mut p = AbrPolicy::default();
        let at = |p: &mut AbrPolicy, now: u64, buf: u64, bps: f64, cur: usize| {
            p.decide(&AbrInput {
                now: secs(now),
                buffer_ahead: secs(buf),
                bps: Some(bps),
                current: cur,
                cands: &all,
                retry_pressure: false,
            })
        };
        // 1.5x of 1M = 1.5M. Condition first holds at t=100.
        assert_eq!(at(&mut p, 100, 35, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 110, 35, 2.0e6, 0), AbrChoice::Stay);
        // A dip resets the hold.
        assert_eq!(at(&mut p, 115, 20, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 116, 35, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 135, 35, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 136, 35, 2.0e6, 0), AbrChoice::Switch(1));
        // Down again at once is refused by the gap, then allowed.
        assert_eq!(at(&mut p, 137, 2, 0.5e6, 1), AbrChoice::Stay);
        assert_eq!(at(&mut p, 141, 2, 0.5e6, 1), AbrChoice::Switch(0));
        // Up needs a minute since the last switch.
        assert_eq!(at(&mut p, 142, 35, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 165, 35, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 200, 35, 2.0e6, 0), AbrChoice::Stay);
        assert_eq!(at(&mut p, 201, 35, 2.0e6, 0), AbrChoice::Switch(1));
    }

    #[test]
    fn codec_compatibility() {
        let h = Some("avc1.64001f,mp4a.40.2");
        assert!(compatible(h, Some("avc1.4d401e, mp4a.40.5"), false));
        assert!(!compatible(h, Some("hvc1.1.6.L93,mp4a.40.2"), true));
        assert!(!compatible(h, None, true));
        assert!(compatible(None, None, true));
        assert!(!compatible(None, None, false));
    }

    #[test]
    fn meter_counts_concurrent_downloads_once() {
        let t0 = Instant::now();
        let mut m = Meter::default();
        // Four downloads overlap for one second and move 1 MB together.
        for _ in 0..4 {
            m.begin(t0);
        }
        m.add(1_000_000);
        for _ in 0..4 {
            m.end(t0 + secs(1));
        }
        let bps = m.sample(t0 + secs(5)).unwrap();
        assert!((bps - 8.0e6).abs() < 1.0, "{bps}");
        // Idle time does not dilute the estimate.
        assert_eq!(m.sample(t0 + secs(60)), Some(bps));
    }

    #[test]
    fn only_transient_failures_are_skippable() {
        assert!(skippable("stalled") && skippable("http 404") && skippable("http 503"));
        assert!(!skippable("unsupported stream") && !skippable("proxy required"));
        assert!(!skippable("no media") && !skippable("cancelled"));
    }

    #[test]
    fn only_three_headers_pass() {
        let h = allowed_headers(vec![
            ("User-Agent".into(), "x".into()),
            ("authorization".into(), "secret".into()),
            ("X-Api-Key".into(), "secret".into()),
            ("REFERER".into(), "https://a.invalid/".into()),
            ("Origin".into(), "bad\r\nvalue".into()),
        ]);
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["User-Agent", "REFERER"]);
    }

    #[test]
    fn urls_are_joined_and_judged() {
        let o = "https://cdn.example.invalid/a/m.m3u8";
        assert_eq!(
            resolve(o, "../b/s.ts", o).unwrap(),
            "https://cdn.example.invalid/b/s.ts"
        );
        assert!(resolve(o, "http://127.0.0.1/x", o).is_err());
        assert!(resolve(o, "ftp://cdn.example.invalid/x", o).is_err());
        assert!(
            resolve(
                "http://127.0.0.1/m.m3u8",
                "http://10.0.0.2/x",
                "http://127.0.0.1/m.m3u8"
            )
            .is_ok()
        );
    }

    #[test]
    fn decrypt_rejects_odd_lengths_and_bad_padding() {
        let (k, iv) = ([1u8; 16], [2u8; 16]);
        assert!(decrypt(&k, &iv, &[0u8; 15]).is_err());
        assert!(decrypt(&k, &iv, &[0u8; 16]).is_err());
    }

    fn live_seg(seq: u64) -> Seg {
        Seg {
            part: Part {
                url: format!("http://x.invalid/s{seq}.ts"),
                range: None,
            },
            dur: Duration::from_secs(2),
            start: Duration::ZERO,
            init: None,
            key: None,
            seq,
            discont: false,
        }
    }

    #[test]
    fn a_live_track_stays_bounded_and_indices_stay_absolute() {
        let mut st = new_state(Track::new(vec![live_seg(0), live_seg(1)]), 0, false);
        st.live = Some(LiveState {
            target: Duration::from_secs(2),
            last_seq: 1,
            epoch_idx: 0,
            ended: false,
            fatal: None,
            fail_run: 0,
            task: None,
        });
        // 5000 reloads of a 6-segment window; the consumer follows.
        for n in 1..5000u64 {
            let first = n;
            let win: Vec<Seg> = (first..first + 6).map(live_seg).collect();
            let added = live_merge(&mut st, first, win);
            assert!(added >= 1);
            st.next = st.track.end();
            assert!(
                st.track.segs.len() <= 6 + LIVE_KEEP_BEHIND + 6,
                "{}",
                st.track.segs.len()
            );
        }
        // Absolute indices survive trimming; the newest segment is last.
        let end = st.track.end();
        assert!(end > 4000);
        assert_eq!(st.track.get(end - 1).unwrap().seq, 5004);
        assert!(st.track.get(0).is_none());
        // Timeline is cumulative even though the head was dropped.
        assert!(st.track.total >= Duration::from_secs(2 * 4000));
        // Replaying an old window adds nothing.
        let old: Vec<Seg> = (5000..5005).map(live_seg).collect();
        assert_eq!(live_merge(&mut st, 5000, old), 0);
    }

    #[test]
    fn track_prefix_sums_and_binary_search() {
        let seg = |d: u64| Seg {
            part: Part {
                url: String::new(),
                range: None,
            },
            dur: Duration::from_secs(d),
            start: Duration::ZERO,
            init: None,
            key: None,
            seq: 0,
            discont: false,
        };
        let t = Track::new(vec![seg(2), seg(2), seg(3)]);
        assert_eq!(t.total, Duration::from_secs(7));
        assert_eq!(t.index_at(Duration::from_millis(1999)), 0);
        assert_eq!(t.index_at(Duration::from_secs(2)), 1);
        assert_eq!(t.index_at(Duration::from_millis(4500)), 2);
        assert_eq!(t.index_at(Duration::from_secs(99)), 2);
    }
}
