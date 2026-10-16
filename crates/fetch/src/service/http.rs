//! Plain HTTP: indexer searches, source search, direct downloads.

use super::open::wait_cancel;
use super::{
    FetchService, MAX_RESPONSE, SEARCH_TIMEOUT, SearchOutcome, StreamReader, StreamTicket,
};
use super::{proxy_blocked, same_origin_policy};
use crate::config::{FetchConfig, Indexer};
use crate::redact::redact_url;
use crate::torznab::{self, SearchResult};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

impl FetchService {
    fn client() -> Result<reqwest::Client, String> {
        Self::builder()?
            .timeout(SEARCH_TIMEOUT)
            .redirect(same_origin_policy())
            .build()
            .map_err(|e| e.without_url().to_string())
    }

    pub(super) async fn http_get(url: String, cancel: Arc<AtomicBool>) -> Result<Vec<u8>, String> {
        let shown = redact_url(&url);
        let client = Self::client()?;
        let mut resp = tokio::select! {
            r = client.get(&url).send() => r.map_err(|e| format!("{shown}: {}", e.without_url()))?,
            _ = wait_cancel(cancel.clone()) => return Err("cancelled".into()),
        };
        if !resp.status().is_success() {
            return Err(format!("{shown}: HTTP {}", resp.status().as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("{shown}: {}", e.without_url()))?
        {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            body.extend_from_slice(&chunk);
            if body.len() > MAX_RESPONSE {
                return Err(format!("{shown}: response too large"));
            }
        }
        Ok(body)
    }

    /// Test an indexer's endpoint and key (Torznab `t=caps`).
    pub fn test_indexer(&self, ix: &Indexer) -> Receiver<Result<(), String>> {
        let (tx, rx) = channel();
        let url = torznab::build_caps_url(ix);
        self.rt().spawn(async move {
            let r = async {
                let body = Self::http_get(url, Arc::default()).await?;
                let xml = String::from_utf8_lossy(&body);
                torznab::parse_caps(&xml).map_err(|e| e.to_string())
            }
            .await;
            let _ = tx.send(r);
        });
        rx
    }

    /// Query every enabled indexer (or one). Per-indexer failures are
    /// returned as counts only, never with the query or key.
    pub fn search(
        &self,
        indexer: Option<u32>,
        query: &str,
        cats: &[u32],
        limit: u32,
        cancel: Arc<AtomicBool>,
    ) -> Receiver<SearchOutcome> {
        let (tx, rx) = channel();
        let targets: Vec<Indexer> = self
            .cfg
            .indexers
            .iter()
            .filter(|i| i.enabled && indexer.is_none_or(|id| id == i.id))
            .cloned()
            .collect();
        let defs: Vec<crate::sources::SourceDef> = if indexer.is_none() {
            self.cfg
                .sources
                .iter()
                .filter(|s| s.enabled)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let urls: Vec<(u32, String)> = targets
            .iter()
            .map(|i| (i.id, torznab::build_search_url(i, query, cats, limit)))
            .collect();
        let query_owned = query.to_owned();
        let cancel_src = cancel.clone();
        self.rt().spawn(async move {
            let queried = urls.len() + defs.len();
            let mut out = SearchOutcome {
                queried,
                failed: 0,
                results: Vec::new(),
            };
            let mut set = tokio::task::JoinSet::new();
            for (id, url) in urls {
                let cancel = cancel.clone();
                set.spawn(async move {
                    let body = Self::http_get(url, cancel).await?;
                    let xml = String::from_utf8_lossy(&body).into_owned();
                    torznab::parse_results(&xml, id).map_err(|e| e.to_string())
                });
            }
            let q = query_owned;
            let mut src_set = tokio::task::JoinSet::new();
            for d in defs {
                let (q, cancel) = (q.clone(), cancel_src.clone());
                src_set.spawn(async move { source_search(d, q, limit, cancel).await });
            }
            while let Some(r) = set.join_next().await {
                match r {
                    Ok(Ok(mut v)) => out.results.append(&mut v),
                    _ => out.failed += 1,
                }
            }
            while let Some(r) = src_set.join_next().await {
                match r {
                    Ok(Ok(mut v)) => out.results.append(&mut v),
                    _ => out.failed += 1,
                }
            }
            out.results
                .sort_by_key(|r| std::cmp::Reverse(r.seeders.unwrap_or(0)));
            let _ = tx.send(out);
        });
        rx
    }

    /// Look for a local Jackett or Prowlarr (loopback only, explicit call).
    pub fn detect_local(&self) -> Receiver<Vec<crate::detect::Detected>> {
        use crate::detect;
        let (tx, rx) = channel();
        self.rt().spawn(async move {
            let found = tokio::task::spawn_blocking(|| {
                detect::listening(detect::JACKETT_PORT, detect::PROWLARR_PORT)
            })
            .await
            .unwrap_or_default();
            let cfgdir = detect::user_config_dir();
            let mut out = Vec::new();
            for (kind, port) in found {
                let Some(dir) = cfgdir.as_deref() else {
                    continue;
                };
                match kind {
                    detect::Kind::Jackett => {
                        if let Some(k) = detect::jackett_key(dir) {
                            out.push(detect::jackett_endpoint(port, k));
                        }
                    }
                    detect::Kind::Prowlarr => {
                        let Some(key) = detect::prowlarr_key(dir) else {
                            continue;
                        };
                        let Ok(client) = Self::client() else { continue };
                        let url = format!("http://127.0.0.1:{port}/api/v1/indexer");
                        let Ok(resp) = client.get(url).header("X-Api-Key", &key).send().await
                        else {
                            continue;
                        };
                        let Ok(body) = resp.bytes().await else {
                            continue;
                        };
                        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
                            continue;
                        };
                        for ix in v.as_array().into_iter().flatten() {
                            let on = ix.get("enable").and_then(|e| e.as_bool()).unwrap_or(true);
                            if let (true, Some(id)) = (on, ix.get("id").and_then(|i| i.as_u64())) {
                                out.push(detect::prowlarr_endpoint(port, id, key.clone()));
                            }
                        }
                    }
                }
            }
            let _ = tx.send(out);
        });
        rx
    }

    pub fn play_http(&self, url: String) -> Receiver<Result<StreamTicket, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        self.rt().spawn(async move {
            let _ = tx.send(http_stream(&cfg, url).await);
        });
        rx
    }
}

/// Last request time per source id, for the per-minute rate limit.
/// Patience, rendered as a `Vec`.
static SOURCE_LAST: Mutex<Vec<(u32, std::time::Instant)>> = Mutex::new(Vec::new());

async fn source_rate_wait(id: u32, per_minute: u32, cancel: Arc<AtomicBool>) -> Result<(), String> {
    if per_minute == 0 {
        return Ok(());
    }
    let gap = Duration::from_millis(60_000 / u64::from(per_minute));
    let wait = {
        let mut g = SOURCE_LAST.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        let next = match g.iter().find(|(i, _)| *i == id) {
            Some((_, t)) => (*t + gap).max(now),
            None => now,
        };
        let wait = next.saturating_duration_since(now);
        // Too long to be worth it: fail without taking a slot.
        if wait > SEARCH_TIMEOUT {
            return Err("rate limited".into());
        }
        match g.iter_mut().find(|(i, _)| *i == id) {
            Some((_, t)) => *t = next,
            None => g.push((id, next)),
        }
        wait
    };
    if !wait.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = wait_cancel(cancel) => return Err("cancelled".into()),
        }
    }
    Ok(())
}

