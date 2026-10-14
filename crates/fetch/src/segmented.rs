//! Segmented VOD streams (HLS and DASH) as a [`TimedSource`].
//!
//! The reader downloads segments ahead of the consumer (at most 60 s, 64 MiB
//! and four requests at once), decrypts AES-128 in memory, and hands out
//! container bytes in presentation order. It never touches the disk and never
//! gives a URL to anyone: the consumer only sees bytes.
//!
//! Rules every request obeys: http(s) only; a public manifest may not point
//! at a loopback/private target (`sources::inward_ok`); the proxy kill switch
//! is checked before each request; only User-Agent, Referer and Origin are
//! sent on top of the defaults, never a source key.
//!
//! Limits for now: muxed representations only (a stream whose audio is a
//! separate rendition is refused as unsupported), VOD only, no live.
//!
//! Chunk contract: the first chunk after a seek or a discontinuity carries
//! `discont: true`; when an init segment goes first it is that chunk (pts
//! `None`) and the media chunk that follows has `discont: false`.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use jizzcraft_core::manifest::looks_like_manifest;
use jizzcraft_core::timed::{Chunk, TimedSource, VariantInfo};
use reqwest::Url;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::pick::{Quality, pick_variant};
use crate::sources::inward_ok;
use crate::{dash, hls};

/// Prefetch window ahead of the playback position.
const WINDOW: Duration = Duration::from_secs(60);
const MAX_BUFFER: usize = 64 << 20;
const MAX_CONCURRENT: usize = 4;
const MAX_SEGMENT: usize = 64 << 20;
const MAX_KEY_READ: usize = 64;
/// Servers that ignore `Range` get the first N bytes read and no more.
const MAX_RANGE_SKIP: u64 = 256 << 20;
// Five tries; after that the server and we agree to disagree.
const ATTEMPTS: u32 = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// Inclusive byte range.
type Range = (u64, u64);

#[derive(Clone, PartialEq, Eq)]
struct Part {
    url: String,
    range: Option<Range>,
}

#[derive(Clone)]
struct KeyDesc {
    url: String,
    iv: Option<[u8; 16]>,
}

#[derive(Clone)]
struct Seg {
    part: Part,
    dur: Duration,
    start: Duration,
    init: Option<Part>,
    key: Option<KeyDesc>,
    seq: u64,
    discont: bool,
}

struct Track {
    segs: Vec<Seg>,
    total: Duration,
}

impl Track {
    fn new(mut segs: Vec<Seg>) -> Self {
        let mut t = Duration::ZERO;
        for s in &mut segs {
            s.start = t;
            t += s.dur;
        }
        Track { segs, total: t }
    }

    /// Index of the segment containing `t` (the last one if past the end).
    fn index_at(&self, t: Duration) -> usize {
        self.segs
            .partition_point(|s| s.start <= t)
            .saturating_sub(1)
    }
}

enum VariantSrc {
    Playlist(String),
    Dash(Box<dash::Representation>),
}

struct Ready {
    bytes: Arc<Vec<u8>>,
    init: Option<Arc<Vec<u8>>>,
}

struct State {
    track: Arc<Track>,
    epoch: u64,
    next: usize,
    ready: HashMap<usize, Ready>,
    inflight: HashMap<usize, AbortHandle>,
    failed: HashMap<usize, String>,
    outbox: VecDeque<Chunk>,
    keys: HashMap<String, Arc<[u8; 16]>>,
    last_init: Option<Part>,
    need_discont: bool,
    est_bps: Option<f64>,
    avg_bytes: usize,
    slow_streak: u32,
    auto: bool,
    variant: usize,
    switching: Option<usize>,
    switched: Option<(usize, Track)>,
    switch_task: Option<AbortHandle>,
}

