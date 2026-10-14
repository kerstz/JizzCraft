//! BitTorrent engine and HTTP client (feature `network`).
//!
//! Rules enforced here:
//! - the service owns a private tokio runtime; callers never see async;
//! - no socket is opened until the first search or add;
//! - torrent file paths are validated with a `list_only` pass before anything
//!   is written, and torrents never overwrite an existing file;
//! - `forget` never deletes downloaded files;
//! - no `tracing` subscriber is ever installed (librqbit logs torrent names).

use crate::config::{FetchConfig, Indexer};
use crate::redact::redact_url;
use crate::torznab::{self, SearchResult};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, DhtSessionConfig,
    ListenerOptions, Session, SessionOptions, SessionPersistenceConfig, api::TorrentIdOrHash,
};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::OnceCell;

const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TorrentState {
    Initializing,
    Downloading,
    Seeding,
    Paused,
    Error,
}

#[derive(Clone, Debug)]
pub struct TorrentStatus {
    pub id: usize,
    pub name: String,
    pub state: TorrentState,
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub eta_secs: Option<u64>,
}

impl TorrentStatus {
    pub fn progress(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.progress_bytes as f32 / self.total_bytes as f32
        }
    }
}

fn mibps(v: Option<u32>) -> Option<std::num::NonZeroU32> {
    v.and_then(|m| std::num::NonZeroU32::new(m.saturating_mul(1024 * 1024)))
}

/// A resolved torrent source: a magnet or the .torrent bytes.
#[derive(Clone)]
pub enum Prepared {
    Magnet(String),
    Bytes(Vec<u8>),
}

/// A torrent whose file list is known but which has not been added yet.
pub struct Listed {
    /// Relative file names and lengths, in torrent order. Never log these.
    pub files: Vec<(String, u64)>,
    prepared: Prepared,
    peers: Vec<std::net::SocketAddr>,
}

/// What `open_reader` hands back.
pub enum StreamTicket {
    /// The file is complete on disk: play it with the normal file player.
    Done(PathBuf),
    Live {
        reader: StreamReader,
        len: u64,
        /// File name only, for a window title (hide it when discreet).
        name: String,
    },
    /// An adaptive stream (HLS/DASH) delivered as timed chunks.
    Timed {
        source: Box<dyn jizzcraft_core::timed::TimedSource>,
        name: String,
    },
}

/// Options for [`FetchService::open_any`].
#[derive(Debug, Clone, Default)]
pub struct StreamOpts {
    pub quality: crate::pick::Quality,
    /// Extra request headers. Only `User-Agent`, `Referer` and `Origin` are
    /// ever sent; anything else is dropped.
    pub headers: Vec<(String, String)>,
}

impl StreamOpts {
    /// Options for the media of a search result: the headers its source asked
    /// for (never the secret).
    pub fn for_result(r: &SearchResult, quality: crate::pick::Quality) -> Self {
        Self {
            quality,
            headers: r.media_headers.clone(),
        }
    }
}

pub trait ReadSeekSend: std::io::Read + std::io::Seek + Send {}
impl<T: std::io::Read + std::io::Seek + Send> ReadSeekSend for T {}

/// A readable, seekable view of a file that is still arriving, whatever the
/// source (torrent or plain HTTP). `WouldBlock` = not here yet.
pub struct StreamReader(Box<dyn ReadSeekSend>);

impl std::io::Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::Seek for StreamReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

/// `Read + Seek` over a file that is still downloading. `WouldBlock` means
/// the pieces under the read position have not arrived yet.
pub struct TorrentReader {
    handle: tokio::runtime::Handle,
    // librqbit does not export `FileStream`, so it is held as a trait object.
    stream: Box<dyn AsyncFile>,
}

trait AsyncFile: tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin> AsyncFile for T {}

impl std::io::Read for TorrentReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        let fut = self.stream.as_mut().read(buf);
        match self
            .handle
            .block_on(async { tokio::time::timeout(Duration::from_millis(100), fut).await })
        {
            Ok(r) => r,
            Err(_) => Err(std::io::ErrorKind::WouldBlock.into()),
        }
    }
}

