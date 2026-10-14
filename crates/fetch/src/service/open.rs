//! Opening a stream from an address: cancel tokens, resolvers, HLS/DASH.

use super::http::{http_stream, http_stream_with, safe_file_name};
use super::{
    FetchService, SEARCH_TIMEOUT, StreamOpts, StreamReader, StreamTicket, current_proxy,
    proxy_blocked, same_origin_policy,
};
use crate::config::FetchConfig;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

impl FetchService {
    /// Stream a plain `http(s)` video file while it is saved next to the
    /// torrent downloads (`<name>.part`, renamed when complete, never
    /// overwriting). Needs a `Content-Length`; refuses playlists.
    /// Play a plain `http(s)` video without writing it to disk: a seekable
    /// reader over `Range` requests. A server that does not honour ranges
    /// falls back to [`Self::play_http`] (saved next to the downloads).
    pub fn stream_url(&self, url: String) -> Receiver<Result<StreamTicket, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let handle = self.rt().handle().clone();
        self.rt().spawn(async move {
            let _ = tx.send(open_stream_url(&cfg, handle, url).await);
        });
        rx
    }

    /// End every `open_any` / `stream_via_resolver` that is still opening
    /// (boss key, panic switch). Streams already handed out are the caller's.
    pub fn cancel_streams(&self) {
        let mut g = self.stream_cancel.lock().unwrap_or_else(|e| e.into_inner());
        g.store(true, Ordering::SeqCst);
        *g = Arc::new(AtomicBool::new(false));
    }

    fn cancel_token(&self) -> Arc<AtomicBool> {
        self.stream_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn spawn_open<F>(&self, fut: F) -> Receiver<Result<StreamTicket, String>>
    where
        F: std::future::Future<Output = Result<StreamTicket, String>> + Send + 'static,
    {
        let (tx, rx) = channel();
        let tok = self.cancel_token();
        self.rt().spawn(async move {
            let r = if tok.load(Ordering::SeqCst) {
                Err("cancelled".to_string())
            } else {
                tokio::select! {
                    r = fut => r,
                    _ = wait_cancel(tok.clone()) => Err("cancelled".to_string()),
                }
            };
            let _ = tx.send(r);
        });
        rx
    }

    /// Open whatever the address is: an HLS or DASH manifest (timed chunks),
    /// a media file (seekable reader), or a page that one of the enabled
    /// resolvers can turn into one. One ranged GET decides.
    pub fn open_any(
        &self,
        url: String,
        opts: StreamOpts,
    ) -> Receiver<Result<StreamTicket, String>> {
        let cfg = self.cfg.clone();
        let handle = self.rt().handle().clone();
        self.spawn_open(open_any_task(cfg, handle, url, opts))
    }

    /// Resolve a page address with the user's resolver `id`, then stream the
    /// best muxed address it returns (file, HLS or DASH).
    pub fn stream_via_resolver(
        &self,
        id: u32,
        page_url: String,
    ) -> Receiver<Result<StreamTicket, String>> {
        let cfg = self.cfg.clone();
        let handle = self.rt().handle().clone();
        self.spawn_open(async move {
            let def = cfg
                .resolvers
                .iter()
                .find(|r| r.id == id && r.enabled)
                .cloned()
                .ok_or("no such resolver")?;
            resolve_and_open(&cfg, handle, def, page_url, &StreamOpts::default()).await
        })
    }
}