struct Shared {
    client: reqwest::Client,
    /// The address the user gave; every other address is judged against it.
    origin: String,
    headers: Vec<(String, String)>,
    variants: Vec<VariantInfo>,
    srcs: Vec<VariantSrc>,
    closed: AtomicBool,
    state: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct SegmentedReader {
    handle: Handle,
    sh: Arc<Shared>,
}

// ---- requests ---------------------------------------------------------------

fn allowed_headers(h: Vec<(String, String)>) -> Vec<(String, String)> {
    h.into_iter()
        .filter(|(k, v)| {
            ["user-agent", "referer", "origin"].contains(&k.to_ascii_lowercase().as_str())
                && v.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
        })
        .collect()
}

fn resolve(base: &str, rel: &str, origin: &str) -> Result<String, String> {
    let u = Url::parse(base)
        .and_then(|b| b.join(rel))
        .map_err(|_| "no media".to_owned())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("unsupported stream".into());
    }
    if !inward_ok(origin, u.as_str()) {
        return Err("unsupported stream".into());
    }
    Ok(u.into())
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let s = resp.headers().get("retry-after")?.to_str().ok()?;
    s.trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n.min(30)))
}

enum BodyErr {
    Final(String),
    Retry(String),
}

/// Read at most `cap` bytes, after discarding `skip` leading bytes.
async fn read_body(
    mut resp: reqwest::Response,
    cap: usize,
    skip: u64,
    take: Option<u64>,
) -> Result<Vec<u8>, BodyErr> {
    if take.is_none() && resp.content_length().is_some_and(|l| l > cap as u64) {
        return Err(BodyErr::Final("too large".into()));
    }
    let mut out = Vec::new();
    let mut seen = 0u64;
    loop {
        let c = match tokio::time::timeout(REQUEST_TIMEOUT, resp.chunk()).await {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => break,
            _ => return Err(BodyErr::Retry("stalled".into())),
        };
        let mut piece: &[u8] = &c;
        if seen < skip {
            let drop_n = ((skip - seen) as usize).min(piece.len());
            piece = &piece[drop_n..];
        }
        seen += c.len() as u64;
        if let Some(t) = take {
            let room = (t as usize).saturating_sub(out.len());
            piece = &piece[..piece.len().min(room)];
        }
        if out.len() + piece.len() > cap {
            return Err(BodyErr::Final("too large".into()));
        }
        out.extend_from_slice(piece);
        if take.is_some_and(|t| out.len() as u64 >= t) {
            break;
        }
    }
    Ok(out)
}

/// One GET with retries. Errors are fixed strings (`http 404`, `stalled`...).
async fn get_bytes(
    sh: &Shared,
    url: &str,
    range: Option<Range>,
    cap: usize,
) -> Result<Vec<u8>, String> {
    let mut last = String::from("stalled");
    for attempt in 0..ATTEMPTS {
        if sh.closed.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        // The kill switch applies to a stream that is already open.
        if crate::service::proxy_blocked() {
            return Err("proxy required".into());
        }
        if !inward_ok(&sh.origin, url) {
            return Err("unsupported stream".into());
        }
        let mut req = sh.client.get(url).timeout(REQUEST_TIMEOUT);
        for (k, v) in &sh.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some((a, b)) = range {
            req = req.header("Range", format!("bytes={a}-{b}"));
        }
        let mut wait = Duration::from_millis(250 << attempt);
        match req.send().await {
            Ok(resp) => {
                let code = resp.status().as_u16();
                match code {
                    200 | 206 => {
                        let (skip, take) = match (range, code) {
                            // The server ignored the range: cut it ourselves.
                            (Some((a, b)), 200) => {
                                if b >= MAX_RANGE_SKIP.max(MAX_SEGMENT as u64) + MAX_SEGMENT as u64
                                {
                                    return Err("too large".into());
                                }
                                (a, Some(b - a + 1))
                            }
                            _ => (0, None),
                        };
                        match read_body(resp, cap, skip, take).await {
                            Ok(b) => return Ok(b),
                            Err(BodyErr::Final(e)) => return Err(e),
                            Err(BodyErr::Retry(e)) => last = e,
                        }
                    }
                    // Final on VOD: the file is not there.
                    404 | 410 => return Err(format!("http {code}")),
                    429 | 503 => {
                        if let Some(d) = retry_after(&resp) {
                            wait = d;
                        }
                        last = format!("http {code}");
                    }
                    408 | 500..=599 => last = format!("http {code}"),
                    c => return Err(format!("http {c}")),
                }
            }
            Err(e) => {
                last = if e.is_timeout() {
                    "stalled".into()
                } else {
                    "error sending request".into()
                };
            }
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(wait).await;
        }
    }
    Err(last)
}

fn decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    if !data.len().is_multiple_of(16) {
        return Err("unsupported stream".into());
    }
    cbc::Decryptor::<Aes128>::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| "unsupported stream".to_owned())
}

