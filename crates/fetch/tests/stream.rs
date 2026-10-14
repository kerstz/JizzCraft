//! `stream.open` path: seekable HTTP through `Range`, no file written.
#![cfg(feature = "network")]

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::service::{FetchService, StreamTicket};

/// The proxy policy is process-global (like the app), so tests that build a
/// service run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tiny HTTP/1.1 server: honours `Range` unless `ranges` is false.
fn serve(body: Vec<u8>, ranges: bool, hits: Arc<AtomicUsize>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let body = body.clone();
            let hits = hits.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                hits.fetch_add(1, Ordering::SeqCst);
                let range = req.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                });
                match (range, ranges) {
                    (Some(r), true) => {
                        let (a, b) = r.split_once('-').unwrap();
                        let a: usize = a.trim().parse().unwrap();
                        let b: usize = b.trim().parse::<usize>().unwrap().min(body.len() - 1);
                        let part = &body[a..=b];
                        let _ = write!(
                            s,
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            part.len()
                        );
                        let _ = s.write_all(part);
                    }
                    _ => {
                        let _ = write!(
                            s,
                            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = s.write_all(&body);
                    }
                }
            });
        }
    });
    port
}

fn svc(dl: &std::path::Path, st: &std::path::Path) -> FetchService {
    FetchService::new(
        FetchConfig {
            download_dir: Some(dl.into()),
            dht: false,
            ..Default::default()
        },
        st.into(),
    )
    .unwrap()
}

fn read_exact_patient(r: &mut impl Read, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    while out.len() < n {
        let want = (n - out.len()).min(buf.len());
        match r.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(k) => out.extend_from_slice(&buf[..k]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "stalled (the spinner won this round)"
        );
    }
    out
}

#[test]
fn range_stream_is_byte_exact_seekable_and_writes_nothing() {
    let _g = serial();
    let data: Vec<u8> = (0..3_500_000u32).map(|i| (i % 251) as u8).collect();
    let hits = Arc::new(AtomicUsize::new(0));
    let port = serve(data.clone(), true, hits.clone());
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = svc(dl.path(), st.path());
    let t = svc
        .stream_url(format!("http://127.0.0.1:{port}/v/clip.mp4"))
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
    // Sequential start, then a far seek, then back, then the tail.
    assert_eq!(read_exact_patient(&mut reader, 100_000), data[..100_000]);
    reader.seek(SeekFrom::Start(2_900_000)).unwrap();
    assert_eq!(
        read_exact_patient(&mut reader, 70_000),
        data[2_900_000..2_970_000]
    );
    reader.seek(SeekFrom::Start(500_000)).unwrap();
    assert_eq!(
        read_exact_patient(&mut reader, 10_000),
        data[500_000..510_000]
    );
    reader.seek(SeekFrom::End(-1000)).unwrap();
    assert_eq!(
        read_exact_patient(&mut reader, 5000),
        data[data.len() - 1000..]
    );
    assert!(hits.load(Ordering::SeqCst) >= 3);
    // Nothing on disk: not a .part, not the final file.
    assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
}

#[test]
fn server_without_ranges_falls_back_to_the_saved_stream() {
    let _g = serial();
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 249) as u8).collect();
    let port = serve(data.clone(), false, Arc::new(AtomicUsize::new(0)));
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = svc(dl.path(), st.path());
    let t = svc
        .stream_url(format!("http://127.0.0.1:{port}/clip.mp4"))
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
    assert_eq!(read_exact_patient(&mut reader, data.len()), data);
}

#[test]
fn playlists_are_refused_before_any_byte_is_kept() {
    let _g = serial();
    let port = serve(
        b"#EXTM3U\n#EXTINF:2,\nhttp://127.0.0.1:1/a.ts\n".to_vec(),
        true,
        Arc::new(AtomicUsize::new(0)),
    );
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = svc(dl.path(), st.path());
    let r = svc
        .stream_url(format!("http://127.0.0.1:{port}/list.mp4"))
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert!(r.is_err());
    assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
}