/// One search on a user-defined source. Errors are fixed phrases.
async fn source_search(
    d: crate::sources::SourceDef,
    query: String,
    limit: u32,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<SearchResult>, String> {
    crate::sources::validate(&d).map_err(str::to_owned)?;
    source_rate_wait(d.id, d.per_minute, cancel.clone()).await?;
    let url = crate::sources::build_url(&d, &query, 1, limit);
    let client = FetchService::builder()?
        .timeout(SEARCH_TIMEOUT)
        .redirect(same_origin_policy())
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let mut req = client.get(&url).header("Accept", "application/json");
    if let Some(ua) = &d.user_agent {
        req = req.header("User-Agent", ua);
    }
    if let Some(r) = &d.referer {
        req = req.header("Referer", r);
    }
    if let Some(h) = &d.auth_header
        && !d.secret.is_empty()
    {
        req = req.header(h, format!("{}{}", d.auth_prefix, d.secret.expose()));
    }
    let mut resp = tokio::select! {
        r = req.send() => r.map_err(|e| format!("error sending request: {}", e.without_url()))?,
        _ = wait_cancel(cancel.clone()) => return Err("cancelled".into()),
    };
    let code = resp.status().as_u16();
    if code == 401 || code == 403 {
        return Err("incorrect user credentials".into());
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {code}"));
    }
    let mut body = Vec::new();
    while let Some(c) = resp
        .chunk()
        .await
        .map_err(|e| e.without_url().to_string())?
    {
        body.extend_from_slice(&c);
        if body.len() > 4 << 20 {
            return Err("answer too large".into());
        }
    }
    let items = crate::sources::parse_items(&d, &body, limit as usize).map_err(str::to_owned)?;
    // The source's own referer and user agent follow its results to the media
    // fetch. The secret does not.
    let mut media_headers = Vec::new();
    if let Some(ua) = &d.user_agent {
        media_headers.push(("User-Agent".to_owned(), ua.clone()));
    }
    if let Some(r) = &d.referer {
        media_headers.push(("Referer".to_owned(), r.clone()));
    }
    Ok(items
        .into_iter()
        .map(|i| SearchResult {
            title: i.title,
            size: i.size,
            seeders: None,
            peers: None,
            magnet: None,
            link: i.media.or(i.page_url),
            pub_date: None,
            indexer_id: crate::sources::ID_BASE + d.id,
            media_headers: media_headers.clone(),
        })
        .collect())
}

/// First free `<stem> (n).<ext>` in `dir`; never an existing file or `.part`.
/// Every download manager eventually invents `final (2) (final).mp4`.
fn unique_name(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    for n in 0..1000 {
        let cand = if n == 0 {
            name.to_owned()
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let p = dir.join(&cand);
        if !p.exists() && !dir.join(format!("{cand}.part")).exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}.{ext}", std::process::id()))
}

pub(super) fn safe_file_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next()?;
    let clean: String = last
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_matches('.').trim().to_owned();
    (!clean.is_empty() && crate::pick::is_video(&clean)).then_some(clean)
}

/// A `.part` file being written by a task; reads never pass what is written.
struct PartReader {
    file: std::fs::File,
    pos: u64,
    len: u64,
    written: Arc<std::sync::atomic::AtomicU64>,
    failed: Arc<AtomicBool>,
    reason: Arc<Mutex<String>>,
    /// Cleared when the reader is dropped: the download stops and its
    /// `.part` goes away.
    alive: Arc<AtomicBool>,
    /// The index sits at the end of the file and there is no `Range`.
    needs_full: bool,
}

impl Drop for PartReader {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}

impl std::io::Read for PartReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Seek, SeekFrom};
        if self.pos >= self.len {
            return Ok(0);
        }
        let avail = self.written.load(Ordering::SeqCst);
        if self.pos >= avail {
            if self.failed.load(Ordering::SeqCst) {
                let r = self
                    .reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                return Err(std::io::Error::other(r));
            }
            if self.needs_full && self.pos > avail {
                // Asked for the index at the end: say so, with the progress,
                // instead of buffering silently.
                return Err(std::io::Error::other(
                    crate::fetch_error::NeedsFullDownloadProgress {
                        written: avail,
                        total: self.len,
                    },
                ));
            }
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min((avail - self.pos) as usize);
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for PartReader {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let np = match from {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::End(d) => self.len as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if np < 0 {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.pos = (np as u64).min(self.len);
        Ok(self.pos)
    }
}

pub(super) async fn http_stream(cfg: &FetchConfig, url: String) -> Result<StreamTicket, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("unsupported address".into());
    }
    let name = safe_file_name(&url).ok_or("no video file")?;
    let client = FetchService::builder()?
        .connect_timeout(crate::stream::CONNECT_TIMEOUT)
        .redirect(same_origin_policy())
        .build()
        .map_err(|e| e.without_url().to_string())?;
    http_stream_with(cfg, client, url, name).await
}