async fn get_key(sh: &Shared, url: &str) -> Result<Arc<[u8; 16]>, String> {
    if let Some(k) = sh.lock().keys.get(url) {
        return Ok(Arc::clone(k));
    }
    let raw = get_bytes(sh, url, None, MAX_KEY_READ).await?;
    let key: [u8; 16] = raw
        .as_slice()
        .try_into()
        .map_err(|_| "unsupported stream".to_owned())?;
    let key = Arc::new(key);
    sh.lock().keys.insert(url.to_owned(), Arc::clone(&key));
    Ok(key)
}

fn head_ok(b: &[u8]) -> Result<(), String> {
    // A playlist where media should be is somebody else's address book.
    if looks_like_manifest(&b[..b.len().min(1024)]) {
        return Err("no media".into());
    }
    Ok(())
}

async fn download(sh: &Shared, seg: &Seg) -> Result<(Ready, usize, Duration), String> {
    let t0 = Instant::now();
    let mut body = get_bytes(sh, &seg.part.url, seg.part.range, MAX_SEGMENT).await?;
    let net = t0.elapsed();
    let wire = body.len();
    if let Some(k) = &seg.key {
        let key = get_key(sh, &k.url).await?;
        let iv = k.iv.unwrap_or_else(|| (u128::from(seg.seq)).to_be_bytes());
        body = decrypt(&key, &iv, &body)?;
    }
    head_ok(&body)?;
    let init = match &seg.init {
        Some(p) => {
            let b = get_bytes(sh, &p.url, p.range, MAX_SEGMENT).await?;
            head_ok(&b)?;
            Some(Arc::new(b))
        }
        None => None,
    };
    Ok((
        Ready {
            bytes: Arc::new(body),
            init,
        },
        wire,
        net,
    ))
}

async fn fetch_task(sh: Arc<Shared>, epoch: u64, idx: usize, seg: Seg) {
    let res = download(&sh, &seg).await;
    let mut st = sh.lock();
    if st.epoch != epoch {
        return;
    }
    st.inflight.remove(&idx);
    match res {
        Ok((r, wire, net)) => {
            let secs = net.as_secs_f64().max(1e-3);
            let sample = wire as f64 * 8.0 / secs;
            st.est_bps = Some(st.est_bps.map_or(sample, |e| e * 0.7 + sample * 0.3));
            st.avg_bytes = if st.avg_bytes == 0 {
                wire
            } else {
                (st.avg_bytes * 7 + wire * 3) / 10
            };
            if net > seg.dur {
                st.slow_streak += 1;
            } else {
                st.slow_streak = 0;
            }
            st.ready.insert(idx, r);
        }
        Err(e) => {
            st.failed.insert(idx, e);
        }
    }
    drop(st);
    sh.cv.notify_all();
}

// ---- tracks -----------------------------------------------------------------

fn hls_track(base: &str, origin: &str, segments: &[hls::Segment]) -> Result<Track, String> {
    let part_of = |uri: &str, r: Option<hls::ByteRange>| -> Result<Part, String> {
        Ok(Part {
            url: resolve(base, uri, origin)?,
            range: r.map(|r| (r.offset, r.offset + r.len - 1)),
        })
    };
    let mut segs = Vec::with_capacity(segments.len());
    for s in segments {
        let init = match &s.map {
            Some(m) => Some(part_of(&m.uri, m.range)?),
            None => None,
        };
        let key = match &s.key {
            hls::KeyRef::None => None,
            hls::KeyRef::Aes128 { uri, iv } => Some(KeyDesc {
                url: resolve(base, uri, origin)?,
                iv: *iv,
            }),
        };
        let dur = if s.duration.is_finite() && s.duration > 0.0 {
            Duration::from_secs_f64(s.duration)
        } else {
            Duration::ZERO
        };
        segs.push(Seg {
            part: part_of(&s.uri, s.range)?,
            dur,
            start: Duration::ZERO,
            init,
            key,
            seq: s.seq,
            discont: s.discontinuity,
        });
    }
    Ok(Track::new(segs))
}