#[test]
fn user_defined_source_search_maps_json_and_keeps_the_secret_in_headers() {
    let _g = serial();
    use std::sync::Mutex;
    let seen: Arc<Mutex<String>> = Arc::default();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            *seen2.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = br#"{"data":[{"name":"Clip","play":"http://127.0.0.1:9/a.mp4"}]}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(body);
        }
    });
    let def = jizzcraft_fetch::sources::parse_import(
        format!(
            r#"{{"name":"mine","search_url":"http://127.0.0.1:{port}/s?q={{query}}",
                "items":"/data","fields":{{"title":"/name","media":"/play"}},
                "auth_header":"Authorization","auth_prefix":"Bearer "}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let mut def = def;
    def.id = 1;
    def.secret = jizzcraft_fetch::Secret::new("TOKEN123");
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let svc = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            sources: vec![def],
            ..Default::default()
        },
        st.path().into(),
    )
    .unwrap();
    let out = svc
        .search(None, "hello world", &[], 10, Arc::default())
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert_eq!((out.queried, out.failed), (1, 0));
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.results[0].title, "Clip");
    assert_eq!(
        out.results[0].indexer_id,
        jizzcraft_fetch::sources::ID_BASE + 1
    );
    let req = seen.lock().unwrap().clone();
    assert!(req.starts_with("GET /s?q=hello%20world "), "{req}");
    assert!(
        req.contains("authorization: Bearer TOKEN123")
            || req.contains("Authorization: Bearer TOKEN123")
    );
}

#[cfg(unix)]
#[test]
fn resolver_runs_argv_only_with_a_clean_environment_and_a_timeout() {
    let _g = serial();
    use jizzcraft_fetch::resolver::{ResolverDef, run};
    let def = |argv: &[&str], t: u32| ResolverDef {
        id: 1,
        name: "t".into(),
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        timeout_secs: t,
        enabled: true,
        honours_proxy: false,
    };
    // The page address is one argument, even with shell metacharacters.
    let evil = "https://x.invalid/a?b=c;touch${IFS}pwned&d=$(id)";
    let sh = def(
        &[
            "/bin/sh",
            "-c",
            r#"printf '{"url":"%s","height":720}' "$1""#,
            "sh",
            "{url}",
        ],
        5,
    );
    let f = run(&sh, evil).unwrap();
    assert_eq!(f[0].url, evil);

    // Environment is emptied: a secret in ours never reaches the child.
    unsafe { std::env::set_var("JIZZ_SECRET_TEST", "leak") };
    let env = def(
        &[
            "/bin/sh",
            "-c",
            r#"printf '{"url":"https://e.invalid/%s.mp4"}' "${JIZZ_SECRET_TEST:-none}""#,
            "sh",
            "{url}",
        ],
        5,
    );
    assert_eq!(
        run(&env, "https://x.invalid/").unwrap()[0].url,
        "https://e.invalid/none.mp4"
    );

    // Timeout kills it.
    let slow = def(&["/bin/sh", "-c", "sleep 30", "sh", "{url}"], 1);
    let t0 = Instant::now();
    assert!(run(&slow, "https://x.invalid/").is_err());
    assert!(t0.elapsed() < Duration::from_secs(10));

    // Output cap.
    let flood = def(
        &[
            "/bin/sh",
            "-c",
            "yes aaaaaaaa | head -c 3000000",
            "sh",
            "{url}",
        ],
        10,
    );
    assert!(run(&flood, "https://x.invalid/").is_err());
}

#[test]
fn require_proxy_blocks_every_request_before_a_socket_opens() {
    let _g = serial();
    // Trap: any connection at all is a failure.
    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = trap.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    std::thread::spawn(move || {
        for s in trap.incoming() {
            if s.is_ok() {
                h2.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let (dl, st) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut def = jizzcraft_fetch::sources::parse_import(
        format!(
            r#"{{"name":"s","search_url":"http://127.0.0.1:{port}/s?q={{query}}",
                "fields":{{"title":"/t","media":"/m"}}}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    def.id = 1;
    let svc = FetchService::new(
        FetchConfig {
            download_dir: Some(dl.path().into()),
            dht: false,
            require_proxy: true,
            sources: vec![def],
            ..Default::default()
        },
        st.path().into(),
    )
    .unwrap();
    let out = svc
        .search(None, "x", &[], 10, Arc::default())
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!((out.queried, out.failed, out.results.len()), (1, 1, 0));
    let r = svc
        .stream_url(format!("http://127.0.0.1:{port}/a.mp4"))
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert!(r.is_err());
    let r = svc
        .play_http(format!("http://127.0.0.1:{port}/a.mp4"))
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert!(r.is_err());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a socket was opened");
}
