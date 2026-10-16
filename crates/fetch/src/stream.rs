//! Seekable HTTP(S) playback without writing to disk.
//!
//! [`RangeReader`] is a blocking `Read + Seek` over a remote file, served by
//! `Range` requests into a bounded in-memory block cache with a little
//! read-ahead. It is the only thing the player sees: bytes reach the player
//! through `Player::open_stream`, never a URL.
//!
//! Contract with the feeder (same as the torrent reader): `WouldBlock` means
//! "not here yet, ask again"; any other error is final.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

/// Smallest cached unit and smallest `Range` request.
pub const UNIT: u64 = 256 << 10;
/// Requests span 1..=16 units (256 KiB..4 MiB), chosen from the throughput.
const MAX_SPAN: u64 = 16;
/// Span used before any throughput is known (1 MiB).
const FIRST_SPAN: u64 = 4;
/// Memory kept for already fetched units (64 MiB, read-ahead included).
const CACHE_UNITS: usize = 256;
/// Units fetched ahead of the read position (3 MiB).
const AHEAD_UNITS: u64 = 12;
/// Requests allowed in flight at once.
const MAX_REQS: usize = 4;
/// Larger resources are refused (also keeps every offset sum in range).
const MAX_LEN: u64 = 1 << 40;

/// Connection setup. Callers build their client with this `connect_timeout`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Time to the first byte of a response (headers).
pub const TTFB_TIMEOUT: Duration = Duration::from_secs(20);
/// A body that sends nothing for this long is dead. A slow body that keeps
/// moving never times out.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one block is retried (jittered backoff) before the error is shown.
pub const RETRY_BUDGET: Duration = Duration::from_secs(60);

/// Fixed reason strings; the caller maps them through `errors::classify`.
pub const NOT_SEEKABLE: &str = "not seekable";
pub const SOURCE_CHANGED: &str = "source changed";

/// Asks the origin of the stream for a fresh address (expired signed URL).
/// The receiver yields the new address, or a reason.
pub type ReopenFn = Arc<dyn Fn() -> Receiver<Result<String, String>> + Send + Sync>;

struct Req {
    id: u64,
    /// Unit range `[start, end)`.
    start: u64,
    end: u64,
    abort: Option<AbortHandle>,
}

#[derive(Default)]
struct Cache {
    blocks: HashMap<u64, (Arc<Vec<u8>>, u64)>,
    inflight: HashSet<u64>,
    reqs: Vec<Req>,
    next_id: u64,
    tick: u64,
    pos_unit: u64,
    /// Unit range `[start, end)` that could not be fetched, with the reason.
    /// A later seek clears it; `global` errors stay (source changed, proxy).
    failed: Option<(u64, u64, String, bool)>,
}

struct Shared {
    client: reqwest::Client,
    url: Mutex<String>,
    len: u64,
    /// Strong ETag or Last-Modified, sent as `If-Range`.
    validator: Option<String>,
    cache: Mutex<Cache>,
    notify: Notify,
    reopen: Option<ReopenFn>,
    reopen_lock: tokio::sync::Mutex<()>,
    /// Smoothed throughput in bytes per second (0 = unknown).
    bps: AtomicU64,
}

pub struct RangeReader {
    handle: Handle,
    shared: Arc<Shared>,
    pos: u64,
}

/// What `open` learned about the resource.
pub struct Opened {
    pub reader: RangeReader,
    pub len: u64,
}

/// `Content-Range: bytes 0-1048575/12345` -> `(0, 1048575, 12345)`.
fn parse_content_range(v: &str) -> Option<(u64, u64, u64)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (span, total) = rest.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    let (a, b, total): (u64, u64, u64) = (
        a.trim().parse().ok()?,
        b.trim().parse().ok()?,
        total.trim().parse().ok()?,
    );
    (total > 0 && a <= b && b < total).then_some((a, b, total))
}

pub(crate) fn content_range(resp: &reqwest::Response) -> Option<(u64, u64, u64)> {
    resp.headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range)
}

/// Strong ETag, else Last-Modified.
pub(crate) fn validator_of(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .filter(|e| !e.starts_with("W/"))
        .or_else(|| {
            resp.headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok())
        })
        .map(str::to_owned)
}

#[derive(Debug, PartialEq, Eq)]
enum BodyFail {
    TooBig,
    Stalled,
    Broken,
}