fn hls_media(base: &str, origin: &str, text: &str) -> Result<Track, String> {
    match hls::parse(text)? {
        hls::Playlist::Media {
            segments,
            ended,
            vod,
            ..
        } => {
            // A live window has no end to seek in; later.
            if !(ended || vod) {
                return Err("unsupported stream".into());
            }
            hls_track(base, origin, &segments)
        }
        hls::Playlist::Master { .. } => Err("unsupported stream".into()),
    }
}

async fn load_track(sh: &Shared, src: &VariantSrc) -> Result<Track, String> {
    match src {
        VariantSrc::Playlist(url) => {
            let raw = get_bytes(sh, url, None, hls::MAX_TEXT).await?;
            let text = String::from_utf8(raw).map_err(|_| "no media".to_owned())?;
            hls_media(url, &sh.origin, &text)
        }
        VariantSrc::Dash(rep) => {
            let origin = &sh.origin;
            let part = |u: &str, r: Option<Range>| -> Result<Part, String> {
                Ok(Part {
                    url: resolve(origin, u, origin)?,
                    range: r,
                })
            };
            let mut segs = Vec::new();
            let mk = |part: Part, dur: Duration, init: Option<Part>| Seg {
                part,
                dur,
                start: Duration::ZERO,
                init,
                key: None,
                seq: 0,
                discont: false,
            };
            match &rep.segs {
                dash::SegSpec::List { init, media } => {
                    let init = match init {
                        Some((u, r)) => Some(part(u, *r)?),
                        None => None,
                    };
                    for (u, r, d) in media {
                        segs.push(mk(part(u, *r)?, *d, init.clone()));
                    }
                }
                dash::SegSpec::Base {
                    url,
                    index_range,
                    init_range,
                } => {
                    let url = resolve(origin, url, origin)?;
                    let idx = get_bytes(sh, &url, Some(*index_range), MAX_SEGMENT).await?;
                    let init = init_range
                        .or_else(|| (index_range.0 > 0).then(|| (0, index_range.0 - 1)))
                        .map(|r| Part {
                            url: url.clone(),
                            range: Some(r),
                        });
                    for ((a, b), d) in dash::parse_sidx(&idx, index_range.1 + 1) {
                        let p = Part {
                            url: url.clone(),
                            range: Some((a, b)),
                        };
                        segs.push(mk(p, d, init.clone()));
                    }
                }
            }
            if segs.is_empty() {
                return Err("no media".into());
            }
            Ok(Track::new(segs))
        }
    }
}

fn new_state(track: Track, variant: usize, auto: bool) -> State {
    State {
        track: Arc::new(track),
        epoch: 0,
        next: 0,
        ready: HashMap::new(),
        inflight: HashMap::new(),
        failed: HashMap::new(),
        outbox: VecDeque::new(),
        keys: HashMap::new(),
        last_init: None,
        need_discont: false,
        est_bps: None,
        avg_bytes: 0,
        slow_streak: 0,
        auto,
        variant,
        switching: None,
        switched: None,
        switch_task: None,
    }
}

fn finish(
    handle: Handle,
    client: reqwest::Client,
    origin: String,
    headers: Vec<(String, String)>,
    (variants, srcs): (Vec<VariantInfo>, Vec<VariantSrc>),
    track: Track,
    variant: usize,
) -> SegmentedReader {
    let sh = Arc::new(Shared {
        client,
        origin,
        headers: allowed_headers(headers),
        variants,
        srcs,
        closed: AtomicBool::new(false),
        state: Mutex::new(new_state(track, variant, true)),
        cv: Condvar::new(),
    });
    SegmentedReader { handle, sh }
}

fn probe(client: reqwest::Client, url: &str, headers: Vec<(String, String)>) -> Shared {
    Shared {
        client,
        origin: url.to_owned(),
        headers: allowed_headers(headers),
        variants: Vec::new(),
        srcs: Vec::new(),
        closed: AtomicBool::new(false),
        state: Mutex::new(new_state(Track::new(Vec::new()), 0, true)),
        cv: Condvar::new(),
    }
}