/// `Ok(None)`: the address is not media or a manifest (a page).
async fn open_direct(
    cfg: &FetchConfig,
    handle: tokio::runtime::Handle,
    url: String,
    opts: &StreamOpts,
) -> Result<Option<StreamTicket>, String> {
    use crate::sniff::{Sniff, sniff};
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("unsupported address".to_string());
    }
    if proxy_blocked() {
        return Err("proxy required".into());
    }
    let headers = crate::segmented::allowed_headers(opts.headers.clone());
    let mut map = reqwest::header::HeaderMap::new();
    for (k, v) in &headers {
        if let (Ok(k), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            map.insert(k, v);
        }
    }
    let client = FetchService::builder()?
        .connect_timeout(SEARCH_TIMEOUT)
        .redirect(same_origin_policy())
        .default_headers(map)
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=0-4095")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("error sending request: {}", e.without_url()))?;
    let code = resp.status().as_u16();
    if code != 200 && code != 206 {
        return Err(format!("HTTP {code}"));
    }
    let mut head: Vec<u8> = Vec::new();
    while head.len() < 4096 {
        match resp
            .chunk()
            .await
            .map_err(|e| e.without_url().to_string())?
        {
            Some(c) => head.extend_from_slice(&c),
            None => break,
        }
    }
    drop(resp);
    let name = safe_file_name(&url).unwrap_or_else(|| "stream".to_owned());
    match sniff(&head) {
        Sniff::Hls => {
            let r = crate::segmented::open_hls(handle, client, url, opts.quality, headers).await?;
            Ok(Some(StreamTicket::Timed {
                source: Box::new(r),
                name,
            }))
        }
        Sniff::Dash => {
            let r = crate::segmented::open_dash(handle, client, url, opts.quality, headers).await?;
            Ok(Some(StreamTicket::Timed {
                source: Box::new(r),
                name,
            }))
        }
        Sniff::Media => {
            if proxy_blocked() {
                return Err("proxy required".into());
            }
            match crate::stream::open(handle, client.clone(), url.clone()).await {
                Ok(o) => Ok(Some(StreamTicket::Live {
                    reader: StreamReader(Box::new(o.reader)),
                    len: o.len,
                    name,
                })),
                Err(e) if e == crate::stream::NOT_SEEKABLE => {
                    http_stream_with(cfg, client, url, name).await.map(Some)
                }
                Err(e) => Err(e),
            }
        }
        Sniff::Other => Ok(None),
    }
}

async fn resolve_and_open(
    cfg: &FetchConfig,
    handle: tokio::runtime::Handle,
    def: crate::resolver::ResolverDef,
    page_url: String,
    opts: &StreamOpts,
) -> Result<StreamTicket, String> {
    if cfg.require_proxy && (current_proxy().is_none() || !def.honours_proxy) {
        return Err("proxy required".to_string());
    }
    let formats = tokio::task::spawn_blocking(move || crate::resolver::run(&def, &page_url))
        .await
        .map_err(|_| "resolver failed".to_string())??;
    let mut last = "no media".to_string();
    for f in crate::resolver::muxed(formats)?.into_iter().take(4) {
        let mut o = opts.clone();
        o.headers.extend(f.headers);
        match open_direct(cfg, handle.clone(), f.url, &o).await {
            Ok(Some(t)) => return Ok(t),
            Ok(None) => {}
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn open_any_task(
    cfg: Arc<FetchConfig>,
    handle: tokio::runtime::Handle,
    url: String,
    opts: StreamOpts,
) -> Result<StreamTicket, String> {
    if let Some(t) = open_direct(&cfg, handle.clone(), url.clone(), &opts).await? {
        return Ok(t);
    }
    // Not media: a page. Each enabled resolver in order, first usable wins.
    let mut blocked = false;
    for def in cfg.resolvers.iter().filter(|r| r.enabled) {
        if cfg.require_proxy && (current_proxy().is_none() || !def.honours_proxy) {
            blocked = true;
            continue;
        }
        if let Ok(t) = resolve_and_open(&cfg, handle.clone(), def.clone(), url.clone(), &opts).await
        {
            return Ok(t);
        }
    }
    Err(if blocked {
        "proxy required".into()
    } else {
        "no media".into()
    })
}

async fn open_stream_url(
    cfg: &FetchConfig,
    handle: tokio::runtime::Handle,
    url: String,
) -> Result<StreamTicket, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("unsupported address".to_string());
    }
    let name = safe_file_name(&url).ok_or("no video file")?;
    let client = FetchService::builder()?
        .connect_timeout(SEARCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    match crate::stream::open(handle, client, url.clone()).await {
        Ok(o) => Ok(StreamTicket::Live {
            reader: StreamReader(Box::new(o.reader)),
            len: o.len,
            name,
        }),
        Err(e) if e == crate::stream::NOT_SEEKABLE => http_stream(cfg, url).await,
        Err(e) => Err(e),
    }
}

pub(super) async fn wait_cancel(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
