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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

/// Size of one cached block and of one `Range` request.
pub const BLOCK: u64 = 1 << 20;
/// Memory kept for already fetched blocks (read-ahead included).
const CACHE_BLOCKS: usize = 64;
/// Blocks fetched ahead of the read position.
const AHEAD: u64 = 3;
/// Requests allowed in flight at once.
const MAX_INFLIGHT: usize = AHEAD as usize + 2;
/// Larger resources are refused (also keeps every offset sum in range).
const MAX_LEN: u64 = 1 << 40;
// Five tries, then we accept that the server and we want different things.
const RETRIES: u32 = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Fixed reason strings; the caller maps them through `errors::classify`.
pub const NOT_SEEKABLE: &str = "not seekable";
pub const SOURCE_CHANGED: &str = "source changed";

#[derive(Default)]
struct Cache {
    blocks: HashMap<u64, (Arc<Vec<u8>>, u64)>,
    inflight: HashSet<u64>,
    tick: u64,
    failed: Option<String>,
}

struct Shared {
    client: reqwest::Client,
    url: String,
    len: u64,
    /// Strong ETag or Last-Modified, sent as `If-Range`.
    validator: Option<String>,
    cache: Mutex<Cache>,
    notify: Notify,
    /// Running block fetches, aborted when the reader is dropped.
    tasks: Mutex<Vec<AbortHandle>>,
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

/// Read a body of at most `want` bytes; anything longer is refused.
async fn read_capped(mut resp: reqwest::Response, want: usize) -> Option<Vec<u8>> {
    if resp.content_length().is_some_and(|l| l > want as u64) {
        return None;
    }
    let mut out = Vec::with_capacity(want.min(BLOCK as usize));
    while let Ok(Some(c)) = resp.chunk().await {
        if out.len() + c.len() > want {
            return None;
        }
        out.extend_from_slice(&c);
    }
    Some(out)
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let s = resp.headers().get("retry-after")?.to_str().ok()?;
    s.trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n.min(30)))
}