/// Read a body of at most `want` bytes. No total deadline: only a silence of
/// [`STALL_TIMEOUT`] ends it, so a slow link that keeps moving is fine.
async fn read_capped(mut resp: reqwest::Response, want: usize) -> Result<Vec<u8>, BodyFail> {
    if resp.content_length().is_some_and(|l| l > want as u64) {
        return Err(BodyFail::TooBig);
    }
    let mut out = Vec::with_capacity(want.min((UNIT * MAX_SPAN) as usize));
    loop {
        match tokio::time::timeout(STALL_TIMEOUT, resp.chunk()).await {
            Err(_) => return Err(BodyFail::Stalled),
            Ok(Err(_)) => return Err(BodyFail::Broken),
            Ok(Ok(None)) => return Ok(out),
            Ok(Ok(Some(c))) => {
                if out.len() + c.len() > want {
                    return Err(BodyFail::TooBig);
                }
                if crate::service::proxy_blocked() {
                    return Err(BodyFail::Broken);
                }
                out.extend_from_slice(&c);
            }
        }
    }
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let s = resp.headers().get("retry-after")?.to_str().ok()?;
    s.trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n.min(30)))
}

/// A short, URL-free reason for a failed send. A certificate that is not yet
/// valid or already expired means the computer's clock is off.
pub fn net_error(e: &reqwest::Error) -> String {
    let mut chain = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(x) = cur {
        chain.push_str(&x.to_string().to_ascii_lowercase());
        chain.push(' ');
        cur = x.source();
    }
    if chain.contains("certificate")
        && (chain.contains("expired")
            || chain.contains("not valid yet")
            || chain.contains("notvalidyet"))
    {
        "clock skew".into()
    } else if e.is_timeout() {
        "server timeout".into()
    } else if e.is_connect() {
        "unreachable: connection failed".into()
    } else {
        "error sending request".into()
    }
}

/// Send with a time-to-first-byte limit (connect time is the client's).
pub async fn send_ttfb(req: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    match tokio::time::timeout(TTFB_TIMEOUT, req.send()).await {
        Err(_) => Err("server timeout".into()),
        Ok(Err(e)) => Err(net_error(&e)),
        Ok(Ok(r)) => Ok(r),
    }
}

pub(crate) fn status_reason(code: u16) -> String {
    match code {
        429 | 503 => format!("server busy (HTTP {code})"),
        300..=399 => format!("redirect refused (HTTP {code})"),
        c => format!("HTTP {c}"),
    }
}

/// Open `url`. `Err(NOT_SEEKABLE)` when the server does not honour `Range`
/// (the caller then falls back to the save-while-playing path).
pub async fn open(handle: Handle, client: reqwest::Client, url: String) -> Result<Opened, String> {
    open_with(handle, client, url, None).await
}

/// [`open`] with a hook that renews the address when it expires mid-stream.
pub async fn open_with(
    handle: Handle,
    client: reqwest::Client,
    url: String,
    reopen: Option<ReopenFn>,
) -> Result<Opened, String> {
    let resp = send_ttfb(
        client
            .get(&url)
            .header("Range", format!("bytes=0-{}", UNIT - 1)),
    )
    .await?;
    let status = resp.status();
    if status.as_u16() == 200 {
        return Err(NOT_SEEKABLE.into());
    }
    if status.as_u16() != 206 {
        return Err(status_reason(status.as_u16()));
    }
    let (a, b, len) = content_range(&resp).ok_or_else(|| NOT_SEEKABLE.to_string())?;
    // The first block must be exactly what was asked for (or the whole file).
    if a != 0 || b + 1 != UNIT.min(len) || len > MAX_LEN {
        return Err(NOT_SEEKABLE.into());
    }
    let validator = validator_of(&resp);
    let head = read_capped(resp, UNIT as usize)
        .await
        .map_err(|f| match f {
            BodyFail::Stalled => "server timeout".to_string(),
            _ => NOT_SEEKABLE.to_string(),
        })
        .and_then(|h| {
            if h.len() as u64 == b + 1 {
                Ok(h)
            } else {
                Err(NOT_SEEKABLE.to_string())
            }
        })?;
    if jizzcraft_core::manifest::looks_like_manifest(&head) {
        return Err("refused: this refers to network media".into());
    }
    let mut cache = Cache::default();
    cache.blocks.insert(0, (Arc::new(head), 0));
    let shared = Arc::new(Shared {
        client,
        url: Mutex::new(url),
        len,
        validator,
        cache: Mutex::new(cache),
        notify: Notify::new(),
        reopen,
        reopen_lock: tokio::sync::Mutex::new(()),
        bps: AtomicU64::new(0),
    });
    Ok(Opened {
        reader: RangeReader {
            handle,
            shared,
            pos: 0,
        },
        len,
    })
}

