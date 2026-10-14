//! Hidden self-test (engine build only). Honoured only with
//! `JIZZCRAFT_BROWSER_SELFTEST=1` and `--self-test`; no Hello, no app.
//!
//! It serves a test page from two loopback HTTP servers owned by this process
//! and observes, through WebKit signals and JS results, that every policy
//! holds. Loopback is reachable here ONLY because this mode is a test: the
//! helper's own navigation filter never looked at the host (only the scheme),
//! so there is no helper rule to relax; the "no local or private host" rule is
//! the app-side `check_play_url`, which this mode does not touch.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// What Rust-side signal handlers saw. Shared with the engine wiring.
#[derive(Default)]
pub struct Obs {
    pub windows_requested: Cell<u32>,
    pub permission_requests: Cell<u32>,
    pub notifications_shown: Cell<u32>,
    pub downloads: Cell<u32>,
    pub boss: Cell<bool>,
    pub refused_navigations: RefCell<Vec<String>>,
}

pub type Hits = Arc<Mutex<Vec<String>>>;

pub struct Servers {
    pub main: SocketAddr,
    pub hits: Hits,
}

fn wav() -> Vec<u8> {
    let n: u32 = 8000;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + n).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&n.to_le_bytes());
    v.extend(std::iter::repeat_n(128u8, n as usize));
    v
}

fn page(other: u16) -> String {
    format!(
        r#"<!doctype html><meta charset=utf-8><title>Secret page title</title><body>
<script>
var R = {{}};
document.title = 'Secret page title';
try {{ var w = window.open('/popup', '_blank'); R.popup_null = (w === null || w === undefined); }} catch (e) {{ R.popup_null = true; }}
var l = document.createElement('a'); l.href = '/popup2'; l.target = '_blank'; document.body.appendChild(l); try {{ l.click(); }} catch (e) {{}}
var d = document.createElement('a'); d.href = '/file.bin'; d.download = 'file.bin'; document.body.appendChild(d); try {{ d.click(); }} catch (e) {{}}
try {{ navigator.geolocation.getCurrentPosition(function () {{ R.geo = 'granted'; }}, function (e) {{ R.geo = 'denied:' + e.code; }}); }} catch (e) {{ R.geo = 'absent'; }}
try {{ if (typeof Notification === 'undefined') R.notif = 'absent'; else {{ var p = Notification.requestPermission(function (x) {{ R.notif = x; }}); if (p && p.then) p.then(function (x) {{ R.notif = x; }}); }} }} catch (e) {{ R.notif = 'absent'; }}
try {{ if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) R.cam = 'absent'; else navigator.mediaDevices.getUserMedia({{video: true, audio: true}}).then(function () {{ R.cam = 'granted'; }}, function (e) {{ R.cam = 'denied:' + e.name; }}); }} catch (e) {{ R.cam = 'absent'; }}
R.webrtc = (typeof RTCPeerConnection === 'undefined' && typeof webkitRTCPeerConnection === 'undefined') ? 'absent' : 'present';
R.eme = (typeof navigator.requestMediaKeySystemAccess === 'undefined') ? 'absent' : 'present';
try {{ var a = new Audio('/a.wav'); a.autoplay = true; var pr = a.play(); if (pr && pr.then) pr.then(function () {{ R.autoplay = 'played'; }}, function (e) {{ R.autoplay = 'denied:' + e.name; }}); else R.autoplay = 'unknown'; }} catch (e) {{ R.autoplay = 'denied:' + e.name; }}
fetch('http://127.0.0.1:{other}/x').then(function () {{ R.cross_fetch = 'readable'; }}, function () {{ R.cross_fetch = 'blocked'; }});
setTimeout(function () {{ location.href = 'file:///etc/hostname'; }}, 300);
setTimeout(function () {{ R.final_scheme = location.protocol; R.done = true; }}, 3500);
</script>"#
    )
}

fn handle(mut s: TcpStream, hits: &Hits, other: u16, is_main: bool) {
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut buf = [0u8; 4096];
    let n = s.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
        .to_owned();
    if let Ok(mut h) = hits.lock() {
        h.push(format!("{}{path}", if is_main { "main" } else { "other" }));
    }
    let (ctype, extra, body): (&str, &str, Vec<u8>) = match (is_main, path.as_str()) {
        (true, "/") => ("text/html", "", page(other).into_bytes()),
        (true, "/a.wav") => ("audio/wav", "", wav()),
        (true, "/file.bin") => (
            "application/octet-stream",
            "Content-Disposition: attachment; filename=file.bin\r\n",
            b"payload".to_vec(),
        ),
        (true, "/popup" | "/popup2") => ("text/html", "", b"<p>popup</p>".to_vec()),
        // No CORS headers on purpose: the page must not be able to read it.
        _ => ("text/plain", "", b"other".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(&body);
}

pub fn start() -> std::io::Result<Servers> {
    let hits: Hits = Arc::default();
    let a = TcpListener::bind("127.0.0.1:0")?;
    let b = TcpListener::bind("127.0.0.1:0")?;
    let (aa, bb) = (a.local_addr()?, b.local_addr()?);
    for (l, is_main) in [(a, true), (b, false)] {
        let hits = hits.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let hits = hits.clone();
                std::thread::spawn(move || handle(c, &hits, bb.port(), is_main));
            }
        });
    }
    Ok(Servers { main: aa, hits })
}

pub fn count(hits: &Hits, what: &str) -> usize {
    hits.lock()
        .map(|h| h.iter().filter(|p| p.as_str() == what).count())
        .unwrap_or(0)
}
