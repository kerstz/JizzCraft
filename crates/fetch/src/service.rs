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
    !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

pub struct FetchService {
    rt: Option<Runtime>,
    cfg: Arc<FetchConfig>,
    state_dir: PathBuf,
    session: Arc<OnceCell<Arc<Session>>>,
    cache: Arc<Mutex<Vec<TorrentStatus>>>,
}

impl FetchService {
    pub fn new(cfg: FetchConfig, state_dir: PathBuf) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("jizz-fetch")
            .enable_all()
            .build()?;
        Ok(Self {
            rt: Some(rt),
            cfg: Arc::new(cfg),
            state_dir,
            session: Arc::new(OnceCell::new()),
            cache: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn rt(&self) -> &Runtime {
        self.rt.as_ref().expect("runtime alive until drop")
    }

    fn client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
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
        let urls: Vec<(u32, String)> = targets
            .iter()
            .map(|i| (i.id, torznab::build_search_url(i, query, cats, limit)))
            .collect();
        self.rt().spawn(async move {
            let queried = urls.len();
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
            while let Some(r) = set.join_next().await {
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
        let dir = cfg
            .download_dir
            .clone()
            .ok_or_else(|| "no download folder set".to_string())?;
        cell.get_or_try_init(|| async {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let opts = SessionOptions {
                dht: cfg.dht.then(|| DhtSessionConfig {
                    persistence: None,
                    ..Default::default()
                }),
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(state_dir.to_path_buf()),
                }),
                fastresume: true,
                listen: cfg.listen_incoming.then(ListenerOptions::default),
                connect: Some(ConnectionOptions {
                    proxy_url: cfg.proxy.as_ref().map(|p| p.expose().to_owned()),
                    ..Default::default()
                }),
                disable_local_service_discovery: true,
                ..Default::default()
            };
            Session::new_with_opts(dir, opts)
                .await
                .map_err(|e| e.to_string())
        })
        .await
        .cloned()
    }

    /// Add a torrent. Returns its id.
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
            let r = Self::add_inner(&cfg, &state, &cell, source, peers).await;
            let _ = tx.send(r);
        });
        rx
    }

    async fn add_inner(
        cfg: &FetchConfig,
        state: &Path,
        cell: &OnceCell<Arc<Session>>,
        source: Source,
        peers: Vec<std::net::SocketAddr>,
    ) -> Result<usize, String> {
        let session = Self::session(cfg, state, cell).await?;
        let dir = cfg.download_dir.clone().ok_or("no download folder set")?;
        let bytes_or_url = match source {
            Source::Magnet(m) => AddTorrent::from_url(m),
            Source::TorrentUrl(u) => {
                if u.starts_with("magnet:") {
                    AddTorrent::from_url(u)
                } else {
                    AddTorrent::from_bytes(Self::http_get(u, Arc::default()).await?)
                }
            }
            Source::TorrentFile(p) => {
                AddTorrent::from_bytes(std::fs::read(&p).map_err(|e| e.kind().to_string())?)
            }
        };
        let clone = |a: &AddTorrent<'_>| match a {
            AddTorrent::Url(u) => AddTorrent::from_url(u.to_string()),
            AddTorrent::TorrentFileBytes(b) => AddTorrent::from_bytes(b.clone()),
        };
        // First pass: metadata only, validate every path.
        let listed = session
            .add_torrent(
                clone(&bytes_or_url),
                Some(AddTorrentOptions {
                    list_only: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        if let AddTorrentResponse::ListOnly(l) = listed {
            for f in l.info.iter_file_details() {
                if !path_is_safe(&f.filename.to_pathbuf()) {
                    return Err("torrent contains an unsafe file path; refused".into());
                }
            }
        }
        let added = session
            .add_torrent(
                bytes_or_url,
                Some(AddTorrentOptions {
                    overwrite: false,
                    initial_peers: (!peers.is_empty()).then_some(peers),
                    output_folder: Some(dir.to_string_lossy().into_owned()),
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

async fn wait_cancel(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