enum Try {
    Done(Vec<u8>),
    /// Worth another go; carries the server's wait and the failure class.
    Retry(Option<Duration>, &'static str),
    Fatal(String),
    Expired,
}

async fn try_once(sh: &Shared, url: &str, b0: u64, b1: u64) -> Try {
    let want = (b1 - b0 + 1) as usize;
    let mut req = sh
        .client
        .get(url)
        .header("Range", format!("bytes={b0}-{b1}"));
    if let Some(v) = &sh.validator {
        req = req.header("If-Range", v);
    }
    let resp = match send_ttfb(req).await {
        Ok(r) => r,
        Err(e) if e == "clock skew" => return Try::Fatal(e),
        Err(e) if e == "server timeout" => return Try::Retry(None, "server timeout"),
        Err(_) => return Try::Retry(None, "unreachable"),
    };
    match resp.status().as_u16() {
        206 => {
            if content_range(&resp) != Some((b0, b1, sh.len)) {
                return Try::Fatal(SOURCE_CHANGED.into());
            }
            if let (Some(want_v), Some(got_v)) = (&sh.validator, validator_of(&resp))
                && *want_v != got_v
            {
                return Try::Fatal(SOURCE_CHANGED.into());
            }
            match read_capped(resp, want).await {
                Ok(b) if b.len() == want => Try::Done(b),
                Err(BodyFail::Stalled) => Try::Retry(None, "server timeout"),
                _ => Try::Retry(None, "unreachable"),
            }
        }
        // The server ignored the range or the entity changed.
        200 => Try::Fatal(SOURCE_CHANGED.into()),
        401 | 403 | 410 => Try::Expired,
        429 | 503 => Try::Retry(retry_after(&resp), "server busy"),
        408 | 500..=599 => Try::Retry(retry_after(&resp), "unreachable"),
        c => Try::Fatal(status_reason(c)),
    }
}

/// The renewed address must be the same entity: same length, same validator.
async fn verify_reopened(sh: &Shared, url: &str) -> bool {
    let Ok(resp) = send_ttfb(sh.client.get(url).header("Range", "bytes=0-0")).await else {
        return false;
    };
    if resp.status().as_u16() != 206 {
        return false;
    }
    if content_range(&resp).map(|c| c.2) != Some(sh.len) {
        return false;
    }
    match (&sh.validator, validator_of(&resp)) {
        (Some(a), Some(b)) => *a == b,
        _ => true,
    }
}

/// Returns true when a fresh address is now in `sh.url`.
async fn renew_url(sh: &Shared, used: &str) -> bool {
    let Some(f) = &sh.reopen else { return false };
    let _g = sh.reopen_lock.lock().await;
    // Another request already renewed it while we waited.
    if *sh.url.lock().unwrap_or_else(|e| e.into_inner()) != used {
        return true;
    }
    let rx = f();
    let got = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(20)))
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|r| r.ok());
    match got {
        Some(new) if verify_reopened(sh, &new).await => {
            *sh.url.lock().unwrap_or_else(|e| e.into_inner()) = new;
            true
        }
        _ => false,
    }
}

/// Cheap jitter in 0.75..1.25 without a random-number dependency.
pub(crate) fn jitter(d: Duration, salt: u64) -> Duration {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut x = n ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let f = 750 + (x % 500);
    d * f as u32 / 1000
}

