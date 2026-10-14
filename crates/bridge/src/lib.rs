//! Line protocol between the app and an optional separate browser helper.
//!
//! One JSON message per line over stdin/stdout, at most [`MAX_LINE`] bytes
//! per line. No network code lives here: this crate only frames messages and
//! vets the one thing a helper may ask the app to do, play a URL.

use std::io::{self, BufRead, Write};
use std::net::{IpAddr, Ipv4Addr};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Hard cap on one line, newline excluded.
pub const MAX_LINE: usize = 8 * 1024;
/// Hard cap on a play URL.
pub const MAX_URL: usize = 4096;

/// App to helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHelper {
    Hello {
        v: u32,
        #[serde(default)]
        proxy: Option<String>,
        #[serde(default)]
        require_proxy: bool,
        #[serde(default)]
        js: bool,
        #[serde(default)]
        media: bool,
        #[serde(default)]
        remember: bool,
        #[serde(default)]
        discreet: bool,
        #[serde(default)]
        disguised: bool,
        #[serde(default)]
        rules_path: Option<String>,
        #[serde(default)]
        search_template: Option<String>,
        /// Software rendering: the helper turns the DMABUF renderer off.
        #[serde(default)]
        software: bool,
    },
    Close,
    Discreet {
        on: bool,
    },
}

/// Helper to app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromHelper {
    Ready {
        engine: String,
    },
    Play {
        url: String,
        #[serde(default)]
        title: Option<String>,
    },
    Boss,
    Closed,
    Error {
        code: String,
    },
}

/// Why a message could not be read. Only `Eof` and `Io` end a session; the
/// others leave the stream positioned at the next line.
#[derive(Debug)]
pub enum ReadError {
    Eof,
    TooLong,
    Malformed,
    Io(io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ReadError::Eof => "end of stream",
            ReadError::TooLong => "line too long",
            ReadError::Malformed => "malformed message",
            ReadError::Io(_) => "i/o error",
        })
    }
}

impl std::error::Error for ReadError {}

/// Read one line, holding at most `MAX_LINE` bytes; an oversized line is
/// consumed to its newline without being stored.
fn read_line_bounded<R: BufRead>(r: &mut R) -> Result<Vec<u8>, ReadError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut over = false;
    let mut any = false;
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ReadError::Io(e)),
        };
        if chunk.is_empty() {
            if !any {
                return Err(ReadError::Eof);
            }
            break;
        }
        any = true;
        let (take, found) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        if !over {
            if buf.len() + take > MAX_LINE {
                over = true;
                buf = Vec::new();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        r.consume(take + usize::from(found));
        if found {
            break;
        }
    }
    if over {
        Err(ReadError::TooLong)
    } else {
        Ok(buf)
    }
}

/// Read one message. Unknown variants and bad JSON are `Malformed`.
pub fn read_message<R: BufRead, T: DeserializeOwned>(r: &mut R) -> Result<T, ReadError> {
    let mut line = read_line_bounded(r)?;
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    serde_json::from_slice(&line).map_err(|_| ReadError::Malformed)
}

/// Write one message and a newline, refusing anything over `MAX_LINE`.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(msg)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message not serializable"))?;
    if bytes.len() > MAX_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "message too long",
        ));
    }
    bytes.push(b'\n');
    w.write_all(&bytes)?;
    w.flush()
}

// ---- URL vetting -----------------------------------------------------------

/// `inet_aton` forms: `127.1`, `2130706433`, `0x7f000001`, `0177.0.0.1`.
fn legacy_ipv4(h: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = h.split('.').collect();
    if parts.is_empty() || parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut nums = Vec::with_capacity(4);
    for p in &parts {
        let n = if let Some(x) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            if x.is_empty() {
                0
            } else {
                u64::from_str_radix(x, 16).ok()?
            }
        } else if p.len() > 1 && p.starts_with('0') {
            u64::from_str_radix(&p[1..], 8).ok()?
        } else {
            p.parse::<u64>().ok()?
        };
        nums.push(n);
    }
    let (last, init) = nums.split_last()?;
    if init.iter().any(|&n| n > 255) {
        return None;
    }
    let tail_bytes = 5 - nums.len() as u32;
    if *last >= 1u64 << (8 * tail_bytes) {
        return None;
    }
    let mut v = *last;
    for (i, n) in init.iter().enumerate() {
        v |= n << (8 * (3 - i as u32));
    }
    Some(Ipv4Addr::from(v as u32))
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] >= 240)
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            if a.is_loopback() || a.is_unspecified() || a.is_multicast() {
                return false;
            }
            if (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] == 0x2001 && s[1] == 0x0db8)
            {
                return false;
            }
            let embedded = |hi: u16, lo: u16| {
                Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
            };
            if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0) {
                return is_public_ip(IpAddr::V4(embedded(s[6], s[7])));
            }
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0; 4] {
                return is_public_ip(IpAddr::V4(embedded(s[6], s[7])));
            }
            if s[0] == 0x2002 {
                return is_public_ip(IpAddr::V4(embedded(s[1], s[2])));
            }
            true
        }
    }
}

