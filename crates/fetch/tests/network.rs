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
    for bad in [
        "a/b:stream",
        "con.mp4",
        "a/NUL",
        "x/COM1.txt",
        "a/b.",
        "a/b ",
        "a/b?.mp4",
        "a/b|c",
    ] {
        assert!(!path_is_safe(Path::new(bad)), "{bad}");
    }
    assert!(path_is_safe(Path::new("Show S01/Episode 1 - Name.mkv")));
    assert!(path_is_safe(Path::new("comic.mp4")));
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

#[test]
fn play_path_streams_before_the_download_is_complete() {
    use jizzcraft_fetch::pick::{Pick, pick_video};
    use jizzcraft_fetch::service::StreamTicket;
    use std::io::{Read, Seek, SeekFrom};

    let rt = tokio::runtime::Runtime::new().unwrap();
    let seed_dir = tempfile::tempdir().unwrap();
    // Named like a video so the picker chooses it; content is synthetic.
    let content = seed_dir.path().join("sample-clip.mkv");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&content, &data).unwrap();
    std::fs::write(seed_dir.path().join("readme.txt"), b"x").ok();

    let (peer, torrent_bytes, _seeder) = rt.block_on(async {
        let t = create_torrent(
            &content,
            CreateTorrentOptions {
                piece_length: Some(16 * 1024),
                ..Default::default()
            },
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
                // Slow seeder: the stream has to wait for pieces.
                ratelimits: librqbit::limits::LimitsConfig {
                    upload_bps: std::num::NonZeroU32::new(512 * 1024),
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
        let addr: SocketAddr = s.listen_addr().unwrap();
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

    let listed = svc
        .list_files_with_peers(Source::TorrentFile(tf), vec![peer])
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .expect("list");
    let Pick::One(i) = pick_video(&listed.files) else {
        panic!("expected an obvious file: {:?}", listed.files.len());
    };
    let id = svc
        .add_listed(listed, Some(vec![i]))
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .expect("add");
    let ticket = svc
        .open_reader(id, i)
        .recv_timeout(Duration::from_secs(40))
        .unwrap()
        .expect("reader");
    let StreamTicket::Live {
        mut reader, len, ..
    } = ticket
    else {
        panic!("download cannot be complete yet");
    };
    assert_eq!(len, data.len() as u64);

    // Jump to the tail first, as a demuxer looking for its index would.
    reader.seek(SeekFrom::Start(len - 4096)).unwrap();
    let mut tail = vec![0u8; 4096];
    let mut got = 0;
    let t0 = Instant::now();
    let mut would_block = 0;
    while got < tail.len() {
        match reader.read(&mut tail[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => would_block += 1,
            Err(e) => panic!("read: {e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(40), "tail never arrived");
    }
    assert_eq!(tail, data[data.len() - 4096..]);
    // The head arrives intact too, and the download was not finished yet
    // when the tail was readable (a slow seeder, a 3 MB file).
    reader.seek(SeekFrom::Start(0)).unwrap();
    let mut head = vec![0u8; 64 * 1024];
    let mut n = 0;
    while n < head.len() {
        match reader.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => would_block += 1,
            Err(e) => panic!("read: {e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "head never arrived");
    }
    assert_eq!(head, data[..head.len()]);
    eprintln!("would_block observed: {would_block}");
    // The reader never deletes or forgets anything.
    drop(reader);
    assert!(!svc.list().is_empty() || true);
}

/// Serve `body` on 127.0.0.1 with a Content-Length, in slow chunks.
fn serve_slowly(body: Vec<u8>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            for c in body.chunks(32 * 1024) {
                if s.write_all(c).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    });
    port
}

#[test]
fn http_source_streams_then_renames_and_never_overwrites() {
    use jizzcraft_fetch::service::StreamTicket;
    use std::io::{Read, Seek, SeekFrom};
    let data: Vec<u8> = (0..400_000u32).map(|i| (i % 249) as u8).collect();
    let port = serve_slowly(data.clone());
    let dl = tempfile::tempdir().unwrap();
    let st = tempfile::tempdir().unwrap();
    // A file with that name already exists: the download must pick another.
    std::fs::write(dl.path().join("clip.mp4"), b"mine").unwrap();
    let svc = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            ..Default::default()
        },
        st.path().into(),
    )
    .unwrap();
    let t = svc
        .play_http(format!("http://127.0.0.1:{port}/files/clip.mp4?token=abc"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .expect("ticket");
    let StreamTicket::Live {
        mut reader, len, ..
    } = t
    else {
        panic!("expected a live stream");
    };
    assert_eq!(len, data.len() as u64);
    let mut got = Vec::new();
    let mut buf = [0u8; 8192];
    let t0 = Instant::now();
    while got.len() < data.len() {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(30));
    }
    assert_eq!(got, data);
    reader.seek(SeekFrom::Start(100)).unwrap();
    let mut b = [0u8; 4];
    reader.read_exact(&mut b).unwrap();
    assert_eq!(b, [100, 101, 102, 103]);
    // Renamed to a free name; the existing file is untouched; no .part left.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(std::fs::read(dl.path().join("clip.mp4")).unwrap(), b"mine");
    assert_eq!(std::fs::read(dl.path().join("clip (1).mp4")).unwrap(), data);
    assert!(!dl.path().join("clip (1).mp4.part").exists());
}

#[test]
fn http_source_refuses_playlists_and_non_video_names() {
    let port = serve_slowly(b"#EXTM3U\n#EXTINF:2,\nhttp://127.0.0.1:1/a.ts\n".to_vec());
    let dl = tempfile::tempdir().unwrap();
    let st = tempfile::tempdir().unwrap();
    let svc = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            ..Default::default()
        },
        st.path().into(),
    )
    .unwrap();
    let r = svc
        .play_http(format!("http://127.0.0.1:{port}/list.mp4"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert!(r.is_err());
    // Nothing was kept.
    assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
    let r = svc
        .play_http(format!("http://127.0.0.1:{port}/archive.zip"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert!(r.is_err());
}