async fn fetch_span(sh: Arc<Shared>, id: u64, start: u64, end: u64) {
    let b0 = start * UNIT;
    let b1 = (end * UNIT).min(sh.len) - 1;
    let began = Instant::now();
    let mut first_fail: Option<Instant> = None;
    let mut attempt = 0u32;
    let mut renewed = false;
    let outcome: Result<Vec<u8>, String> = loop {
        if crate::service::proxy_blocked() {
            break Err("proxy required".into());
        }
        let url = sh.url.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (wait, kind) = match try_once(&sh, &url, b0, b1).await {
            Try::Done(b) => break Ok(b),
            Try::Fatal(e) => break Err(e),
            Try::Expired => {
                if !renewed {
                    renewed = true;
                    if renew_url(&sh, &url).await {
                        continue;
                    }
                }
                break Err("expired".into());
            }
            Try::Retry(w, k) => (w, k),
        };
        let t0 = *first_fail.get_or_insert_with(Instant::now);
        let backoff = Duration::from_millis(500u64 << attempt.min(4)).min(Duration::from_secs(8));
        let wait = wait.unwrap_or_else(|| jitter(backoff, b0 ^ u64::from(attempt)));
        attempt += 1;
        if t0.elapsed() + wait > RETRY_BUDGET {
            break Err(match kind {
                "server busy" => "server busy".into(),
                "server timeout" => "server timeout".into(),
                _ => "unreachable".to_string(),
            });
        }
        tokio::time::sleep(wait).await;
    };
    if let Ok(b) = &outcome {
        let secs = began.elapsed().as_secs_f64().max(0.01);
        let now = (b.len() as f64 / secs) as u64;
        let old = sh.bps.load(Ordering::Relaxed);
        sh.bps.store(
            if old == 0 { now } else { (old * 3 + now) / 4 },
            Ordering::Relaxed,
        );
    }
    {
        let mut c = sh.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.reqs.retain(|r| r.id != id);
        for u in start..end {
            c.inflight.remove(&u);
        }
        match outcome {
            Ok(b) => {
                for (k, part) in b.chunks(UNIT as usize).enumerate() {
                    c.tick += 1;
                    let t = c.tick;
                    c.blocks
                        .insert(start + k as u64, (Arc::new(part.to_vec()), t));
                }
                // Evict the least recently used units, never the window around
                // the read position.
                let p = c.pos_unit;
                while c.blocks.len() > CACHE_UNITS {
                    let victim = c
                        .blocks
                        .iter()
                        .filter(|(k, _)| **k + 4 < p || **k > p + AHEAD_UNITS + MAX_SPAN)
                        .min_by_key(|(_, (_, t))| *t)
                        .map(|(k, _)| *k);
                    match victim {
                        Some(k) => {
                            c.blocks.remove(&k);
                        }
                        None => break,
                    }
                }
            }
            Err(e) => {
                let global = e == SOURCE_CHANGED || e == "proxy required" || e == "clock skew";
                if c.failed.is_none() || global {
                    c.failed = Some((start, end, e, global));
                }
            }
        }
    }
    sh.notify.notify_one();
}

impl RangeReader {
    fn span_units(&self) -> u64 {
        match self.shared.bps.load(Ordering::Relaxed) {
            0 => FIRST_SPAN,
            // About two seconds of data per request.
            b => (b * 2 / UNIT).clamp(1, MAX_SPAN),
        }
    }

    fn plan(&self, c: &mut Cache, unit: u64, required: bool) {
        let last = (self.shared.len - 1) / UNIT;
        if unit > last || c.blocks.contains_key(&unit) || c.inflight.contains(&unit) {
            return;
        }
        // Read-ahead is optional; the block being read is always fetched.
        if c.reqs.len() >= MAX_REQS && !required {
            return;
        }
        let span = self.span_units();
        let mut end = unit + 1;
        while end < unit + span
            && end <= last
            && !c.blocks.contains_key(&end)
            && !c.inflight.contains(&end)
        {
            end += 1;
        }
        for u in unit..end {
            c.inflight.insert(u);
        }
        c.next_id += 1;
        let id = c.next_id;
        c.reqs.push(Req {
            id,
            start: unit,
            end,
            abort: None,
        });
        let h = self
            .handle
            .spawn(fetch_span(Arc::clone(&self.shared), id, unit, end));
        if let Some(r) = c.reqs.iter_mut().find(|r| r.id == id) {
            r.abort = Some(h.abort_handle());
        }
    }

    fn lookup(&mut self, idx: u64) -> io::Result<Option<Arc<Vec<u8>>>> {
        let mut c = self.shared.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.tick += 1;
        let t = c.tick;
        c.pos_unit = idx;
        let hit = c.blocks.get_mut(&idx).map(|(b, last)| {
            *last = t;
            Arc::clone(b)
        });
        if hit.is_none()
            && let Some((a, b, msg, global)) = &c.failed
            && (*global || (*a..*b).contains(&idx))
        {
            return Err(io::Error::other(msg.clone()));
        }
        self.plan(&mut c, idx, true);
        if hit.is_some() {
            for k in 1..=AHEAD_UNITS {
                self.plan(&mut c, idx + k, false);
            }
        }
        Ok(hit)
    }
}