/// Open `url`. `Err(NOT_SEEKABLE)` when the server does not honour `Range`
/// (the caller then falls back to the save-while-playing path).
pub async fn open(handle: Handle, client: reqwest::Client, url: String) -> Result<Opened, String> {
    let resp = client
        .get(&url)
        .header("Range", format!("bytes=0-{}", BLOCK - 1))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("error sending request: {}", e.without_url()))?;
    let status = resp.status();
    if status.as_u16() == 200 {
        return Err(NOT_SEEKABLE.into());
    }
    if status.as_u16() != 206 {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let (a, b, len) = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range)
        .ok_or_else(|| NOT_SEEKABLE.to_string())?;
    // The first block must be exactly what was asked for (or the whole file).
    if a != 0 || b + 1 != BLOCK.min(len) || len > MAX_LEN {
        return Err(NOT_SEEKABLE.into());
    }
    let validator = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .filter(|e| !e.starts_with("W/"))
        .or_else(|| {
            resp.headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok())
        })
        .map(str::to_owned);
    let head = read_capped(resp, BLOCK as usize)
        .await
        .filter(|h| h.len() as u64 == b + 1)
        .ok_or_else(|| NOT_SEEKABLE.to_string())?;
    if jizzcraft_core::manifest::looks_like_manifest(&head) {
        return Err("refused: this refers to network media".into());
    }
    let mut cache = Cache::default();
    cache.blocks.insert(0, (Arc::new(head), 0));
    let shared = Arc::new(Shared {
        client,
        url,
        len,
        validator,
        cache: Mutex::new(cache),
        notify: Notify::new(),
        tasks: Mutex::new(Vec::new()),
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

async fn fetch_block(sh: Arc<Shared>, idx: u64) {
    let start = idx * BLOCK;
    let end = (start + BLOCK).min(sh.len) - 1;
    let want = (end - start + 1) as usize;
    let mut fatal: Option<String> = None;
    let mut got: Option<Vec<u8>> = None;
    for attempt in 0..RETRIES {
        // The kill switch applies to a stream that is already open.
        if crate::service::proxy_blocked() {
            fatal = Some("proxy required".into());
            break;
        }
        let mut req = sh
            .client
            .get(&sh.url)
            .header("Range", format!("bytes={start}-{end}"))
            .timeout(REQUEST_TIMEOUT);
        if let Some(v) = &sh.validator {
            req = req.header("If-Range", v);
        }
        let mut wait = Duration::from_millis(500 << attempt);
        if let Ok(resp) = req.send().await {
            {
                let code = resp.status().as_u16();
                match code {
                    206 => {
                        let range_ok = resp
                            .headers()
                            .get("content-range")
                            .and_then(|v| v.to_str().ok())
                            .and_then(parse_content_range)
                            == Some((start, end, sh.len));
                        if !range_ok {
                            fatal = Some(SOURCE_CHANGED.into());
                            break;
                        }
                        if let Some(b) = read_capped(resp, want).await
                            && b.len() == want
                        {
                            got = Some(b);
                            break;
                        }
                    }
                    // The server ignored the range or the entity changed.
                    200 => {
                        fatal = Some(SOURCE_CHANGED.into());
                        break;
                    }
                    429 | 503 => {
                        if let Some(d) = retry_after(&resp) {
                            wait = d;
                        }
                    }
                    408 | 500..=599 => {}
                    c => {
                        fatal = Some(format!("HTTP {c}"));
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(wait).await;
    }
    let mut c = sh.cache.lock().unwrap_or_else(|e| e.into_inner());
    c.inflight.remove(&idx);
    match got {
        Some(b) => {
            c.tick += 1;
            let t = c.tick;
            c.blocks.insert(idx, (Arc::new(b), t));
            // Evict the least recently used blocks, never the window ahead.
            // Seniority counts for nothing here; only recent attention does.
            while c.blocks.len() > CACHE_BLOCKS {
                let victim = c
                    .blocks
                    .iter()
                    .filter(|(k, _)| **k < idx.saturating_sub(1) || **k > idx + AHEAD)
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
        None => {
            c.failed
                .get_or_insert_with(|| fatal.unwrap_or_else(|| "timed out".into()));
        }
    }
    drop(c);
    sh.notify.notify_one();
}

impl RangeReader {
    fn want(&self, c: &mut Cache, idx: u64) {
        let last = (self.shared.len - 1) / BLOCK;
        if idx > last || c.blocks.contains_key(&idx) || c.inflight.contains(&idx) {
            return;
        }
        // Read-ahead is optional; the block being read is always fetched.
        if c.inflight.len() >= MAX_INFLIGHT && idx > self.pos / BLOCK {
            return;
        }
        c.inflight.insert(idx);
        let h = self
            .handle
            .spawn(fetch_block(Arc::clone(&self.shared), idx));
        let mut t = self.shared.tasks.lock().unwrap_or_else(|e| e.into_inner());
        t.retain(|a| !a.is_finished());
        t.push(h.abort_handle());
    }

    fn lookup(&mut self, idx: u64) -> io::Result<Option<Arc<Vec<u8>>>> {
        let mut c = self.shared.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(msg) = &c.failed {
            return Err(io::Error::other(msg.clone()));
        }
        c.tick += 1;
        let t = c.tick;
        let hit = c.blocks.get_mut(&idx).map(|(b, last)| {
            *last = t;
            Arc::clone(b)
        });
        self.want(&mut c, idx);
        if hit.is_some() {
            for k in 1..=AHEAD {
                self.want(&mut c, idx + k);
            }
        }
        Ok(hit)
    }
}

impl Drop for RangeReader {
    fn drop(&mut self) {
        // Closing the player cancels every request still in flight.
        let t = self.shared.tasks.lock().unwrap_or_else(|e| e.into_inner());
        for a in t.iter() {
            a.abort();
        }
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.shared.len || buf.is_empty() {
            return Ok(0);
        }
        let idx = self.pos / BLOCK;
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
        let off = (self.pos - idx * BLOCK) as usize;
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
        Ok(self.pos)
    }
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
}
