//! Feature `network` only. Everything stays on 127.0.0.1: a canned HTTP
//! indexer and a two-session swarm. No real network, synthetic data only.
#![cfg(feature = "network")]

use jizzcraft_fetch::config::{FetchConfig, Indexer};
use jizzcraft_fetch::secret::Secret;
use jizzcraft_fetch::service::{FetchService, Source, path_is_safe};
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, Session, SessionOptions,
    create_torrent, spawn_utils::BlockingSpawner,
};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const XML: &str = r#"<rss xmlns:torznab="x"><channel>
<item><title>Sample Clip 01</title><link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link>
<torznab:attr name="seeders" value="3"/></item></channel></rss>"#;

fn mock_indexer(body: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let line = String::from_utf8_lossy(&buf[..n])
                .lines()
                .next()
                .unwrap_or("")
                .to_owned();
            let _ = tx.send(line);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, rx)
}

fn cfg_with(port: u16, dl: &Path) -> FetchConfig {
    FetchConfig {
        indexers: vec![Indexer {
            id: 1,
            name: "mock".into(),
            url: format!("http://127.0.0.1:{port}/torznab"),
            api_key: Secret::new("k3y"),
            enabled: true,
        }],
        download_dir: Some(dl.to_path_buf()),
        dht: false,
        ..Default::default()
    }
}

#[test]
fn search_hits_mock_indexer() {
    let (port, reqs) = mock_indexer(XML);
    let dl = tempfile::tempdir().unwrap();
    let st = tempfile::tempdir().unwrap();
    let svc = FetchService::new(cfg_with(port, dl.path()), st.path().into()).unwrap();
    let out = svc
        .search(None, "sample", &[], 10, Arc::new(AtomicBool::new(false)))
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!((out.queried, out.failed), (1, 0));
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.results[0].title, "Sample Clip 01");
    let line = reqs.recv().unwrap();
    assert!(line.contains("apikey=k3y") && line.contains("t=search") && line.contains("q=sample"));
}

#[test]
fn indexer_error_is_counted_without_leaking() {
    let (port, _r) = mock_indexer(r#"<error code="100" description="bad key"/>"#);
    let dl = tempfile::tempdir().unwrap();
    let st = tempfile::tempdir().unwrap();
    let svc = FetchService::new(cfg_with(port, dl.path()), st.path().into()).unwrap();
    let out = svc
        .search(
            None,
            "private words",
            &[],
            10,
            Arc::new(AtomicBool::new(false)),
        )
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!((out.queried, out.failed, out.results.len()), (1, 1, 0));
}

#[test]
fn unsafe_paths_are_rejected() {
    assert!(path_is_safe(Path::new("a/b.mp4")));
    assert!(!path_is_safe(Path::new("../x")));
    assert!(!path_is_safe(Path::new("/etc/x")));
    assert!(!path_is_safe(Path::new("")));
}

#[test]
fn local_swarm_downloads_and_never_overwrites() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let seed_dir = tempfile::tempdir().unwrap();
    let content = seed_dir.path().join("sample-clip.bin");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&content, &data).unwrap();

    // Seeder: plain librqbit session on loopback, no DHT, no trackers.
    let (peer, torrent_bytes, _seeder) = rt.block_on(async {
        let t = create_torrent(
            &content,
            CreateTorrentOptions::default(),
            &BlockingSpawner::new(1),
        )
        .await
        .unwrap();
        let bytes = t.as_bytes().unwrap();
        let s = Session::new_with_opts(
            seed_dir.path().to_path_buf(),
            SessionOptions {
                disable_trackers: true,
                disable_local_service_discovery: true,
                listen: Some(ListenerOptions {
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                }),
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
        let addr: SocketAddr = s.listen_addr().expect("listening");
        (addr, bytes, s)
    });
    let peer = SocketAddr::from(([127, 0, 0, 1], peer.port()));

    let dl = tempfile::tempdir().unwrap();
    let st = tempfile::tempdir().unwrap();
    let tf = st.path().join("t.torrent");
    std::fs::write(&tf, &torrent_bytes).unwrap();
    let svc = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            ..Default::default()
        },
        st.path().join("state"),
    )
    .unwrap();
    // A file with the same name already exists: it must stay untouched.
    let target = dl.path().join("sample-clip.bin");
    std::fs::write(&target, b"mine").unwrap();
    let r = svc
        .add_with_peers(Source::TorrentFile(tf.clone()), vec![peer])
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    let _ = r; // either refused or added; the existing file must survive.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(std::fs::read(&target).unwrap(), b"mine");
    std::fs::remove_file(&target).unwrap();

    svc.add_with_peers(Source::TorrentFile(tf), vec![peer])
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .ok();
    let t0 = Instant::now();
    loop {
        let l = svc.list();
        if l.first().is_some_and(|t| t.progress() >= 1.0) {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(40),
            "download timed out: {l:?}"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    assert_eq!(std::fs::read(&target).unwrap(), data);
}
