//! A loopback HTTP/1.1 server that misbehaves on purpose. Synthetic bytes
//! only. Requests are numbered from 0 in arrival order.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Clone)]
pub struct Cfg {
    pub body: Vec<u8>,
    /// Honour `Range` (206). False: always 200 with the whole body.
    pub ranges: bool,
    /// Bytes per second; 8000 is 64 kbit/s. `None`: full speed.
    pub throttle_bps: Option<u64>,
    /// Requests `[from, to)` get half a body and a closed connection.
    pub drop_idx: Option<(usize, usize)>,
    /// Requests `[from, to)` get `503` with `Retry-After: 1`.
    pub busy_idx: Option<(usize, usize)>,
    /// From this request on the ETag header changes (still 206).
    pub flip_etag_from: Option<usize>,
    /// Paths starting with this prefix get 403 from this request on.
    pub forbid: Option<(String, usize)>,
    /// Every request is answered with a redirect to this address.
    pub redirect_to: Option<String>,
    /// Exact paths served with their own body, type and `Range` behaviour.
    pub routes: Vec<Route>,
}

#[derive(Clone)]
pub struct Route {
    pub path: String,
    pub body: Vec<u8>,
    pub ctype: String,
    pub ranges: bool,
}

impl Route {
    pub fn new(path: &str, ctype: &str, body: Vec<u8>, ranges: bool) -> Self {
        Self {
            path: path.into(),
            body,
            ctype: ctype.into(),
            ranges,
        }
    }
}

impl Cfg {
    pub fn new(body: Vec<u8>) -> Self {
        Self {
            body,
            ranges: true,
            throttle_bps: None,
            drop_idx: None,
            busy_idx: None,
            flip_etag_from: None,
            forbid: None,
            redirect_to: None,
            routes: Vec::new(),
        }
    }
}

pub struct Server {
    pub port: u16,
    /// Requests received.
    pub hits: Arc<AtomicUsize>,
    /// Responses being written right now.
    pub active: Arc<AtomicUsize>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

pub fn start(cfg: Cfg) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let (h2, a2) = (hits.clone(), active.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            let n = h2.fetch_add(1, Ordering::SeqCst);
            let (cfg, a) = (cfg.clone(), a2.clone());
            std::thread::spawn(move || {
                a.fetch_add(1, Ordering::SeqCst);
                let _ = handle(s, &cfg, n);
                a.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Server { port, hits, active }
}

fn in_range(r: Option<(usize, usize)>, n: usize) -> bool {
    r.is_some_and(|(a, b)| n >= a && n < b)
}

fn send(s: &mut TcpStream, data: &[u8], bps: Option<u64>) -> std::io::Result<()> {
    match bps {
        None => s.write_all(data),
        Some(bps) => {
            // 100 ms slices.
            let slice = (bps / 10).max(1) as usize;
            for c in data.chunks(slice) {
                s.write_all(c)?;
                s.flush()?;
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        }
    }
}

fn handle(mut s: TcpStream, cfg: &Cfg, n: usize) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut buf = [0u8; 2048];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let k = s.read(&mut buf)?;
        if k == 0 {
            return Ok(());
        }
        req.extend_from_slice(&buf[..k]);
    }
    let text = String::from_utf8_lossy(&req).to_string();
    let path = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let range = text.lines().find_map(|l| {
        l.to_ascii_lowercase()
            .strip_prefix("range: bytes=")
            .map(str::to_owned)
    });
    if let Some(to) = &cfg.redirect_to {
        write!(
            s,
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    if let Some((prefix, from)) = &cfg.forbid
        && path.starts_with(prefix.as_str())
        && n >= *from
    {
        write!(
            s,
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    if in_range(cfg.busy_idx, n) {
        write!(
            s,
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    let etag = if cfg.flip_etag_from.is_some_and(|f| n >= f) {
        "\"v2\""
    } else {
        "\"v1\""
    };
    let cut = in_range(cfg.drop_idx, n);
    let bare = path.split('?').next().unwrap_or("");
    let route = cfg.routes.iter().find(|r| r.path == bare);
    let (body, ranges, ctype) = match route {
        Some(r) => (&r.body, r.ranges, r.ctype.as_str()),
        None => (&cfg.body, cfg.ranges, "video/mp4"),
    };
    let total = body.len();
    match (range, ranges) {
        (Some(r), true) => {
            let (a, b) = r.split_once('-').unwrap_or(("0", "0"));
            let a: usize = a.trim().parse().unwrap_or(0);
            let b: usize = b
                .trim()
                .parse::<usize>()
                .unwrap_or(total - 1)
                .min(total - 1);
            let part = &body[a..=b];
            write!(
                s,
                "HTTP/1.1 206 Partial Content\r\nContent-Type: {ctype}\r\nETag: {etag}\r\nContent-Range: bytes {a}-{b}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                part.len()
            )?;
            let part = if cut { &part[..part.len() / 2] } else { part };
            send(&mut s, part, cfg.throttle_bps)
        }
        _ => {
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nETag: {etag}\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
            )?;
            let body = if cut { &body[..total / 2] } else { &body[..] };
            send(&mut s, body, cfg.throttle_bps)
        }
    }
}

/// `ftyp`, then `mdat`, then `moov` last: the index is at the end.
pub fn mp4_moov_last(mdat_len: usize) -> Vec<u8> {
    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }
    let mut out = atom(b"ftyp", b"isom\0\0\x02\0isomiso2");
    out.extend(atom(b"mdat", &vec![0x5Au8; mdat_len]));
    out.extend(atom(b"moov", &[0u8; 64]));
    out
}

pub fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}