/// Open an HLS VOD stream (a master or a media playlist).
pub async fn open_hls(
    handle: Handle,
    client: reqwest::Client,
    url: String,
    quality: Quality,
    headers: Vec<(String, String)>,
) -> Result<SegmentedReader, String> {
    let p = probe(client.clone(), &url, headers.clone());
    let raw = get_bytes(&p, &url, None, hls::MAX_TEXT).await?;
    let text = String::from_utf8(raw).map_err(|_| "no media".to_owned())?;
    match hls::parse(&text)? {
        hls::Playlist::Media { .. } => {
            let track = hls_media(&url, &url, &text)?;
            Ok(finish(
                handle,
                client,
                url,
                headers,
                (Vec::new(), Vec::new()),
                track,
                0,
            ))
        }
        hls::Playlist::Master { variants, audio } => {
            let mut info = Vec::new();
            let mut srcs = Vec::new();
            let mut separate_audio = Vec::new();
            for v in &variants {
                // A variant pointing inward from a public master is dropped.
                let Ok(u) = resolve(&url, &v.uri, &url) else {
                    continue;
                };
                info.push(VariantInfo {
                    height: v.height,
                    bandwidth: v.bandwidth,
                });
                separate_audio.push(
                    v.audio_group
                        .as_ref()
                        .is_some_and(|g| audio.iter().any(|a| &a.group == g && a.uri.is_some())),
                );
                srcs.push(VariantSrc::Playlist(u));
            }
            if info.is_empty() {
                return Err("unsupported stream".into());
            }
            let pairs: Vec<(Option<u32>, u64)> =
                info.iter().map(|i| (i.height, i.bandwidth)).collect();
            let idx = pick_variant(&pairs, quality, None);
            if separate_audio[idx] {
                return Err("unsupported stream".into());
            }
            let track = load_track(&p, &srcs[idx]).await?;
            Ok(finish(
                handle,
                client,
                url,
                headers,
                (info, srcs),
                track,
                idx,
            ))
        }
    }
}

/// Open a static single-period DASH stream (list, template or sidx based).
pub async fn open_dash(
    handle: Handle,
    client: reqwest::Client,
    url: String,
    quality: Quality,
    headers: Vec<(String, String)>,
) -> Result<SegmentedReader, String> {
    let p = probe(client.clone(), &url, headers.clone());
    let raw = get_bytes(&p, &url, None, dash::MAX_TEXT).await?;
    let mpd = dash::parse(&raw)?;
    let has = |k: dash::Kind| mpd.sets.iter().any(|s| s.kind == k);
    if has(dash::Kind::Video) && has(dash::Kind::Audio) {
        // Separate audio is not muxed in yet.
        return Err("unsupported stream".into());
    }
    let set = mpd
        .sets
        .iter()
        .find(|s| s.kind == dash::Kind::Video)
        .or_else(|| mpd.sets.first())
        .ok_or_else(|| "no media".to_owned())?;
    let info: Vec<VariantInfo> = set
        .reps
        .iter()
        .map(|r| VariantInfo {
            height: r.height,
            bandwidth: r.bandwidth,
        })
        .collect();
    let pairs: Vec<(Option<u32>, u64)> = info.iter().map(|i| (i.height, i.bandwidth)).collect();
    let idx = pick_variant(&pairs, quality, None);
    let srcs: Vec<VariantSrc> = set
        .reps
        .iter()
        .map(|r| VariantSrc::Dash(Box::new(r.clone())))
        .collect();
    let track = load_track(&p, &srcs[idx]).await?;
    Ok(finish(
        handle,
        client,
        url,
        headers,
        (info, srcs),
        track,
        idx,
    ))
}

// ---- reader -----------------------------------------------------------------

impl SegmentedReader {
    /// Start downloads for the window ahead of `next`.
    fn top_up(&self, st: &mut State) {
        let track = Arc::clone(&st.track);
        if st.next >= track.segs.len() {
            return;
        }
        let base = track.segs[st.next].start;
        let mut bytes: usize = st.ready.values().map(|r| r.bytes.len()).sum();
        for idx in st.next..track.segs.len() {
            let seg = &track.segs[idx];
            let wanted_now = idx == st.next;
            if !wanted_now && seg.start.saturating_sub(base) >= WINDOW {
                break;
            }
            if st.ready.contains_key(&idx)
                || st.inflight.contains_key(&idx)
                || st.failed.contains_key(&idx)
            {
                continue;
            }
            if st.inflight.len() >= MAX_CONCURRENT {
                break;
            }
            if !wanted_now && bytes + st.inflight.len() * st.avg_bytes > MAX_BUFFER {
                break;
            }
            bytes += st.avg_bytes;
            let h = self
                .handle
                .spawn(fetch_task(Arc::clone(&self.sh), st.epoch, idx, seg.clone()));
            st.inflight.insert(idx, h.abort_handle());
        }
    }

