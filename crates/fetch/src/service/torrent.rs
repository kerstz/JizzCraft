//! Torrent session, add/list/pause/resume/forget and file readers.

use super::{FetchService, StreamReader, StreamTicket};
use crate::config::FetchConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, DhtSessionConfig,
    ListenerOptions, Session, SessionOptions, SessionPersistenceConfig, api::TorrentIdOrHash,
};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use tokio::sync::OnceCell;

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

impl FetchService {
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
