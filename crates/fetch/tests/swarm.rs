//! Torrent behaviour against a loopback swarm of slow peers. Feature
//! `network`; everything stays on 127.0.0.1 with synthetic data.
#![cfg(feature = "network")]

use jizzcraft_fetch::config::{FetchConfig, ResumeOnStart};
use jizzcraft_fetch::service::{FetchService, ProbeState, Source, StreamTicket, TorrentState};
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, Session, SessionOptions,
    create_torrent, spawn_utils::BlockingSpawner,
};
use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

struct Swarm {
    peers: Vec<SocketAddr>,
    torrent: Vec<u8>,
    data: Vec<u8>,
    _seeders: Vec<std::sync::Arc<Session>>,
    _dirs: Vec<tempfile::TempDir>,
    _rt: tokio::runtime::Runtime,
}

/// `n` seeders of one 2.4 MB "video", each capped at `bps` upload.
fn swarm(n: usize, bps: u32) -> Swarm {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data: Vec<u8> = (0..2_400_000u32).map(|i| (i % 251) as u8).collect();
    let mut peers = Vec::new();
    let mut seeders = Vec::new();
    let mut dirs = Vec::new();
    let mut torrent = Vec::new();
    for _ in 0..n {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample-clip.mkv"), &data).unwrap();
        let known = torrent.clone();
        let (addr, bytes, s) = rt.block_on(async {
            // One metainfo for the whole swarm: same info hash everywhere.
            let bytes = if known.is_empty() {
                create_torrent(
                    &dir.path().join("sample-clip.mkv"),
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

fn service(dl: &Path, st: &Path, resume: ResumeOnStart) -> FetchService {
    FetchService::new(
        FetchConfig {
            download_dir: Some(dl.into()),
            dht: false,
            resume_on_start: resume,
            ..Default::default()
        },
        st.join("state"),
    )
    .unwrap()
}

fn file_in(dir: &Path, bytes: &[u8]) -> Source {
    let p = dir.join("t.torrent");
    std::fs::write(&p, bytes).unwrap();
    Source::TorrentFile(p)
}

fn read_at(r: &mut (impl Read + Seek + ?Sized), pos: u64, n: usize, secs: u64) -> Vec<u8> {
    r.seek(SeekFrom::Start(pos)).unwrap();
    let mut out = vec![0u8; n];
    let mut got = 0;
    let t0 = Instant::now();
    while got < n {
        match r.read(&mut out[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("read: {e}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(secs), "read stalled");
    }
    out.truncate(got);
    out
}

fn wait_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn add_and_open(svc: &FetchService, sw: &Swarm, st: &Path) -> (usize, StreamTicket) {
    let listed = svc
        .list_files_with_peers(file_in(st, &sw.torrent), sw.peers.clone())
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .expect("list");
    let id = svc
        .add_listed(listed, Some(vec![0]))
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .expect("add");
    let t = svc
        .open_reader(id, 0)
        .recv_timeout(Duration::from_secs(40))
        .unwrap()
        .expect("reader");
    (id, t)
}

#[test]
fn three_slow_peers_seek_into_missing_pieces_then_play() {
    let sw = swarm(3, 200 * 1024);
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = service(dl.path(), st.path(), ResumeOnStart::Ask);
    let (_id, t) = add_and_open(&svc, &sw, st.path());
    let StreamTicket::Live {
        mut reader,
        len,
        status,
        ..
    } = t
    else {
        panic!("not complete yet");
    };
    let status = status.expect("torrents carry a status");
    assert_eq!(len, sw.data.len() as u64);
    // Far into the file first: those pieces are not there yet.
    let mid = 1_300_000usize;
    assert_eq!(
        read_at(&mut reader, mid as u64, 40_000, 60),
        sw.data[mid..mid + 40_000]
    );
    // Then the head, then the tail.
    assert_eq!(read_at(&mut reader, 0, 30_000, 60), sw.data[..30_000]);
    let tail = sw.data.len() - 5000;
    assert_eq!(read_at(&mut reader, tail as u64, 5000, 60), sw.data[tail..]);
    wait_for(10, "peers visible in the status", || {
        status.peers.load(Ordering::Relaxed) >= 1
    });
    assert!(!status.paused.load(Ordering::Relaxed));
    assert!(status.error().is_none());
}

#[test]
fn pausing_while_playing_is_reported_not_buffered_silently() {
    let sw = swarm(1, 128 * 1024);
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = service(dl.path(), st.path(), ResumeOnStart::Ask);
    let (id, t) = add_and_open(&svc, &sw, st.path());
    let StreamTicket::Live {
        mut reader, status, ..
    } = t
    else {
        panic!("not complete yet");
    };
    let status = status.unwrap();
    assert_eq!(read_at(&mut reader, 0, 20_000, 60), sw.data[..20_000]);
    svc.pause(id);
    wait_for(10, "paused flag", || status.paused.load(Ordering::Relaxed));
    svc.resume(id);
    wait_for(10, "resumed flag", || {
        !status.paused.load(Ordering::Relaxed)
    });
    assert_eq!(
        read_at(&mut reader, 600_000, 20_000, 60),
        sw.data[600_000..620_000]
    );
}

#[test]
fn magnet_without_peers_times_out_and_can_be_cancelled() {
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = service(dl.path(), st.path(), ResumeOnStart::Ask);
    svc.set_metadata_timeout(Duration::from_secs(2));
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let (rx, probe) = svc.list_files_probed(Source::Magnet(magnet.into()), Vec::new());
    let e = rx
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .err()
        .expect("must fail");
    assert!(e.contains("no peers"), "{e}");
    assert_eq!(probe.state(), ProbeState::Failed);
    assert_eq!(probe.peers(), 0);

    // A long wait ends at once on cancel.
    svc.set_metadata_timeout(Duration::from_secs(120));
    // A peer that accepts the connection and never says a word.
    let mute = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = vec![mute.local_addr().unwrap()];
    std::thread::spawn(move || {
        let held: Vec<_> = mute.incoming().take(4).collect();
        std::thread::sleep(Duration::from_secs(30));
        drop(held);
    });
    let (rx, _p) = svc.list_files_probed(Source::Magnet(magnet.into()), dead);
    std::thread::sleep(Duration::from_millis(500));
    let t0 = Instant::now();
    svc.cancel_streams();
    let e = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .err()
        .expect("must fail");
    assert!(e.contains("cancelled"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(5));
}

#[test]
fn restart_keeps_torrents_and_ask_does_not_resume_them() {
    let sw = swarm(1, 100 * 1024);
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    {
        let svc = service(dl.path(), st.path(), ResumeOnStart::Ask);
        let listed = svc
            .list_files_with_peers(file_in(st.path(), &sw.torrent), sw.peers.clone())
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        svc.add_listed(listed, None)
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        wait_for(30, "some progress", || {
            svc.list()
                .first()
                .is_some_and(|t| t.progress_bytes > 50_000)
        });
        // Let the persistence file settle before the "crash".
        std::thread::sleep(Duration::from_secs(1));
    }
    let svc = service(dl.path(), st.path(), ResumeOnStart::Ask);
    svc.start()
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    wait_for(10, "torrent restored", || !svc.list().is_empty());
    // It may be live for a blink while its files are checked; then it sleeps.
    wait_for(10, "restored torrent paused", || {
        svc.list()
            .first()
            .is_some_and(|t| t.state == TorrentState::Paused)
    });
    let first = svc.list()[0].clone();
    // Adding another torrent later must not wake it either.
    std::thread::sleep(Duration::from_secs(2));
    // `list` answers from the last refresh: ask twice.
    svc.list();
    std::thread::sleep(Duration::from_millis(300));
    let again = svc.list();
    let again = again[0].clone();
    assert_eq!(again.state, TorrentState::Paused);
    assert!(again.progress_bytes.abs_diff(first.progress_bytes) < 64 * 1024);
    // The user says yes.
    let n = svc
        .resume_all()
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(n, 1);
    // (Peers are not persisted and there is no tracker or DHT here, so the
    // download itself cannot finish; leaving the paused state is the point.)
    wait_for(10, "torrent running again", || {
        svc.list()
            .first()
            .is_some_and(|t| t.state != TorrentState::Paused)
    });
}

#[test]
fn disk_preflight_refuses_before_anything_is_written() {
    let sw = swarm(1, 512 * 1024);
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = service(dl.path(), st.path(), ResumeOnStart::Ask);
    // A volume with 100 MiB free: the file fits, the 512 MiB margin does not.
    svc.set_free_space_fn(|_| Some(100 * 1024 * 1024));
    let r = svc
        .add_with_peers(file_in(st.path(), &sw.torrent), sw.peers.clone())
        .recv_timeout(Duration::from_secs(30))
        .unwrap();
    assert!(r.as_ref().is_err_and(|e| e.contains("disk full")), "{r:?}");
    assert!(svc.list().is_empty());
    assert!(!dl.path().join("sample-clip.mkv").exists());
    // Plenty of room: accepted.
    svc.set_free_space_fn(|_| Some(10 * 1024 * 1024 * 1024));
    svc.add_with_peers(file_in(st.path(), &sw.torrent), sw.peers.clone())
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .expect("added");
}