const INTRANET_SUFFIXES: &[&str] = &[
    ".localhost",
    ".local",
    ".lan",
    ".home",
    ".internal",
    ".intranet",
    ".localdomain",
    ".home.arpa",
];

/// Loopback, private, link-local, single-label and intranet hosts, in any
/// spelling a URL parser accepts. `h` is lower-case ASCII without brackets.
/// When in doubt, local.
fn is_local_host(h: &str) -> bool {
    let h = h.trim_end_matches('.');
    if h.is_empty() || !h.is_ascii() || h.contains('%') {
        return true;
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        return !is_public_ip(ip);
    }
    if let Some(ip) = legacy_ipv4(h) {
        return !is_public_ip(IpAddr::V4(ip));
    }
    let last = h.rsplit('.').next().unwrap_or("");
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        return true;
    }
    h.contains(':') || !h.contains('.') || INTRANET_SUFFIXES.iter().any(|s| h.ends_with(s))
}

/// Vet a URL a helper asks the app to play. Returns the normalized URL
/// (lower-case scheme and host). Errors are fixed sentences, never an echo.
pub fn check_play_url(url: &str) -> Result<String, &'static str> {
    if url.len() > MAX_URL {
        return Err("the address is too long");
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the address has forbidden characters");
    }
    if url.contains('\\') {
        return Err("the address has forbidden characters");
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("only http and https addresses are allowed")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("only http and https addresses are allowed");
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    if auth.contains('@') {
        return Err("the address must not carry credentials");
    }
    let (host, port) = if let Some(v6) = auth.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or("the address has no valid host")?;
        match after {
            "" => (format!("[{h}]"), None),
            _ => match after.strip_prefix(':') {
                Some(p) => (format!("[{h}]"), Some(p)),
                None => return Err("the address has no valid host"),
            },
        }
        .pipe_check(h)?
    } else {
        match auth.split_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), Some(p)),
            None => (auth.to_ascii_lowercase(), None),
        }
        .pipe_check(&auth.split(':').next().unwrap_or("").to_ascii_lowercase())?
    };
    if let Some(p) = port {
        let ok = !p.is_empty()
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| n > 0 && n <= 65535);
        if !ok {
            return Err("the address has an invalid port");
        }
    }
    let mut out = format!("{scheme}://{host}");
    if let Some(p) = port {
        out.push(':');
        out.push_str(p);
    }
    out.push_str(tail);
    Ok(out)
}

/// Small helper so the two host branches share the locality check.
trait PipeCheck<'a>: Sized {
    fn pipe_check(self, bare: &str) -> Result<(String, Option<&'a str>), &'static str>;
}

