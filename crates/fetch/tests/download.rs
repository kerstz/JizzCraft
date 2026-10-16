//! The save queue end to end against a loopback "site": files, pages,
//! HLS, DASH, limits, pauses and the kill switch. Synthetic bytes only.
#![cfg(feature = "network")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::config::SaveSettings;
use jizzcraft_fetch::download::manager::{DownloadRequest, SaveSource};
use jizzcraft_fetch::download::{JobId, JobState, JobView};
use jizzcraft_fetch::errors::{self, classify};
use jizzcraft_fetch::formats::{Format, Meta};
use jizzcraft_fetch::pick::Quality;
use jizzcraft_fetch::service::{FetchService, Via};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    reset_policy();
    g
}

fn reset_policy() {
    let d = tempfile::tempdir().unwrap();
    let _ = FetchService::new(
        FetchConfig {
            dht: false,
            ..Default::default()
        },
        d.path().into(),
    )
    .unwrap();
}

// ---- a tiny site ---------------------------------------------------------

#[derive(Clone)]
struct Resp {
    status: u16,
    ctype: &'static str,
    body: Vec<u8>,
    etag: &'static str,
    /// Send half of the body, then hang up.
    cut: bool,
    /// Bytes per second.
    bps: Option<u64>,
}

fn ok(ctype: &'static str, body: impl Into<Vec<u8>>) -> Resp {
    Resp {
        status: 200,
        ctype,
        body: body.into(),
        etag: "\"e1\"",
        cut: false,
        bps: None,
    }
}

fn html(body: &str) -> Resp {
    ok("text/html", body.as_bytes())
}

fn missing() -> Resp {
    Resp {
        status: 404,
        ..ok("text/plain", Vec::new())
    }
}

/// path, index of this request among all requests, `Range` start.
type Handler = Arc<dyn Fn(&str, usize, Option<usize>) -> Resp + Send + Sync>;

struct Site {
    port: u16,
    hits: Arc<AtomicUsize>,
    _log: Arc<Mutex<Vec<String>>>,
}

impl Site {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

fn slow_write(s: &mut TcpStream, data: &[u8], bps: Option<u64>) {
    match bps {
        None => {
            let _ = s.write_all(data);
        }
        Some(b) => {
            for c in data.chunks((b / 10).max(1) as usize) {
                if s.write_all(c).is_err() || s.flush().is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn serve(h: Handler) -> Site {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (h2, l2) = (hits.clone(), log.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let n = h2.fetch_add(1, Ordering::SeqCst);
            let (h, log) = (h.clone(), l2.clone());
            std::thread::spawn(move || {
                let mut req = Vec::new();
                let mut buf = [0u8; 2048];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(k) => req.extend_from_slice(&buf[..k]),
                    }
                }
                let text = String::from_utf8_lossy(&req).to_string();
                let path = text
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                log.lock().unwrap().push(path.clone());
                let from = text.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .and_then(|r| r.split('-').next().and_then(|a| a.trim().parse().ok()))
                });
                let r = h(&path, n, from);
                let total = r.body.len();
                match from {
                    Some(a) if r.status == 200 && a < total => {
                        let part = &r.body[a..];
                        let _ = write!(
                            s,
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: {}\r\nETag: {}\r\nContent-Range: bytes {a}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            r.ctype,
                            r.etag,
                            total - 1,
                            part.len()
                        );
                        let part = if r.cut { &part[..part.len() / 2] } else { part };
                        slow_write(&mut s, part, r.bps);
                    }
                    _ => {
                        let _ = write!(
                            s,
                            "HTTP/1.1 {} X\r\nContent-Type: {}\r\nETag: {}\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
                            r.status, r.ctype, r.etag
                        );
                        let body = if r.cut {
                            &r.body[..total / 2]
                        } else {
                            &r.body[..]
                        };
                        slow_write(&mut s, body, r.bps);
                    }
                }
            });
        }
    });
    Site {
        port,
        hits,
        _log: log,
    }
}