impl Drop for RangeReader {
    fn drop(&mut self) {
        // Closing the player cancels every request still in flight.
        let c = self.shared.cache.lock().unwrap_or_else(|e| e.into_inner());
        for r in c.reqs.iter() {
            if let Some(a) = &r.abort {
                a.abort();
            }
        }
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.shared.len || buf.is_empty() {
            return Ok(0);
        }
        let idx = self.pos / UNIT;
        let mut block = self.lookup(idx)?;
        if block.is_none() {
            let sh = Arc::clone(&self.shared);
            let _ = self.handle.block_on(async {
                tokio::time::timeout(Duration::from_millis(100), sh.notify.notified()).await
            });
            block = self.lookup(idx)?;
        }
        let Some(b) = block else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        let off = (self.pos - idx * UNIT) as usize;
        let n = buf.len().min(b.len().saturating_sub(off));
        buf[..n].copy_from_slice(&b[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let np = match pos {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(d) => self.shared.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if np < 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.pos = (np as u64).min(self.shared.len);
        let unit = self.pos / UNIT;
        let mut c = self.shared.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.pos_unit = unit;
        // Requests for places nobody will read any more are abandoned.
        let mut gone = Vec::new();
        c.reqs.retain(|r| {
            let needed = r.end > unit && r.start <= unit + AHEAD_UNITS;
            if !needed {
                if let Some(a) = &r.abort {
                    a.abort();
                }
                gone.push((r.start, r.end));
            }
            needed
        });
        for (a, b) in gone {
            for u in a..b {
                c.inflight.remove(&u);
            }
        }
        // A seek is a fresh start: transient failures are forgotten.
        if c.failed.as_ref().is_some_and(|f| !f.3) {
            c.failed = None;
        }
        Ok(self.pos)
    }
}

// ---- Resume position: memory only, never on disk -------------------------

const RESUME_ENTRIES: usize = 64;

/// Least-recently-used list of resume positions, newest last.
static RESUME: Mutex<Vec<([u8; 32], Duration)>> = Mutex::new(Vec::new());

/// Key from host and path only (the query often carries expiring tokens),
/// through a process-keyed std hasher: nothing recognisable is kept, and the
/// keys differ on every run.
fn resume_key(url: &str) -> [u8; 32] {
    use std::hash::{BuildHasher, Hash, Hasher};
    use std::sync::OnceLock;
    static STATE: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    let st = STATE.get_or_init(std::collections::hash_map::RandomState::new);
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let mut out = [0u8; 32];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut h = st.build_hasher();
        (i as u8).hash(&mut h);
        base.hash(&mut h);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

pub fn resume_get(url: &str) -> Option<Duration> {
    let k = resume_key(url);
    let mut g = RESUME.lock().unwrap_or_else(|e| e.into_inner());
    let i = g.iter().position(|(a, _)| *a == k)?;
    let e = g.remove(i);
    let d = e.1;
    g.push(e);
    Some(d)
}

pub fn resume_put(url: &str, at: Duration) {
    let k = resume_key(url);
    let mut g = RESUME.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|(a, _)| *a != k);
    g.push((k, at));
    while g.len() > RESUME_ENTRIES {
        g.remove(0);
    }
}

pub fn resume_clear_all() {
    RESUME.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::parse_content_range as t;

    #[test]
    fn content_range_is_parsed_strictly() {
        assert_eq!(t("bytes 0-1023/4096"), Some((0, 1023, 4096)));
        assert_eq!(t("bytes 5-9/10"), Some((5, 9, 10)));
        assert_eq!(t("bytes 0-1023/*"), None);
        assert_eq!(t("garbage"), None);
        assert_eq!(t("bytes 0-0/0"), None);
        assert_eq!(t("bytes 9-5/10"), None);
        assert_eq!(t("bytes 0-10/10"), None);
    }

    #[test]
    fn resume_is_bounded_lru_and_ignores_the_query() {
        use super::*;
        resume_clear_all();
        resume_put("http://h/a.mp4?token=1", Duration::from_secs(5));
        assert_eq!(
            resume_get("http://h/a.mp4?token=2"),
            Some(Duration::from_secs(5))
        );
        for i in 0..100 {
            resume_put(&format!("http://h/{i}.mp4"), Duration::from_secs(i));
        }
        assert_eq!(resume_get("http://h/a.mp4"), None);
        assert_eq!(resume_get("http://h/99.mp4"), Some(Duration::from_secs(99)));
        assert_eq!(RESUME.lock().unwrap().len(), 64);
        resume_clear_all();
        assert_eq!(resume_get("http://h/99.mp4"), None);
    }
}