impl<'a> PipeCheck<'a> for (String, Option<&'a str>) {
    fn pipe_check(self, bare: &str) -> Result<(String, Option<&'a str>), &'static str> {
        if bare.is_empty() || !bare.is_ascii() {
            return Err("the address has no valid host");
        }
        if is_local_host(&bare.to_ascii_lowercase()) {
            return Err("local and private hosts are not allowed");
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn hello() -> ToHelper {
        ToHelper::Hello {
            v: 1,
            proxy: Some("socks5://127.0.0.1:9050".into()),
            require_proxy: true,
            js: false,
            media: true,
            remember: false,
            discreet: true,
            disguised: false,
            rules_path: Some("/tmp/r.json".into()),
            search_template: None,
            software: false,
        }
    }

    #[test]
    fn round_trips() {
        let mut buf = Vec::new();
        for m in [hello(), ToHelper::Close, ToHelper::Discreet { on: true }] {
            write_message(&mut buf, &m).unwrap();
        }
        let mut r = Cursor::new(buf);
        assert_eq!(read_message::<_, ToHelper>(&mut r).unwrap(), hello());
        assert_eq!(
            read_message::<_, ToHelper>(&mut r).unwrap(),
            ToHelper::Close
        );
        assert_eq!(
            read_message::<_, ToHelper>(&mut r).unwrap(),
            ToHelper::Discreet { on: true }
        );
        assert!(matches!(
            read_message::<_, ToHelper>(&mut r),
            Err(ReadError::Eof)
        ));

        let mut buf = Vec::new();
        let msgs = vec![
            FromHelper::Ready { engine: "x".into() },
            FromHelper::Play {
                url: "https://a.example/v".into(),
                title: None,
            },
            FromHelper::Boss,
            FromHelper::Closed,
            FromHelper::Error { code: "e".into() },
        ];
        for m in &msgs {
            write_message(&mut buf, m).unwrap();
        }
        let mut r = Cursor::new(buf);
        for m in &msgs {
            assert_eq!(&read_message::<_, FromHelper>(&mut r).unwrap(), m);
        }
    }

    #[test]
    fn oversized_line_refused_and_stream_stays_in_sync() {
        let mut data = vec![b'a'; MAX_LINE * 5 + 17];
        data.push(b'\n');
        data.extend_from_slice(b"{\"t\":\"boss\"}\n");
        // BufReader with a tiny buffer exercises the multi-chunk path.
        let mut r = io::BufReader::with_capacity(64, Cursor::new(data));
        assert!(matches!(
            read_message::<_, FromHelper>(&mut r),
            Err(ReadError::TooLong)
        ));
        assert_eq!(
            read_message::<_, FromHelper>(&mut r).unwrap(),
            FromHelper::Boss
        );
    }

    #[test]
    fn line_at_the_cap_is_read_and_write_refuses_more() {
        let pad = MAX_LINE - "{\"t\":\"error\",\"code\":\"\"}".len();
        let line = format!("{{\"t\":\"error\",\"code\":\"{}\"}}\n", "x".repeat(pad));
        assert_eq!(line.len() - 1, MAX_LINE);
        let mut r = Cursor::new(line.into_bytes());
        assert!(read_message::<_, FromHelper>(&mut r).is_ok());
        let big = FromHelper::Error {
            code: "x".repeat(MAX_LINE),
        };
        assert!(write_message(&mut Vec::new(), &big).is_err());
    }

    #[test]
    fn malformed_and_unknown_are_dropped_then_sync() {
        let data = b"not json\n{\"t\":\"nope\"}\n\n{\"t\":\"closed\"}\n{\"t\":\"boss\"}";
        let mut r = Cursor::new(&data[..]);
        for _ in 0..3 {
            assert!(matches!(
                read_message::<_, FromHelper>(&mut r),
                Err(ReadError::Malformed)
            ));
        }
        assert_eq!(
            read_message::<_, FromHelper>(&mut r).unwrap(),
            FromHelper::Closed
        );
        // Unterminated final line is still parsed.
        assert_eq!(
            read_message::<_, FromHelper>(&mut r).unwrap(),
            FromHelper::Boss
        );
    }

    #[test]
    fn refused_urls() {
        for u in [
            "file:///etc/passwd",
            "data:text/html,hi",
            "javascript:alert(1)",
            "https://user:password@example.com/",
            "https://user@example.com/",
            "http://localhost/",
            "http://localhost:8080/x",
            "http://127.0.0.1/",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0177.0.0.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/",
            "http://nas/",
            "http://box.lan/",
            "http://a\\@b/",
            "http://127.0.0.1\\@example.com/",
            "http://example.com/a\nb",
            "http://example.com/a b",
            "http://example.com:99999/",
            "http://example.com:/",
            "http://",
            "example.com",
            "ftp://example.com/",
        ] {
            assert!(check_play_url(u).is_err(), "accepted {u:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL));
        assert!(check_play_url(&long).is_err());
    }

    #[test]
    fn errors_do_not_echo_input() {
        let e = check_play_url("http://secret-marker-host/").unwrap_err();
        assert!(!e.contains("secret-marker"));
    }

    #[test]
    fn valid_urls_are_normalized() {
        assert_eq!(
            check_play_url("HTTPS://Example.COM/Path?q=1#f").unwrap(),
            "https://example.com/Path?q=1#f"
        );
        assert_eq!(
            check_play_url("http://example.com:8080").unwrap(),
            "http://example.com:8080"
        );
        assert_eq!(
            check_play_url("https://[2606:4700::1]:443/v").unwrap(),
            "https://[2606:4700::1]:443/v"
        );
        assert!(check_play_url("https://8.8.8.8/").is_ok());
    }

    #[test]
    fn garbage_never_panics() {
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for round in 0..200 {
            let n = (next() % 40_000) as usize;
            let mut bytes: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            if round % 3 == 0 {
                // Bias towards structure: braces, quotes, newlines.
                for b in bytes.iter_mut().step_by(7) {
                    *b = b"{}\"\n:,"[(next() % 6) as usize];
                }
            }
            let mut r = io::BufReader::with_capacity(128, Cursor::new(bytes.clone()));
            let mut guard = 0;
            loop {
                guard += 1;
                assert!(guard < 100_000);
                match read_message::<_, FromHelper>(&mut r) {
                    Err(ReadError::Eof) | Err(ReadError::Io(_)) => break,
                    _ => {}
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            let _ = check_play_url(&text);
            let _ = check_play_url(&format!("http://{text}"));
        }
    }
}
