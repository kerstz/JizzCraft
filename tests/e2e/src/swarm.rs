//! A loopback swarm: `n` librqbit seeders of one file, upload-capped, found
//! through initial peers (no DHT, no tracker).

use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, Session, SessionOptions,
    create_torrent, spawn_utils::BlockingSpawner,
};
use std::net::SocketAddr;
use std::sync::Arc;

pub struct Swarm {
    pub peers: Vec<SocketAddr>,
    /// The `.torrent` file's bytes.
    pub torrent: Vec<u8>,
    pub data: Arc<Vec<u8>>,
    _seeders: Vec<Arc<Session>>,
    _dirs: Vec<tempfile::TempDir>,
    _rt: tokio::runtime::Runtime,
}

pub const FILE_NAME: &str = "sample-clip.webm";

/// `n` seeders of `data`, each capped at `bps` bytes/s of upload.
pub fn swarm(n: usize, bps: u32, data: Arc<Vec<u8>>) -> Swarm {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (mut peers, mut seeders, mut dirs) = (Vec::new(), Vec::new(), Vec::new());
    let mut torrent: Vec<u8> = Vec::new();
    for _ in 0..n {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), &*data).unwrap();
        let known = torrent.clone();
        let (addr, bytes, s) = rt.block_on(async {
            // One metainfo for the whole swarm: the same info hash everywhere.
            let bytes = if known.is_empty() {
                create_torrent(
                    &dir.path().join(FILE_NAME),
                    CreateTorrentOptions {
                        piece_length: Some(16 * 1024),
                        ..Default::default()
                    },
                    &BlockingSpawner::new(1),
                )
                .await
                .unwrap()
                .as_bytes()
                .unwrap()
            } else {
                known.into()
            };
            let s = Session::new_with_opts(
                dir.path().to_path_buf(),
                SessionOptions {
                    disable_trackers: true,
                    disable_local_service_discovery: true,
                    listen: Some(ListenerOptions {
                        listen_addr: "127.0.0.1:0".parse().unwrap(),
                        ..Default::default()
                    }),
                    ratelimits: librqbit::limits::LimitsConfig {
                        upload_bps: std::num::NonZeroU32::new(bps),
                        download_bps: None,
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            s.add_torrent(
                AddTorrent::from_bytes(bytes.clone()),
                Some(AddTorrentOptions {
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
            (s.listen_addr().unwrap(), bytes, s)
        });
        peers.push(SocketAddr::from(([127, 0, 0, 1], addr.port())));
        torrent = bytes.to_vec();
        seeders.push(s);
        dirs.push(dir);
    }
    Swarm {
        peers,
        torrent,
        data,
        _seeders: seeders,
        _dirs: dirs,
        _rt: rt,
    }
}