pub(super) async fn http_stream_with(
    cfg: &FetchConfig,
    client: reqwest::Client,
    url: String,
    name: String,
) -> Result<StreamTicket, String> {
    use crate::stream::{STALL_TIMEOUT, send_ttfb};
    use std::io::Write;
    use std::sync::atomic::AtomicU64;
    let mut resp = send_ttfb(client.get(&url)).await?;
    let code = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(match code {
            429 | 503 => format!("server busy (HTTP {code})"),
            300..=399 => format!("redirect refused (HTTP {code})"),
            c => format!("HTTP {c}"),
        });
    }
    let len = resp
        .content_length()
        .filter(|&l| l > 0)
        .ok_or("size unknown")?;
    let dir = cfg.effective_download_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let final_path = unique_name(&dir, &name);
    let part = PathBuf::from(format!("{}.part", final_path.display()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|e| e.to_string())?;

    // First bytes: a playlist or manifest is refused before anything is kept.
    // A body that falls silent for STALL_TIMEOUT ends the attempt; a slow one
    // that keeps moving does not.
    let mut head: Vec<u8> = Vec::new();
    let mut head_err: Option<String> = None;
    while head.len() < 4096 {
        if proxy_blocked() {
            head_err = Some("proxy required".into());
            break;
        }
        match tokio::time::timeout(STALL_TIMEOUT, resp.chunk()).await {
            Err(_) => {
                head_err = Some("server timeout".into());
                break;
            }
            Ok(Err(e)) => {
                head_err = Some(crate::stream::net_error(&e));
                break;
            }
            Ok(Ok(Some(c))) => head.extend_from_slice(&c),
            Ok(Ok(None)) => break,
        }
    }
    if let Some(e) = head_err {
        drop(file);
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    if jizzcraft_core::manifest::looks_like_manifest(&head) {
        drop(file);
        let _ = std::fs::remove_file(&part);
        return Err("refused: this refers to network media".into());
    }
    let needs_full = crate::sniff::mp4_index_at_end(&head) && len > head.len() as u64;
    if let Err(e) = file.write_all(&head) {
        drop(file);
        let _ = std::fs::remove_file(&part);
        return Err(e.to_string());
    }
    let written = Arc::new(AtomicU64::new(head.len() as u64));
    let failed = Arc::new(AtomicBool::new(false));
    let alive = Arc::new(AtomicBool::new(true));
    let reason = Arc::new(Mutex::new(String::from("unreachable")));
    let (w2, f2, a2, r2, part2, final2) = (
        Arc::clone(&written),
        Arc::clone(&failed),
        Arc::clone(&alive),
        Arc::clone(&reason),
        part.clone(),
        final_path.clone(),
    );
    // The reader is opened before the task starts so a drop is never missed.
    let reader = PartReader {
        file: std::fs::File::open(&part).map_err(|e| e.to_string())?,
        pos: 0,
        len,
        written,
        failed,
        reason,
        alive,
        needs_full,
    };
    tokio::spawn(async move {
        let set_reason = |s: &str| {
            *r2.lock().unwrap_or_else(|e| e.into_inner()) = s.to_owned();
        };
        let mut ok = true;
        let mut idle = Duration::ZERO;
        let tick = Duration::from_secs(1);
        while ok {
            if !a2.load(Ordering::SeqCst) {
                // Nobody is reading any more.
                ok = false;
                set_reason("cancelled");
                break;
            }
            if proxy_blocked() {
                ok = false;
                set_reason("proxy required");
                break;
            }
            match tokio::time::timeout(tick, resp.chunk()).await {
                Err(_) => {
                    idle += tick;
                    if idle >= STALL_TIMEOUT {
                        ok = false;
                        set_reason("server timeout");
                    }
                }
                Ok(Ok(Some(c))) => {
                    idle = Duration::ZERO;
                    if let Err(e) = file.write_all(&c) {
                        ok = false;
                        set_reason(
                            if e.kind() == std::io::ErrorKind::StorageFull
                                || e.raw_os_error() == Some(28)
                            {
                                "disk full"
                            } else {
                                "no folder: permission denied"
                            },
                        );
                    } else {
                        w2.fetch_add(c.len() as u64, Ordering::SeqCst);
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    ok = false;
                    set_reason(&crate::stream::net_error(&e));
                }
            }
        }
        let complete = ok && w2.load(Ordering::SeqCst) >= len && file.sync_all().is_ok();
        drop(file);
        if complete {
            // Never replace a file that appeared meanwhile, even at the very
            // last moment: the rename itself refuses.
            if crate::savename::rename_noclobber(&part2, &final2).is_err() {
                f2.store(true, Ordering::SeqCst);
                let _ = std::fs::remove_file(&part2);
            }
        } else {
            // A server without Range cannot resume: a partial file is junk.
            f2.store(true, Ordering::SeqCst);
            let _ = std::fs::remove_file(&part2);
        }
    });
    Ok(StreamTicket::Live {
        reader: StreamReader(Box::new(reader)),
        len,
        name,
        status: None,
    })
}
