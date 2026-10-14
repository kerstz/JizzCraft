//! BitTorrent engine and HTTP client (feature `network`).
//!
//! Rules enforced here:
//! - the service owns a private tokio runtime; callers never see async;
//! - no socket is opened until the first search or add;
//! - torrent file paths are validated with a `list_only` pass before anything
//!   is written, and torrents never overwrite an existing file;
//! - `forget` never deletes downloaded files;
//! - no `tracing` subscriber is ever installed (librqbit logs torrent names).

mod http;
mod open;
mod proxy;
mod torrent;

use crate::config::FetchConfig;
use crate::torznab::SearchResult;
use librqbit::Session;
use proxy::publish_proxy;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::OnceCell;

pub use torrent::{
    Listed, Prepared, ProbeState, Source, StreamStatus, TorrentProbe, TorrentReader, TorrentState,
    TorrentStatus, path_is_safe,
};

pub(crate) use proxy::{current_proxy, proxy_blocked, same_origin_policy};

const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

/// What `open_reader` hands back.
pub enum StreamTicket {
    /// The file is complete on disk: play it with the normal file player.
    Done(PathBuf),
    Live {
        reader: StreamReader,
        len: u64,
        /// File name only, for a window title (hide it when discreet).
        name: String,
        /// Live numbers for a torrent (peers, speed, paused, error). `None`
        /// for plain HTTP sources.
        status: Option<Arc<StreamStatus>>,
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

pub struct FetchService {
    rt: Option<Runtime>,
    cfg: Arc<FetchConfig>,
    state_dir: PathBuf,
    session: Arc<OnceCell<Arc<Session>>>,
    cache: Arc<Mutex<Vec<TorrentStatus>>>,
    /// Raised by `cancel_streams`, then swapped for a fresh flag.
    stream_cancel: Mutex<Arc<AtomicBool>>,
    /// Torrent-side shared state (live statuses, tuning, policy timers).
    tor: torrent::TorrentShared,
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
            tor: torrent::TorrentShared::default(),
        })
    }

    /// Replace the configuration (indexers, download folder). A running
    /// session keeps its network options (proxy, DHT, listener, bound
    /// interface) until the next start; speed limits and the seeding policy
    /// apply immediately.
    pub fn set_config(&mut self, cfg: FetchConfig) {
        publish_proxy(&cfg);
        self.cfg = Arc::new(cfg);
        self.apply_limits();
    }

    fn rt(&self) -> &Runtime {
        self.rt.as_ref().expect("runtime alive until drop")
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