impl std::io::Seek for TorrentReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        use tokio::io::AsyncSeekExt;
        let fut = self.stream.as_mut().seek(pos);
        match self
            .handle
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), fut).await })
        {
            Ok(r) => r,
            Err(_) => Err(std::io::ErrorKind::TimedOut.into()),
        }
    }
}

/// What to add. A `TorrentUrl` is fetched over HTTP first (indexer download
/// link); a `Magnet` goes straight to the session.
#[derive(Clone)]
pub enum Source {
    Magnet(String),
    TorrentUrl(String),
    TorrentFile(PathBuf),
}

/// Reject any path that could escape the download folder.
pub fn path_is_safe(p: &Path) -> bool {
    !p.as_os_str().is_empty()
        && p.components().all(|c| match c {
            Component::Normal(n) => name_is_portable(&n.to_string_lossy()),
            _ => false,
        })
}

/// A file name that is harmless on every platform: no alternate data stream
/// (`a:b`), no characters Windows forbids, no control characters, no trailing
/// dot or space, no reserved device name. Checked on all platforms so a
/// torrent behaves the same everywhere.
fn name_is_portable(n: &str) -> bool {
    if n.is_empty()
        || n.chars()
            .any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*' | '\\'))
        || n.ends_with('.')
        || n.ends_with(' ')
    {
        return false;
    }
    // Windows trims trailing spaces of the stem before comparing. It also
    // reserves `COM¹`, because superscripts were apparently a threat vector.
    let stem = n
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p).is_some_and(|d| {
            let mut c = d.chars();
            matches!(c.next(), Some('0'..='9' | '¹' | '²' | '³')) && c.next().is_none()
        })
    };
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT");
    !reserved
}

/// The SOCKS5 proxy for HTTP requests (indexers, direct downloads), shared by
/// the static request helpers. Set from the configuration.
static HTTP_PROXY: Mutex<Option<String>> = Mutex::new(None);

/// Mirrors `FetchConfig::require_proxy` for the static request helpers.
static REQUIRE_PROXY: AtomicBool = AtomicBool::new(false);

/// True when the kill switch is on and no proxy is set.
pub(crate) fn proxy_blocked() -> bool {
    REQUIRE_PROXY.load(Ordering::SeqCst) && current_proxy().is_none()
}

/// Redirects stay on the same host and scheme. A key sent to a source must
/// never follow a redirect to another site.
pub(crate) fn same_origin_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let ok = attempt.previous().len() < 3
            && attempt.previous().last().is_some_and(|p| {
                p.host_str() == attempt.url().host_str() && p.scheme() == attempt.url().scheme()
            });
        if ok { attempt.follow() } else { attempt.stop() }
    })
}

/// The SOCKS5 proxy currently in force, for helpers started by the user.
pub(crate) fn current_proxy() -> Option<String> {
    HTTP_PROXY.lock().ok().and_then(|g| g.clone())
}

fn publish_proxy(cfg: &FetchConfig) {
    REQUIRE_PROXY.store(cfg.require_proxy, Ordering::SeqCst);
    if let Ok(mut g) = HTTP_PROXY.lock() {
        *g = cfg
            .proxy
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| p.expose().to_owned());
    }
}

pub struct FetchService {
    rt: Option<Runtime>,
    cfg: Arc<FetchConfig>,
    state_dir: PathBuf,
    session: Arc<OnceCell<Arc<Session>>>,
    cache: Arc<Mutex<Vec<TorrentStatus>>>,
    /// Raised by `cancel_streams`, then swapped for a fresh flag.
    stream_cancel: Mutex<Arc<AtomicBool>>,
}