    /// Keep one segment behind and nothing beyond the window.
    fn evict(st: &mut State) {
        let track = Arc::clone(&st.track);
        if track.segs.is_empty() {
            return;
        }
        let lo = st.next.saturating_sub(1);
        let hi_t = track.segs.get(st.next).map_or(track.total, |s| s.start) + WINDOW;
        let keep = |i: usize| i >= lo && track.segs.get(i).is_some_and(|s| s.start <= hi_t);
        st.ready.retain(|i, _| keep(*i));
        let stale: Vec<usize> = st.inflight.keys().copied().filter(|i| !keep(*i)).collect();
        for i in stale {
            if let Some(h) = st.inflight.remove(&i) {
                h.abort();
            }
        }
    }

    fn apply_switch(&self, st: &mut State) {
        let Some((v, track)) = st.switched.take() else {
            return;
        };
        let at = st
            .track
            .segs
            .get(st.next)
            .map_or(st.track.total, |s| s.start);
        let idx = if track.segs.is_empty() || at >= track.total {
            track.segs.len()
        } else {
            track.index_at(at)
        };
        st.epoch += 1;
        for (_, h) in st.inflight.drain() {
            h.abort();
        }
        st.ready.clear();
        st.failed.clear();
        st.track = Arc::new(track);
        st.next = idx;
        st.variant = v;
        st.switching = None;
    }

    fn emit(st: &mut State, idx: usize) {
        let seg = st.track.segs[idx].clone();
        let Some(r) = st.ready.get(&idx) else {
            return;
        };
        let (bytes, init) = (Arc::clone(&r.bytes), r.init.clone());
        let flag = st.need_discont || seg.discont;
        let mut first = true;
        if let (Some(p), Some(i)) = (&seg.init, init)
            && st.last_init.as_ref() != Some(p)
        {
            st.outbox.push_back(Chunk {
                bytes: i.as_ref().clone(),
                pts: None,
                discont: flag,
            });
            first = false;
        }
        st.outbox.push_back(Chunk {
            bytes: bytes.as_ref().clone(),
            pts: Some(seg.start),
            discont: flag && first,
        });
        st.last_init = seg.init.clone();
        st.need_discont = false;
        st.next = idx + 1;
    }
}

impl Drop for SegmentedReader {
    fn drop(&mut self) {
        // Closing the player cancels every request still in flight.
        self.sh.closed.store(true, Ordering::SeqCst);
        let mut st = self.sh.lock();
        st.epoch += 1;
        for (_, h) in st.inflight.drain() {
            h.abort();
        }
        if let Some(h) = st.switch_task.take() {
            h.abort();
        }
        drop(st);
        self.sh.cv.notify_all();
    }
}

impl TimedSource for SegmentedReader {
    fn duration(&self) -> Option<Duration> {
        Some(self.sh.lock().track.total)
    }

    fn seek(&mut self, t: Duration) -> Duration {
        let mut st = self.sh.lock();
        let track = Arc::clone(&st.track);
        if track.segs.is_empty() || t >= track.total {
            st.next = track.segs.len();
            st.outbox.clear();
            return track.total;
        }
        let idx = track.index_at(t);
        st.next = idx;
        st.need_discont = true;
        st.last_init = None;
        st.outbox.clear();
        st.failed.clear();
        Self::evict(&mut st);
        self.top_up(&mut st);
        track.segs[idx].start
    }