/// `ftyp`, `mdat`, `moov`: a file the sniffer calls media.
fn mp4(n: usize, tag: u8) -> Vec<u8> {
    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }
    let mut out = atom(b"ftyp", b"isom\0\0\x02\0isomiso2");
    out.extend(atom(b"mdat", &vec![tag; n]));
    out.extend(atom(b"moov", &[0u8; 64]));
    out
}

/// A route table: exact paths, 404 for the rest.
fn table(routes: Vec<(&'static str, Resp)>) -> Handler {
    Arc::new(move |p: &str, _, _| {
        let bare = p.split('?').next().unwrap_or(p);
        routes
            .iter()
            .find(|(k, _)| *k == bare)
            .map_or_else(missing, |(_, r)| r.clone())
    })
}

// ---- the service ---------------------------------------------------------

struct Rig {
    svc: FetchService,
    dir: tempfile::TempDir,
    _state: tempfile::TempDir,
}

fn rig_with(edit: impl FnOnce(&mut FetchConfig)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut cfg = FetchConfig {
        dht: false,
        save: SaveSettings {
            dir: Some(dir.path().to_owned()),
            ..SaveSettings::default()
        },
        ..Default::default()
    };
    edit(&mut cfg);
    let svc = FetchService::new(cfg, state.path().into()).unwrap();
    Rig {
        svc,
        dir,
        _state: state,
    }
}

fn rig() -> Rig {
    rig_with(|_| {})
}

impl Rig {
    fn start(&self, url: &str) -> JobId {
        self.svc.download_start(DownloadRequest::url(url)).unwrap()[0]
    }

    fn job(&self, id: JobId) -> JobView {
        self.svc
            .download_list()
            .into_iter()
            .find(|j| j.id == id)
            .expect("job listed")
    }

    /// Wait until every listed job is over.
    fn settle(&self) -> Vec<JobView> {
        let t0 = Instant::now();
        loop {
            let l = self.svc.download_list();
            if !l.is_empty() && l.iter().all(|j| j.state.is_over()) {
                return l;
            }
            assert!(t0.elapsed() < Duration::from_secs(40), "timed out");
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn until(&self, what: &str, mut f: impl FnMut(&Rig) -> bool) {
        let t0 = Instant::now();
        while !f(self) {
            assert!(t0.elapsed() < Duration::from_secs(30), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn files(&self) -> Vec<String> {
        names(self.dir.path())
    }

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join(name)).unwrap()
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// ---- direct files and pages ------------------------------------------------

#[test]
fn direct_url_saved_byte_identical_with_title_name() {
    let _g = serial();
    let body = mp4(300_000, 7);
    let site = serve(table(vec![("/v.mp4", ok("video/mp4", body.clone()))]));
    let rig = rig();
    // A bare address has no title: the neutral name, never the file name.
    let id = rig.start(&site.url("/v.mp4"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    assert_eq!(rig.read("video.mp4"), body);
    // A picked format with a title is named after the title.
    let f = Format {
        url: site.url("/v.mp4"),
        height: Some(720),
        protocol: "http".into(),
        headers: vec![],
        audio_only: false,
        video_only: false,
        bitrate: None,
    };
    let meta = Meta {
        title: Some("My Clip".into()),
        ..Meta::default()
    };
    let ids = rig
        .svc
        .download_start(DownloadRequest {
            source: SaveSource::Picked {
                format: f,
                meta,
                page: None,
                via: Via::Direct,
                entry: 0,
                index: 0,
            },
            quality: Quality::default(),
            dir: None,
            all_entries: false,
        })
        .unwrap();
    assert_ne!(ids[0], id);
    rig.until("second done", |r| r.job(ids[0]).state == JobState::Done);
    assert_eq!(rig.read("My Clip.mp4"), body);
    assert_eq!(rig.files(), ["My Clip.mp4", "video.mp4"]);
    let fin = rig.svc.download_take_finished();
    assert_eq!((fin.done, fin.failed), (2, 0));
    assert_eq!(fin.dirs.len(), 1);
    // A second call has nothing more to say.
    assert_eq!(rig.svc.download_take_finished().done, 0);
}

#[test]
fn page_url_probed_then_saved() {
    let _g = serial();
    let body = mp4(120_000, 3);
    let site = serve(table(vec![
        (
            "/page",
            html(
                r#"<meta property="og:title" content="Probed Title">
                   <meta property="og:video" content="/media/clip.mp4">"#,
            ),
        ),
        ("/media/clip.mp4", ok("video/mp4", body.clone())),
    ]));
    let rig = rig();
    let id = rig.start(&site.url("/page"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    assert_eq!(rig.job(id).title.as_deref(), Some("Probed Title"));
    assert_eq!(rig.read("Probed Title.mp4"), body);
    assert_eq!(rig.files(), ["Probed Title.mp4"]);
}

#[test]
fn page_with_three_videoobjects_queues_three_with_all_entries() {
    let _g = serial();
    let site = serve(table(vec![
        (
            "/page",
            html(
                r#"<script type="application/ld+json">{"@context":"x","@graph":[
                {"@type":"VideoObject","name":"One","contentUrl":"/a.mp4"},
                {"@type":"VideoObject","name":"Two","contentUrl":"/b.mp4"},
                {"@type":"VideoObject","name":"Three","contentUrl":"/c.mp4"}]}</script>"#,
            ),
        ),
        ("/a.mp4", ok("video/mp4", mp4(20_000, 1))),
        ("/b.mp4", ok("video/mp4", mp4(20_000, 2))),
        ("/c.mp4", ok("video/mp4", mp4(20_000, 3))),
    ]));
    let rig = rig();
    let mut req = DownloadRequest::url(site.url("/page"));
    req.all_entries = true;
    rig.svc.download_start(req).unwrap();
    rig.until("three jobs", |r| r.svc.download_list().len() == 3);
    let l = rig.settle();
    assert!(l.iter().all(|j| j.state == JobState::Done));
    assert_eq!(rig.files(), ["One.mp4", "Three.mp4", "Two.mp4"]);
    assert_eq!(rig.read("Two.mp4"), mp4(20_000, 2));
    let fin = rig.svc.download_take_finished();
    assert_eq!(fin.done, 3);
    // Without the flag only the first is taken.
    let rig2 = rig_with(|_| {});
    rig2.start(&site.url("/page"));
    rig2.settle();
    assert_eq!(rig2.files(), ["One.mp4"]);
}

#[test]
fn expired_link_renewed_through_page() {
    let _g = serial();
    let body = mp4(300_000, 9);
    let pages = Arc::new(AtomicUsize::new(0));
    let (p2, b2) = (pages.clone(), body.clone());
    let h: Handler = Arc::new(move |path, _, from| {
        if path == "/page" {
            let n = p2.fetch_add(1, Ordering::SeqCst) + 1;
            return html(&format!(
                r#"<meta property="og:video" content="/v.mp4?t={n}">"#
            ));
        }
        // Token 1 gives half a file and is then withdrawn.
        if path == "/v.mp4?t=1" {
            return Resp {
                cut: from.is_none() || from == Some(0),
                status: if from.is_some_and(|f| f > 0) {
                    403
                } else {
                    200
                },
                ..ok("video/mp4", b2.clone())
            };
        }
        if path == "/v.mp4?t=2" {
            return ok("video/mp4", b2.clone());
        }
        missing()
    });
    let site = serve(h);
    let rig = rig();
    rig.start(&site.url("/page"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    assert_eq!(rig.read("video.mp4"), body);
    assert_eq!(pages.load(Ordering::SeqCst), 2);
}

// ---- HLS and DASH -----------------------------------------------------------

fn ts_seg(i: usize) -> Vec<u8> {
    let mut v = vec![0x47];
    v.extend((0..999 + i * 7).map(|k| ((k + i) % 200) as u8));
    v
}

fn hls_playlist(n: usize, extra: &str) -> String {
    let mut p = format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n{extra}");
    for i in 0..n {
        p.push_str(&format!("#EXTINF:2,\ns{i}.ts\n"));
    }
    p.push_str("#EXT-X-ENDLIST\n");
    p
}

#[test]
fn hls_ts_saved_as_one_ts_file() {
    let _g = serial();
    let n = 6;
    let mut routes = vec![(
        "/m.m3u8",
        ok("application/vnd.apple.mpegurl", hls_playlist(n, "")),
    )];
    let names: Vec<&'static str> = (0..n)
        .map(|i| &*Box::leak(format!("/s{i}.ts").into_boxed_str()))
        .collect();
    for (i, nm) in names.iter().enumerate() {
        routes.push((nm, ok("video/mp2t", ts_seg(i))));
    }
    let site = serve(table(routes));
    let rig = rig();
    rig.start(&site.url("/m.m3u8"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    let want: Vec<u8> = (0..n).flat_map(ts_seg).collect();
    assert_eq!(rig.read("video.ts"), want);
    assert_eq!(rig.files(), ["video.ts"]);
}

fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let n: usize = parts.iter().map(|p| p.len()).sum::<usize>() + 8;
    let mut out = (n as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

fn fbx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    bx(kind, &[&[0, 0, 0, 0], body])
}

fn init_mp4(handler: &[u8; 4], format: &[u8; 4]) -> Vec<u8> {
    let mut mvhd = vec![0u8; 96];
    mvhd.extend_from_slice(&2u32.to_be_bytes());
    let mut tk = vec![0u8; 8];
    tk.extend_from_slice(&1u32.to_be_bytes());
    tk.extend_from_slice(&[0u8; 68]);
    let entry = bx(format, &[&[0u8; 8]]);
    let mut stsd = 1u32.to_be_bytes().to_vec();
    stsd.extend_from_slice(&entry);
    let stbl = bx(b"stbl", &[&fbx(b"stsd", &stsd)]);
    let minf = bx(b"minf", &[&stbl]);
    let mut hd = vec![0u8; 4];
    hd.extend_from_slice(handler);
    hd.extend_from_slice(&[0u8; 12]);
    let mdia = bx(b"mdia", &[&fbx(b"hdlr", &hd), &minf]);
    let trak = bx(b"trak", &[&fbx(b"tkhd", &tk), &mdia]);
    let mut tx = 1u32.to_be_bytes().to_vec();
    tx.extend_from_slice(&[0u8; 16]);
    let mvex = bx(b"mvex", &[&fbx(b"trex", &tx)]);
    let moov = bx(b"moov", &[&fbx(b"mvhd", &mvhd), &trak, &mvex]);
    let mut out = bx(b"ftyp", &[b"isom", &[0, 0, 0, 0], b"isomiso6"]);
    out.extend_from_slice(&moov);
    out
}

fn frag_mp4(seq: u32, payload: &[u8]) -> Vec<u8> {
    let traf = bx(b"traf", &[&fbx(b"tfhd", &1u32.to_be_bytes())]);
    let moof = bx(b"moof", &[&fbx(b"mfhd", &seq.to_be_bytes()), &traf]);
    let mut out = moof;
    out.extend_from_slice(&bx(b"mdat", &[payload]));
    out
}

fn count_of(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

#[test]
fn hls_fmp4_init_written_once() {
    let _g = serial();
    let init = init_mp4(b"vide", b"avc1");
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nf0.m4s\n\
#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nf1.m4s\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nf2.m4s\n#EXT-X-ENDLIST\n";
    let site = serve(table(vec![
        ("/m.m3u8", ok("application/vnd.apple.mpegurl", pl)),
        ("/init.mp4", ok("video/mp4", init.clone())),
        ("/f0.m4s", ok("video/mp4", frag_mp4(0, b"AAAA"))),
        ("/f1.m4s", ok("video/mp4", frag_mp4(1, b"BBBB"))),
        ("/f2.m4s", ok("video/mp4", frag_mp4(2, b"CCCC"))),
    ]));
    let rig = rig();
    rig.start(&site.url("/m.m3u8"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    let out = rig.read("video.mp4");
    let mut want = init.clone();
    for (i, t) in [b"AAAA", b"BBBB", b"CCCC"].iter().enumerate() {
        want.extend(frag_mp4(i as u32, *t));
    }
    assert_eq!(out, want);
    assert_eq!(count_of(&out, b"moov"), 1);
}

#[test]
fn dash_separate_audio_saved_as_one_mp4() {
    let _g = serial();
    let mpd = r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period>
<AdaptationSet mimeType="video/mp4"><Representation id="v1" bandwidth="900000" height="720">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="vinit.mp4" media="v$Number$.m4s"/>
</Representation></AdaptationSet>
<AdaptationSet mimeType="audio/mp4" lang="en"><Representation id="a1" bandwidth="64000">
<SegmentTemplate timescale="1" duration="2" startNumber="0" initialization="ainit.mp4" media="a$Number$.m4s"/>
</Representation></AdaptationSet></Period></MPD>"#;
    let site = serve(table(vec![
        ("/s.mpd", ok("application/dash+xml", mpd)),
        ("/vinit.mp4", ok("video/mp4", init_mp4(b"vide", b"avc1"))),
        ("/ainit.mp4", ok("video/mp4", init_mp4(b"soun", b"mp4a"))),
        ("/v0.m4s", ok("video/mp4", frag_mp4(0, b"V0"))),
        ("/v1.m4s", ok("video/mp4", frag_mp4(1, b"V1"))),
        ("/a0.m4s", ok("video/mp4", frag_mp4(0, b"A0"))),
        ("/a1.m4s", ok("video/mp4", frag_mp4(1, b"A1"))),
    ]));
    let rig = rig();
    rig.start(&site.url("/s.mpd"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    let out = rig.read("video.mp4");
    assert_eq!(count_of(&out, b"moov"), 1);
    assert_eq!(count_of(&out, b"trak"), 2);
    for tag in [&b"V0"[..], b"A0", b"V1", b"A1"] {
        assert_eq!(count_of(&out, tag), 1);
    }
    assert_eq!(rig.files(), ["video.mp4"]);
}

#[test]
fn aes128_saved_decrypted() {
    let _g = serial();
    let key = [5u8; 16];
    let iv: [u8; 16] = std::array::from_fn(|i| i as u8);
    let plain0 = ts_seg(0);
    let plain1 = ts_seg(1);
    let enc = |d: &[u8]| {
        cbc::Encryptor::<Aes128>::new((&key).into(), (&iv).into())
            .encrypt_padded_vec_mut::<Pkcs7>(d)
    };
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n\
#EXTINF:2,\ns0.ts\n#EXTINF:2,\ns1.ts\n#EXT-X-ENDLIST\n";
    let site = serve(table(vec![
        ("/m.m3u8", ok("application/vnd.apple.mpegurl", pl)),
        ("/key.bin", ok("application/octet-stream", key.to_vec())),
        ("/s0.ts", ok("video/mp2t", enc(&plain0))),
        ("/s1.ts", ok("video/mp2t", enc(&plain1))),
    ]));
    let rig = rig();
    rig.start(&site.url("/m.m3u8"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    let mut want = plain0;
    want.extend(plain1);
    assert_eq!(rig.read("video.ts"), want);
}

#[test]
fn live_refused_with_sentence() {
    let _g = serial();
    let pl = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2,\ns0.ts\n#EXTINF:2,\ns1.ts\n";
    let site = serve(table(vec![
        ("/live.m3u8", ok("application/vnd.apple.mpegurl", pl)),
        ("/s0.ts", ok("video/mp2t", ts_seg(0))),
        ("/s1.ts", ok("video/mp2t", ts_seg(1))),
    ]));
    let rig = rig();
    rig.start(&site.url("/live.m3u8"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Failed);
    assert_eq!(l[0].error, Some(errors::LIVE_NOT_SAVED));
    assert!(rig.files().is_empty());
}

#[test]
fn lost_segment_fails_and_leaves_nothing() {
    let _g = serial();
    let site = serve(table(vec![
        (
            "/m.m3u8",
            ok("application/vnd.apple.mpegurl", hls_playlist(4, "")),
        ),
        ("/s0.ts", ok("video/mp2t", ts_seg(0))),
        ("/s2.ts", ok("video/mp2t", ts_seg(2))),
        ("/s3.ts", ok("video/mp2t", ts_seg(3))),
    ]));
    let rig = rig();
    // A file already in the folder stays as it is.
    std::fs::write(rig.dir.path().join("keep.txt"), b"mine").unwrap();
    rig.start(&site.url("/m.m3u8"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Failed);
    assert_eq!(l[0].error, Some(errors::NOT_FOUND));
    assert_eq!(rig.files(), ["keep.txt"]);
    assert_eq!(rig.read("keep.txt"), b"mine");
    let fin = rig.svc.download_take_finished();
    assert_eq!((fin.done, fin.failed), (0, 1));
}

// ---- limits, pauses, shutdown ------------------------------------------------

fn slow_file(bps: u64, n: usize) -> Handler {
    let body = mp4(n, 4);
    Arc::new(move |_, _, _| Resp {
        bps: Some(bps),
        ..ok("video/mp4", body.clone())
    })
}

#[test]
fn concurrency_limit_holds() {
    let _g = serial();
    let site = serve(slow_file(400_000, 200_000));
    let rig = rig_with(|c| c.save.concurrency = 2);
    for i in 0..4 {
        rig.start(&site.url(&format!("/{i}.mp4")));
    }
    let t0 = Instant::now();
    let mut peak = 0;
    loop {
        let l = rig.svc.download_list();
        peak = peak.max(l.iter().filter(|j| j.state.holds_slot()).count());
        if l.iter().all(|j| j.state.is_over()) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(40));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(peak <= 2, "peak {peak}");
    assert!(peak >= 1);
    // Four files under four distinct names.
    assert_eq!(
        rig.files(),
        [
            "video (1).mp4",
            "video (2).mp4",
            "video (3).mp4",
            "video.mp4"
        ]
    );
}

#[test]
fn speed_cap_is_honoured() {
    let _g = serial();
    let body = mp4(2 * 1024 * 1024, 5);
    let site = serve(table(vec![("/v.mp4", ok("video/mp4", body))]));
    let rig = rig_with(|c| c.down_limit_mibps = Some(1));
    let t0 = Instant::now();
    rig.start(&site.url("/v.mp4"));
    let l = rig.settle();
    assert_eq!(l[0].state, JobState::Done, "{:?}", l[0].error);
    assert!(
        t0.elapsed() >= Duration::from_millis(1800),
        "{:?}",
        t0.elapsed()
    );
}

#[test]
fn pause_resume_cancel_states() {
    let _g = serial();
    let site = serve(slow_file(150_000, 500_000));
    let rig = rig_with(|c| c.save.concurrency = 1);
    let a = rig.start(&site.url("/a.mp4"));
    let b = rig.start(&site.url("/b.mp4"));
    assert_eq!(rig.job(b).state, JobState::Queued);
    // A queued job can be held and released.
    assert!(rig.svc.download_pause(b));
    assert_eq!(rig.job(b).state, JobState::Paused);
    assert!(rig.svc.download_resume(b));
    assert_eq!(rig.job(b).state, JobState::Queued);
    rig.until("running", |r| {
        let j = r.job(a);
        j.state == JobState::Running && j.done_bytes > 5_000
    });
    assert!(rig.svc.download_pause(a));
    assert_eq!(rig.job(a).state, JobState::Paused);
    // The chunk already on its way still lands; then nothing moves.
    std::thread::sleep(Duration::from_millis(400));
    let at = rig.job(a).done_bytes;
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.job(a).done_bytes, at);
    assert!(rig.svc.download_resume(a));
    rig.until("moving again", |r| r.job(a).done_bytes > at);
    assert!(rig.svc.download_cancel(a));
    rig.until("cancelled", |r| r.job(a).state == JobState::Cancelled);
    // The waiting job takes its place; cancel it too.
    rig.until("b running", |r| r.job(b).state == JobState::Running);
    assert!(rig.svc.download_cancel(b));
    rig.until("b cancelled", |r| r.job(b).state == JobState::Cancelled);
    assert!(rig.files().is_empty());
    // Cancelled is not a failure.
    let fin = rig.svc.download_take_finished();
    assert_eq!((fin.done, fin.failed), (0, 0));
    assert!(rig.svc.download_forget(a));
    assert!(!rig.svc.download_forget(a));
    assert_eq!(rig.svc.download_list().len(), 1);
}

#[test]
fn service_drop_removes_parts() {
    let _g = serial();
    let site = serve(slow_file(150_000, 500_000));
    let rig = rig();
    let id = rig.start(&site.url("/a.mp4"));
    rig.until("part exists", |r| {
        r.job(id).done_bytes > 5_000 && r.files().iter().any(|f| f.ends_with(".part"))
    });
    let Rig { svc, dir, _state } = rig;
    drop(svc);
    assert!(names(dir.path()).is_empty());
}

#[test]
fn panic_switch_pauses_downloads() {
    let _g = serial();
    let site = serve(slow_file(150_000, 500_000));
    let rig = rig();
    let id = rig.start(&site.url("/a.mp4"));
    rig.until("moving", |r| r.job(id).done_bytes > 5_000);
    rig.svc.cancel_streams();
    assert_eq!(rig.job(id).state, JobState::Paused);
    std::thread::sleep(Duration::from_millis(400));
    let at = rig.job(id).done_bytes;
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.job(id).done_bytes, at);
    // Files stay where they are; nothing was deleted.
    assert!(rig.files().iter().any(|f| f.ends_with(".part")));
    rig.svc.download_cancel(id);
    rig.settle();
    assert!(rig.files().is_empty());
}

#[test]
fn require_proxy_no_proxy_every_save_path_zero_hits() {
    let _g = serial();
    let site = serve(table(vec![
        ("/v.mp4", ok("video/mp4", mp4(10_000, 1))),
        (
            "/page",
            html(r#"<meta property="og:video" content="/v.mp4">"#),
        ),
        (
            "/m.m3u8",
            ok("application/vnd.apple.mpegurl", hls_playlist(2, "")),
        ),
        ("/s.mpd", ok("application/dash+xml", "<MPD/>")),
    ]));
    let rig = rig_with(|c| c.require_proxy = true);
    for p in ["/v.mp4", "/page", "/m.m3u8", "/s.mpd"] {
        rig.start(&site.url(p));
    }
    let l = rig.settle();
    assert_eq!(l.len(), 4);
    for j in &l {
        assert_eq!(j.state, JobState::Failed);
        assert_eq!(j.error, Some(errors::PROXY_REQUIRED));
    }
    assert_eq!(site.hits.load(Ordering::SeqCst), 0);
    assert!(rig.files().is_empty());
    assert_eq!(classify("proxy required"), errors::PROXY_REQUIRED);
}
