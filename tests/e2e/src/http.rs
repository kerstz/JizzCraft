//! A loopback HTTP/1.1 server driven by a closure. One connection, one
//! response, `Connection: close`. Synthetic bytes only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A request, as the handler sees it.
pub struct Req {
    pub path: String,
    /// `bytes=a-b`: start and optional inclusive end.
    pub range: Option<(u64, Option<u64>)>,
    /// Arrival order across the whole server, from 0.
    pub index: usize,
    /// How many times this path was asked before, from 0.
    pub nth: usize,
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub ctype: String,
    pub body: Arc<Vec<u8>>,
    /// Honour `Range` (206). False: always the whole body with 200.
    pub ranges: bool,
    /// Bytes per second for the body; 8000 is 64 kbit/s.
    pub bps: Option<u64>,
    /// Send half of the body, then close the connection.
    pub cut: bool,
    /// Wait for this flag before answering (a server that sits on a request).
    pub hold: Option<Arc<AtomicBool>>,
    /// Wait this long before answering.
    pub delay: Duration,
}

impl Reply {
    pub fn ok(ctype: &str, body: impl Into<Vec<u8>>) -> Reply {
        Reply::shared(ctype, Arc::new(body.into()))
    }
    pub fn shared(ctype: &str, body: Arc<Vec<u8>>) -> Reply {
        Reply {
            status: 200,
            ctype: ctype.into(),
            body,
            ranges: true,
            bps: None,
            cut: false,
            hold: None,
            delay: Duration::ZERO,
        }
    }
    pub fn status(code: u16) -> Reply {
        Reply {
            status: code,
            ..Reply::ok("text/plain", Vec::new())
        }
    }
    pub fn bps(mut self, bps: u64) -> Reply {
        self.bps = Some(bps);
        self
    }
    pub fn cut(mut self) -> Reply {
        self.cut = true;
        self
    }
    pub fn hold(mut self, flag: Arc<AtomicBool>) -> Reply {
        self.hold = Some(flag);
        self
    }
    pub fn delay(mut self, d: Duration) -> Reply {
        self.delay = d;
        self
    }
}

pub type Handler = Arc<dyn Fn(&Req) -> Reply + Send + Sync>;

pub struct Server {
    pub port: u16,
    /// Connections accepted: a socket that reached this server.
    pub accepted: Arc<AtomicUsize>,
    /// Requests that carried a `Range` header.
    pub ranged: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
}

impl Server {
    pub fn start(handler: Handler) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        let s = Server {
            port,
            accepted: Arc::default(),
            ranged: Arc::default(),
            log: Arc::default(),
            stop: Arc::default(),
            active: Arc::default(),
        };
        let (accepted, ranged, log, stop, active) = (
            s.accepted.clone(),
            s.ranged.clone(),
            s.log.clone(),
            s.stop.clone(),
            s.active.clone(),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((c, _)) => {
                        let index = accepted.fetch_add(1, Ordering::SeqCst);
                        let _ = c.set_nonblocking(false);
                        let (h, ranged, log, stop, active) = (
                            handler.clone(),
                            ranged.clone(),
                            log.clone(),
                            stop.clone(),
                            active.clone(),
                        );
                        active.fetch_add(1, Ordering::SeqCst);
                        std::thread::spawn(move || {
                            let _ = serve(c, &*h, index, &ranged, &log, &stop);
                            active.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        s
    }

    /// Exact paths, each with its own body and content type.
    pub fn files(routes: Vec<(&str, &str, Arc<Vec<u8>>)>) -> Server {
        let routes: Vec<(String, String, Arc<Vec<u8>>)> = routes
            .into_iter()
            .map(|(p, t, b)| (p.to_owned(), t.to_owned(), b))
            .collect();
        Server::start(Arc::new(move |r: &Req| {
            match routes.iter().find(|(p, _, _)| *p == r.path) {
                Some((_, t, b)) => Reply::shared(t, b.clone()),
                None => Reply::status(404),
            }
        }))
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// How many times `path` was asked for (query ignored).
    pub fn hits_for(&self, path: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.as_str() == path)
            .count()
    }

    pub fn paths(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Wait until no response is being written (best effort, 3 s).
    pub fn quiesce(&self) {
        let end = Instant::now() + Duration::from_secs(3);
        while self.active.load(Ordering::SeqCst) > 0 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn serve(
    mut s: TcpStream,
    handler: &(dyn Fn(&Req) -> Reply + Send + Sync),
    index: usize,
    ranged: &AtomicUsize,
    log: &Mutex<Vec<String>>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let full = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let path = full.split('?').next().unwrap_or("/").to_owned();
    let range = text.lines().find_map(|l| {
        let l = l.to_ascii_lowercase();
        let r = l.strip_prefix("range: bytes=")?.to_owned();
        let (a, b) = r.split_once('-')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()))
    });
    if range.is_some() {
        ranged.fetch_add(1, Ordering::SeqCst);
    }
    if std::env::var_os("E2E_DEBUG").is_some() {
        eprintln!("[http] #{index} {path} {range:?}");
    }
    let nth = {
        let mut g = log.lock().unwrap();
        let n = g.iter().filter(|p| **p == path).count();
        g.push(path.clone());
        n
    };
    let r = handler(&Req {
        path,
        range,
        index,
        nth,
    });
    // A held request is released by the flag, or given up after 30 s.
    if let Some(flag) = &r.hold {
        let end = Instant::now() + Duration::from_secs(30);
        while !flag.load(Ordering::SeqCst) && Instant::now() < end && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if !r.delay.is_zero() {
        std::thread::sleep(r.delay);
    }
    let total = r.body.len() as u64;
    if r.status != 200 {
        write!(
            s,
            "HTTP/1.1 {} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            r.status
        )?;
        return Ok(());
    }
    let body: &[u8] = &r.body;
    let (slice, head) = match range {
        Some((a, b)) if r.ranges => {
            if a >= total {
                write!(
                    s,
                    "HTTP/1.1 416 X\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )?;
                return Ok(());
            }
            let b = b.unwrap_or(total - 1).min(total - 1);
            (
                &body[a as usize..=b as usize],
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: {}\r\nETag: \"v1\"\r\nContent-Range: bytes {a}-{b}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    r.ctype,
                    b - a + 1
                ),
            )
        }
        _ => (
            body,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nETag: \"v1\"\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
                r.ctype
            ),
        ),
    };
    s.write_all(head.as_bytes())?;
    let slice = if r.cut {
        &slice[..slice.len() / 2]
    } else {
        slice
    };
    match r.bps {
        None => s.write_all(slice),
        Some(bps) => {
            // 100 ms slices.
            for c in slice.chunks((bps / 10).max(1) as usize) {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                s.write_all(c)?;
                s.flush()?;
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        }
    }
}