    fn pull(&mut self) -> io::Result<Option<Chunk>> {
        let deadline = Instant::now() + WAIT_SLICE;
        let sh = Arc::clone(&self.sh);
        let mut st = sh.lock();
        loop {
            if let Some(c) = st.outbox.pop_front() {
                return Ok(Some(c));
            }
            self.apply_switch(&mut st);
            if st.next >= st.track.segs.len() {
                return Ok(None);
            }
            self.top_up(&mut st);
            let idx = st.next;
            if st.ready.contains_key(&idx) {
                Self::emit(&mut st, idx);
                Self::evict(&mut st);
                continue;
            }
            if let Some(e) = st.failed.get(&idx) {
                return Err(io::Error::other(e.clone()));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            st = sh
                .cv
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn buffered_until(&self) -> Option<Duration> {
        let st = self.sh.lock();
        let track = &st.track;
        let first = track.segs.get(st.next)?;
        let mut t = first.start;
        let mut i = st.next;
        while st.ready.contains_key(&i) {
            t = track.segs[i].start + track.segs[i].dur;
            i += 1;
        }
        Some(t)
    }

    fn variants(&self) -> Vec<VariantInfo> {
        self.sh.variants.clone()
    }

    fn current_variant(&self) -> Option<usize> {
        (!self.sh.variants.is_empty()).then(|| self.sh.lock().variant)
    }

    fn select_variant(&mut self, v: Option<usize>) {
        let mut st = self.sh.lock();
        let Some(i) = v else {
            st.auto = true;
            return;
        };
        st.auto = false;
        if i >= self.sh.variants.len() || i == st.variant || st.switching == Some(i) {
            return;
        }
        st.switching = Some(i);
        st.switched = None;
        if let Some(h) = st.switch_task.take() {
            h.abort();
        }
        let sh = Arc::clone(&self.sh);
        let task = self.handle.spawn(async move {
            let res = load_track(&sh, &sh.srcs[i]).await;
            let mut st = sh.lock();
            if st.switching != Some(i) {
                return;
            }
            match res {
                Ok(t) => st.switched = Some((i, t)),
                // Stay where we are; the current variant still plays.
                Err(_) => st.switching = None,
            }
            drop(st);
            sh.cv.notify_all();
        });
        st.switch_task = Some(task.abort_handle());
    }

    fn wants_downgrade(&mut self) -> bool {
        let mut st = self.sh.lock();
        if !st.auto || self.sh.variants.len() < 2 || st.slow_streak < 2 {
            return false;
        }
        let lowest = (0..self.sh.variants.len())
            .min_by_key(|&i| self.sh.variants[i].bandwidth)
            .unwrap_or(0);
        if st.variant == lowest || st.ready.contains_key(&st.next) {
            return false;
        }
        st.slow_streak = 0;
        true
    }
}

#[allow(dead_code)]
fn _assert_send(r: SegmentedReader) -> impl Send {
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_three_headers_pass() {
        let h = allowed_headers(vec![
            ("User-Agent".into(), "x".into()),
            ("authorization".into(), "secret".into()),
            ("X-Api-Key".into(), "secret".into()),
            ("REFERER".into(), "https://a.invalid/".into()),
            ("Origin".into(), "bad\r\nvalue".into()),
        ]);
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["User-Agent", "REFERER"]);
    }

    #[test]
    fn urls_are_joined_and_judged() {
        let o = "https://cdn.example.invalid/a/m.m3u8";
        assert_eq!(
            resolve(o, "../b/s.ts", o).unwrap(),
            "https://cdn.example.invalid/b/s.ts"
        );
        assert!(resolve(o, "http://127.0.0.1/x", o).is_err());
        assert!(resolve(o, "ftp://cdn.example.invalid/x", o).is_err());
        assert!(
            resolve(
                "http://127.0.0.1/m.m3u8",
                "http://10.0.0.2/x",
                "http://127.0.0.1/m.m3u8"
            )
            .is_ok()
        );
    }

    #[test]
    fn decrypt_rejects_odd_lengths_and_bad_padding() {
        let (k, iv) = ([1u8; 16], [2u8; 16]);
        assert!(decrypt(&k, &iv, &[0u8; 15]).is_err());
        assert!(decrypt(&k, &iv, &[0u8; 16]).is_err());
    }

    #[test]
    fn track_prefix_sums_and_binary_search() {
        let seg = |d: u64| Seg {
            part: Part {
                url: String::new(),
                range: None,
            },
            dur: Duration::from_secs(d),
            start: Duration::ZERO,
            init: None,
            key: None,
            seq: 0,
            discont: false,
        };
        let t = Track::new(vec![seg(2), seg(2), seg(3)]);
        assert_eq!(t.total, Duration::from_secs(7));
        assert_eq!(t.index_at(Duration::from_millis(1999)), 0);
        assert_eq!(t.index_at(Duration::from_secs(2)), 1);
        assert_eq!(t.index_at(Duration::from_millis(4500)), 2);
        assert_eq!(t.index_at(Duration::from_secs(99)), 2);
    }
}