impl FetchService {
    pub fn new(cfg: FetchConfig, state_dir: PathBuf) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("jizz-fetch")
            .enable_all()
            .build()?;
        publish_proxy(&cfg);
        Ok(Self {
            rt: Some(rt),
            cfg: Arc::new(cfg),
            state_dir,
            session: Arc::new(OnceCell::new()),
            cache: Arc::new(Mutex::new(Vec::new())),
            stream_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
        })
    }

    /// Replace the configuration (indexers, download folder). A running
    /// session keeps its network options until the next start.
    pub fn set_config(&mut self, cfg: FetchConfig) {
        publish_proxy(&cfg);
        self.cfg = Arc::new(cfg);
    }

    fn rt(&self) -> &Runtime {
        self.rt.as_ref().expect("runtime alive until drop")
    }

    pub(crate) fn builder() -> Result<reqwest::ClientBuilder, String> {
        let mut b = reqwest::Client::builder();
        let proxy = HTTP_PROXY.lock().ok().and_then(|g| g.clone());
        if proxy.is_none() && REQUIRE_PROXY.load(Ordering::SeqCst) {
            return Err("proxy required".into());
        }
        if let Some(p) = proxy {
            // socks5h: the proxy resolves names too.
            let url = p.replacen("socks5://", "socks5h://", 1);
            // Loopback (a local Jackett, say) is not a leak and the proxy
            // machine's own loopback is not what the user means.
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| e.without_url().to_string())?
                .no_proxy(reqwest::NoProxy::from_string("127.0.0.1,::1,localhost"));
            b = b.proxy(proxy);
        }
        Ok(b)
    }

    fn client() -> Result<reqwest::Client, String> {
        Self::builder()?
            .timeout(SEARCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|e| e.without_url().to_string())
    }

    async fn http_get(url: String, cancel: Arc<AtomicBool>) -> Result<Vec<u8>, String> {
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

    async fn session(
        cfg: &FetchConfig,
        state_dir: &Path,
        cell: &OnceCell<Arc<Session>>,
    ) -> Result<Arc<Session>, String> {
        let dir = cfg.effective_download_dir();
        cell.get_or_try_init(|| async {
            if cfg.require_proxy && cfg.proxy.as_ref().is_none_or(|p| p.is_empty()) {
                return Err("proxy required".to_string());
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let policy = cfg.net_policy();
            let opts = SessionOptions {
                disable_trackers: !policy.trackers,
                dht: policy.dht.then(|| DhtSessionConfig {
                    persistence: None,
                    ..Default::default()
                }),
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(state_dir.to_path_buf()),
                }),
                fastresume: true,
                listen: policy.listen.then(ListenerOptions::default),
                connect: Some(ConnectionOptions {
                    proxy_url: cfg.proxy.as_ref().map(|p| p.expose().to_owned()),
                    ..Default::default()
                }),
                disable_local_service_discovery: true,
                ratelimits: librqbit::limits::LimitsConfig {
                    download_bps: mibps(cfg.down_limit_mibps),
                    upload_bps: mibps(cfg.up_limit_mibps),
                },
                ..Default::default()
            };
            Session::new_with_opts(dir, opts)
                .await
                .map_err(|e| e.to_string())
        })
        .await
        .cloned()
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

    pub fn play_http(&self, url: String) -> Receiver<Result<StreamTicket, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        self.rt().spawn(async move {
            let _ = tx.send(http_stream(&cfg, url).await);
        });
        rx
    }

    /// File names and lengths of an already added torrent.
    pub fn torrent_files(&self, id: usize) -> Receiver<Result<Vec<(String, u64)>, String>> {
        let (tx, rx) = channel();
        let session = self.session.clone();
        self.rt().spawn(async move {
            let r = async {
                let s = session.get().cloned().ok_or("no session")?;
                let h = s.get(TorrentIdOrHash::Id(id)).ok_or("torrent not found")?;
                h.with_metadata(|m| {
                    m.file_infos
                        .iter()
                        .map(|f| (f.relative_filename.to_string_lossy().into_owned(), f.len))
                        .collect::<Vec<_>>()
                })
                .map_err(|e| e.to_string())
            }
            .await;
            let _ = tx.send(r);
        });
        rx
    }

    /// Open the session (restores persisted torrents). Idempotent.
    pub fn start(&self) -> Receiver<Result<(), String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        self.rt().spawn(async move {
            let _ = tx.send(Self::session(&cfg, &state, &cell).await.map(|_| ()));
        });
        rx
    }

    /// Add a whole torrent. Returns its id.
    pub fn add(&self, source: Source) -> Receiver<Result<usize, String>> {
        self.add_with_peers(source, Vec::new())
    }

    /// Like [`add`](Self::add), with known peer addresses to try first.
    pub fn add_with_peers(
        &self,
        source: Source,
        peers: Vec<std::net::SocketAddr>,
    ) -> Receiver<Result<usize, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        self.rt().spawn(async move {
            let r = async {
                let prepared = Self::prepare(source).await?;
                Self::add_prepared(&cfg, &state, &cell, prepared, None, peers).await
            }
            .await;
            let _ = tx.send(r);
        });
        rx
    }

    /// Resolve a source to bytes or a magnet, then read the file list
    /// without writing anything. Unsafe paths are refused here.
    pub fn list_files(&self, source: Source) -> Receiver<Result<Listed, String>> {
        self.list_files_with_peers(source, Vec::new())
    }

    pub fn list_files_with_peers(
        &self,
        source: Source,
        peers: Vec<std::net::SocketAddr>,
    ) -> Receiver<Result<Listed, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        self.rt().spawn(async move {
            let r = async {
                let prepared = Self::prepare(source).await?;
                let session = Self::session(&cfg, &state, &cell).await?;
                let files = Self::list_prepared(&session, &prepared, &peers).await?;
                Ok(Listed {
                    files,
                    prepared,
                    peers,
                })
            }
            .await;
            let _ = tx.send(r);
        });
        rx
    }

    /// Add a listed torrent; `only` limits the download to those file
    /// indexes (None = everything). Returns its id.
    pub fn add_listed(
        &self,
        listed: Listed,
        only: Option<Vec<usize>>,
    ) -> Receiver<Result<usize, String>> {
        let (tx, rx) = channel();
        let cfg = self.cfg.clone();
        let state = self.state_dir.clone();
        let cell = self.session.clone();
        self.rt().spawn(async move {
            let r =
                Self::add_prepared(&cfg, &state, &cell, listed.prepared, only, listed.peers).await;
            let _ = tx.send(r);
        });
        rx
    }

    async fn prepare(source: Source) -> Result<Prepared, String> {
        Ok(match source {
            Source::Magnet(m) => Prepared::Magnet(m),
            Source::TorrentUrl(u) if u.starts_with("magnet:") => Prepared::Magnet(u),
            Source::TorrentUrl(u) => Prepared::Bytes(Self::http_get(u, Arc::default()).await?),
            Source::TorrentFile(p) => {
                Prepared::Bytes(std::fs::read(&p).map_err(|e| e.kind().to_string())?)
            }
        })
    }

    fn to_add(p: &Prepared) -> AddTorrent<'static> {
        match p {
            Prepared::Magnet(m) => AddTorrent::from_url(m.clone()),
            Prepared::Bytes(b) => AddTorrent::from_bytes(b.clone()),
        }
    }

    async fn list_prepared(
        session: &Arc<Session>,
        p: &Prepared,
        peers: &[std::net::SocketAddr],
    ) -> Result<Vec<(String, u64)>, String> {
        let listed = session
            .add_torrent(
                Self::to_add(p),
                Some(AddTorrentOptions {
                    list_only: true,
                    initial_peers: (!peers.is_empty()).then(|| peers.to_vec()),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        let AddTorrentResponse::ListOnly(l) = listed else {
            return Err("torrent is already being downloaded".into());
        };
        let mut out = Vec::new();
        for f in l.info.iter_file_details() {
            let path = f.filename.to_pathbuf();
            if !path_is_safe(&path) {
                return Err("torrent contains an unsafe file path; refused".into());
            }
            out.push((path.to_string_lossy().into_owned(), f.len));
        }
        Ok(out)
    }

    async fn add_prepared(
        cfg: &FetchConfig,
        state: &Path,
        cell: &OnceCell<Arc<Session>>,
        prepared: Prepared,
        only: Option<Vec<usize>>,
        peers: Vec<std::net::SocketAddr>,
    ) -> Result<usize, String> {
        let session = Self::session(cfg, state, cell).await?;
        // The metadata pass also validates every path before anything is written.
        Self::list_prepared(&session, &prepared, &peers).await?;
        let dir = cfg.effective_download_dir();
        let added = session
            .add_torrent(
                Self::to_add(&prepared),
                Some(AddTorrentOptions {
                    overwrite: false,
                    output_folder: Some(dir.to_string_lossy().into_owned()),
                    only_files: only,
                    initial_peers: (!peers.is_empty()).then_some(peers),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        added
            .into_handle()
            .map(|h| h.id())
            .ok_or_else(|| "torrent was not added".into())
    }

    /// Something to play for file `file` of torrent `id`: the finished file
    /// on disk, or a reader over the part that is still downloading.
    pub fn open_reader(&self, id: usize, file: usize) -> Receiver<Result<StreamTicket, String>> {
        let (tx, rx) = channel();
        let session = self.session.clone();
        let handle = self.rt().handle().clone();
        self.rt().spawn(async move {
            let r = async {
                let session = session.get().cloned().ok_or("no session")?;
                let h = session
                    .get(TorrentIdOrHash::Id(id))
                    .ok_or("torrent not found")?;
                // Metadata and state may take a moment after an add.
                let t0 = std::time::Instant::now();
                let (len, rel) = loop {
                    if let Ok(v) = h.with_metadata(|m| {
                        m.file_infos
                            .get(file)
                            .map(|f| (f.len, f.relative_filename.clone()))
                    }) && let Some(v) = v
                    {
                        break v;
                    }
                    if t0.elapsed() > Duration::from_secs(60) {
                        return Err("could not read the torrent metadata".to_string());
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                };
                let path = h.output_folder().join(&rel);
                if !path_is_safe(&rel) {
                    return Err("unsafe file path".to_string());
                }
                let done = h.stats().file_progress.get(file).is_some_and(|&p| p >= len);
                if done && path.is_file() {
                    return Ok(StreamTicket::Done(path));
                }
                // A paused torrent never receives pieces: the user asked to play.
                // "Play" is not a suggestion.
                if h.is_paused() {
                    let _ = session.unpause(&h).await;
                }
                let stream = loop {
                    match h.clone().stream(file).await {
                        Ok(s) => break s,
                        Err(_) if t0.elapsed() < Duration::from_secs(60) => {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                        Err(_) => return Err("could not start streaming".to_string()),
                    }
                };
                Ok(StreamTicket::Live {
                    reader: StreamReader(Box::new(TorrentReader {
                        handle,
                        stream: Box::new(stream),
                    })),
                    len,
                    name: rel
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                })
            }
            .await;
            let _ = tx.send(r);
        });
        rx
    }

    /// Snapshot of every torrent. Reading is cheap; the cache is refreshed
    /// on the runtime so the UI thread never waits on the session.
    pub fn list(&self) -> Vec<TorrentStatus> {
        if let Some(session) = self.session.get().cloned() {
            let cache = self.cache.clone();
            let seed = self.cfg.seed_after_complete;
            self.rt().spawn(async move {
                let mut done = Vec::new();
                let items: Vec<_> =
                    session.with_torrents(|it| it.map(|(id, h)| (id, h.clone())).collect());
                let mut v = Vec::with_capacity(items.len());
                for (id, h) in items {
                    let s = h.stats();
                    let state = match (&s.state, s.error.is_some(), s.finished) {
                        (_, true, _) => TorrentState::Error,
                        (librqbit::TorrentStatsState::Paused, _, _) => TorrentState::Paused,
                        (librqbit::TorrentStatsState::Initializing { .. }, _, _) => {
                            TorrentState::Initializing
                        }
                        (_, _, true) => TorrentState::Seeding,
                        _ => TorrentState::Downloading,
                    };
                    if s.finished && !seed && state != TorrentState::Paused {
                        done.push(h.clone());
                    }
                    let (down, up) = s.live.as_ref().map_or((0, 0), |l| {
                        (
                            (l.download_speed.mbps * 1_048_576.0) as u64,
                            (l.upload_speed.mbps * 1_048_576.0) as u64,
                        )
                    });
                    let eta = (down > 0 && !s.finished)
                        .then(|| s.total_bytes.saturating_sub(s.progress_bytes) / down);
                    v.push(TorrentStatus {
                        id,
                        name: h.name().unwrap_or_default(),
                        state,
                        progress_bytes: s.progress_bytes,
                        total_bytes: s.total_bytes,
                        down_bps: down,
                        up_bps: up,
                        eta_secs: eta,
                    });
                }
                // Finished and not seeding: pause so we stop uploading.
                for h in done {
                    let _ = session.pause(&h).await;
                }
                if let Ok(mut g) = cache.lock() {
                    *g = v;
                }
            });
        }
        self.cache.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn pause(&self, id: usize) {
        if let Some(s) = self.session.get().cloned() {
            self.rt().spawn(async move {
                if let Some(h) = s.get(TorrentIdOrHash::Id(id)) {
                    let _ = s.pause(&h).await;
                }
            });
        }
    }

    pub fn resume(&self, id: usize) {
        if let Some(s) = self.session.get().cloned() {
            self.rt().spawn(async move {
                if let Some(h) = s.get(TorrentIdOrHash::Id(id)) {
                    let _ = s.unpause(&h).await;
                }
            });
        }
    }

    /// Stop tracking a torrent. Downloaded files are left on disk, always.
    /// We forget; we do not shred.
    pub fn forget(&self, id: usize) {
        if let Some(s) = self.session.get().cloned() {
            self.rt().spawn(async move {
                let _ = s.delete(TorrentIdOrHash::Id(id), false).await;
            });
        }
    }
}

impl Drop for FetchService {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

#[derive(Debug, Default)]
pub struct SearchOutcome {
    pub queried: usize,
    pub failed: usize,
    pub results: Vec<SearchResult>,
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

async fn wait_cancel(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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

fn safe_file_name(url: &str) -> Option<String> {
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
}

impl std::io::Read for PartReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Seek, SeekFrom};
        if self.pos >= self.len {
            return Ok(0);
        }
        let avail = self.written.load(Ordering::SeqCst);
        if self.pos >= avail {
            return Err(if self.failed.load(Ordering::SeqCst) {
                std::io::Error::other("download failed")
            } else {
                std::io::ErrorKind::WouldBlock.into()
            });
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

async fn http_stream(cfg: &FetchConfig, url: String) -> Result<StreamTicket, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("unsupported address".into());
    }
    let name = safe_file_name(&url).ok_or("no video file")?;
    let client = FetchService::builder()?
        .connect_timeout(SEARCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    http_stream_with(cfg, client, url, name).await
}

async fn http_stream_with(
    cfg: &FetchConfig,
    client: reqwest::Client,
    url: String,
    name: String,
) -> Result<StreamTicket, String> {
    use std::io::Write;
    use std::sync::atomic::AtomicU64;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("error sending request: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
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
    if jizzcraft_core::manifest::looks_like_manifest(&head) {
        drop(file);
        let _ = std::fs::remove_file(&part);
        return Err("refused: this refers to network media".into());
    }
    file.write_all(&head).map_err(|e| e.to_string())?;
    let written = Arc::new(AtomicU64::new(head.len() as u64));
    let failed = Arc::new(AtomicBool::new(false));
    let (w2, f2, part2, final2) = (
        Arc::clone(&written),
        Arc::clone(&failed),
        part.clone(),
        final_path.clone(),
    );
    tokio::spawn(async move {
        let mut ok = true;
        while ok {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    if file.write_all(&c).is_err() {
                        ok = false;
                    } else {
                        w2.fetch_add(c.len() as u64, Ordering::SeqCst);
                    }
                }
                Ok(None) => break,
                Err(_) => ok = false,
            }
        }
        let complete = ok && w2.load(Ordering::SeqCst) >= len && file.sync_all().is_ok();
        drop(file);
        if complete {
            // Never replace a file that appeared meanwhile.
            if final2.exists() {
                f2.store(true, Ordering::SeqCst);
            } else {
                let _ = jizzcraft_core::fsutil::replace(&part2, &final2);
            }
        } else {
            f2.store(true, Ordering::SeqCst);
        }
    });
    let reader = PartReader {
        file: std::fs::File::open(&part).map_err(|e| e.to_string())?,
        pos: 0,
        len,
        written,
        failed,
    };
    Ok(StreamTicket::Live {
        reader: StreamReader(Box::new(reader)),
        len,
        name,
    })
}
