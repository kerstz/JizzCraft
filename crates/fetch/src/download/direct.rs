//! Save one plain-HTTP file to disk.
//!
//! The file is written to `<name>.part` (created new, owner only) and moved
//! to its final name without ever replacing anything. A dropped connection
//! resumes with `Range` when the server honours it and restarts from zero
//! when it does not; a changed file (validator or length) restarts once and
//! then fails. Cancelling or failing removes the `.part` this download
//! created and nothing else.

use super::{FreeSpace, JobCtl, RateLimiter, Renew};
use crate::diskspace::{RUN_MARGIN, is_full, is_low};
use crate::formats::allowed_headers;
use crate::savename::{
    create_part, ext_for, part_path, remove_own_part, rename_noclobber, same_file, unique_path,
};
use crate::service::{FetchService, proxy_blocked, same_origin_policy};
use crate::sniff::{Sniff, sniff};
use crate::stream::{
    STALL_TIMEOUT, content_range, jitter, net_error, send_ttfb, status_reason, validator_of,
};
use reqwest::Response;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How many attempts in a row may end without a single new byte.
const MAX_FAILS: u32 = 6;
const HEAD: usize = 4096;
const RENEWALS: u32 = 3;
const SLICE: Duration = Duration::from_millis(100);
const CONNECT: Duration = Duration::from_secs(15);

static RETRY_BASE_MS: AtomicU64 = AtomicU64::new(1000);

/// Tests shorten the pause between attempts. `None` restores the default.
#[doc(hidden)]
pub fn set_retry_base(d: Option<Duration>) {
    let ms = d.map_or(1000, |d| d.as_millis() as u64);
    RETRY_BASE_MS.store(ms, Ordering::SeqCst);
}

enum ReqErr {
    Fatal(String),
    Retry(String),
    /// 403 or 410: the address may have expired.
    Denied(u16),
}

enum Pump {
    Eof,
    Paused,
    Retry(String),
    Fatal(String),
}

struct Info {
    start: u64,
    total: Option<u64>,
    validator: Option<String>,
    resumable: bool,
}

struct Job<'a> {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    ctl: &'a Arc<JobCtl>,
    limiter: &'a Arc<RateLimiter>,
    free: &'a FreeSpace,
    dir0: PathBuf,
    file: File,
    done: u64,
    total: Option<u64>,
    validator: Option<String>,
    resumable: bool,
    restarts: u32,
    last_free_check: Instant,
    window: (Instant, u64),
}

/// Download `url` into `dir` as `<stem>.<ext>`; the extension comes from the
/// bytes, never from the address. `renew` supplies a fresh address when the
/// server starts refusing the old one.
#[allow(clippy::too_many_arguments)]
pub async fn save_direct(
    url: &str,
    headers: &[(String, String)],
    dir: &Path,
    stem: &str,
    ctl: &Arc<JobCtl>,
    limiter: &Arc<RateLimiter>,
    free: &FreeSpace,
    renew: Option<&Renew>,
) -> Result<PathBuf, String> {
    if proxy_blocked() {
        return Err("proxy required".into());
    }
    let client = FetchService::builder()?
        .connect_timeout(CONNECT)
        .redirect(same_origin_policy())
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let headers = allowed_headers(headers.to_vec());
    let (mut resp, info) = match request(&client, url, &headers, 0).await {
        Ok(x) => x,
        Err(ReqErr::Denied(403)) => return Err("forbidden".into()),
        Err(ReqErr::Denied(c)) => return Err(status_reason(c)),
        Err(ReqErr::Fatal(e) | ReqErr::Retry(e)) => return Err(e),
    };
    ctl.resumable.store(info.resumable, Ordering::SeqCst);
    ctl.total.store(info.total.unwrap_or(0), Ordering::SeqCst);

    // The first block decides what this is before anything is created.
    let mut head: Vec<u8> = Vec::new();
    while head.len() < HEAD {
        if ctl.cancelled() {
            return Err("cancelled".into());
        }
        if proxy_blocked() {
            return Err("proxy required".into());
        }
        match tokio::time::timeout(STALL_TIMEOUT, resp.chunk()).await {
            Err(_) => return Err("stalled".into()),
            Ok(Err(e)) => return Err(net_error(&e)),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(c))) => head.extend_from_slice(&c),
        }
    }
    match sniff(&head) {
        Sniff::Hls | Sniff::Dash => return Err("refused: this refers to network media".into()),
        Sniff::Other => return Err("not a video".into()),
        Sniff::Media => {}
    }
    let ext = ext_for(&head).unwrap_or("bin");

    if !std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Err("no folder".into());
    }
    let dir0 = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    if let Some(t) = info.total
        && is_full(free(&dir0), t)
    {
        return Err("disk full".into());
    }
    let (final_path, file) = reserve(&dir0, stem, ext)?;
    let part = part_path(&final_path);
    let mut job = Job {
        client,
        url: url.to_owned(),
        headers,
        ctl,
        limiter,
        free,
        dir0,
        file,
        done: 0,
        total: info.total,
        validator: info.validator,
        resumable: info.resumable,
        restarts: 0,
        last_free_check: Instant::now(),
        window: (Instant::now(), 0),
    };
    match job.run(resp, head, &final_path, &part, renew).await {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = remove_own_part(&job.file, &part);
            Err(e)
        }
    }
}

/// Pick a free name and create its `.part`. Racing creators lose to
/// `create_new` and try the next name.
fn reserve(dir: &Path, stem: &str, ext: &str) -> Result<(PathBuf, File), String> {
    let mut last = String::from("no folder");
    for _ in 0..4 {
        let final_path = unique_path(dir, stem, ext);
        match create_part(&part_path(&final_path)) {
            Ok(f) => return Ok((final_path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// One GET from byte `from`. Only the status line is judged here.
async fn request(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    from: u64,
) -> Result<(Response, Info), ReqErr> {
    if proxy_blocked() {
        return Err(ReqErr::Fatal("proxy required".into()));
    }
    let mut req = client.get(url).header("Range", format!("bytes={from}-"));
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = send_ttfb(req).await.map_err(|e| {
        if e == "clock skew" {
            ReqErr::Fatal(e)
        } else {
            ReqErr::Retry(e)
        }
    })?;
    let code = resp.status().as_u16();
    let info = match code {
        206 => {
            let (a, _, len) =
                content_range(&resp).ok_or_else(|| ReqErr::Fatal("not a video".into()))?;
            Info {
                start: a,
                total: Some(len),
                validator: validator_of(&resp),
                resumable: true,
            }
        }
        200 => Info {
            start: 0,
            total: resp.content_length(),
            validator: validator_of(&resp),
            resumable: false,
        },
        403 | 410 => return Err(ReqErr::Denied(code)),
        429 | 500..=599 => return Err(ReqErr::Retry(status_reason(code))),
        _ => return Err(ReqErr::Fatal(status_reason(code))),
    };
    Ok((resp, info))
}

impl Job<'_> {
    fn gone(&self) -> Option<String> {
        if self.ctl.cancelled() {
            Some("cancelled".into())
        } else if proxy_blocked() {
            Some("proxy required".into())
        } else {
            None
        }
    }

    /// Sleep, but wake for a cancel or the kill switch.
    async fn nap(&self, d: Duration) -> Result<(), String> {
        let end = Instant::now() + d;
        loop {
            if let Some(e) = self.gone() {
                return Err(e);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            tokio::time::sleep(left.min(SLICE)).await;
        }
    }

    fn publish(&self) {
        self.ctl.done.store(self.done, Ordering::SeqCst);
        self.ctl
            .total
            .store(self.total.unwrap_or(0), Ordering::SeqCst);
        self.ctl.resumable.store(self.resumable, Ordering::SeqCst);
    }

    fn reset_file(&mut self) -> Result<(), String> {
        self.file.set_len(0).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        self.done = 0;
        self.publish();
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file.write_all(bytes).map_err(|e| e.to_string())?;
        self.done += bytes.len() as u64;
        self.ctl.done.store(self.done, Ordering::SeqCst);
        Ok(())
    }

    /// Speed over the last half second or more; room on the disk once a
    /// second. A nearly full volume pauses the job and keeps what is there.
    fn tick(&mut self) {
        let now = Instant::now();
        let span = now.saturating_duration_since(self.window.0);
        if span >= Duration::from_millis(500) {
            let moved = self.done.saturating_sub(self.window.1);
            self.ctl
                .bps
                .store((moved as f64 / span.as_secs_f64()) as u64, Ordering::SeqCst);
            self.window = (now, self.done);
        }
        if now.saturating_duration_since(self.last_free_check) >= Duration::from_secs(1) {
            self.last_free_check = now;
            let remaining = self.total.map_or(0, |t| t.saturating_sub(self.done));
            if is_low((self.free)(&self.dir0), remaining, RUN_MARGIN) {
                self.ctl.disk_low.store(true, Ordering::SeqCst);
                self.ctl.pause.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Read the body until it ends, the job is paused, or something breaks.
    async fn pump(&mut self, resp: &mut Response) -> Pump {
        let mut quiet = Instant::now();
        loop {
            if let Some(e) = self.gone() {
                return Pump::Fatal(e);
            }
            if self.ctl.paused() {
                return Pump::Paused;
            }
            let chunk = match tokio::time::timeout(Duration::from_millis(250), resp.chunk()).await {
                Err(_) => {
                    if quiet.elapsed() >= STALL_TIMEOUT {
                        return Pump::Retry("stalled".into());
                    }
                    continue;
                }
                Ok(Err(e)) => {
                    return Pump::Retry(if e.is_body() || e.is_decode() {
                        "incomplete download".into()
                    } else {
                        net_error(&e)
                    });
                }
                Ok(Ok(None)) => return Pump::Eof,
                Ok(Ok(Some(c))) => c,
            };
            quiet = Instant::now();
            let wait = self.limiter.take(chunk.len(), Instant::now());
            if !wait.is_zero()
                && let Err(e) = self.nap(wait).await
            {
                return Pump::Fatal(e);
            }
            if let Err(e) = self.write(&chunk) {
                return Pump::Fatal(e);
            }
            if self.total.is_some_and(|t| self.done > t) {
                return Pump::Fatal("source changed".into());
            }
            self.tick();
        }
    }

    /// Wait out a pause. `Err` for a cancel or the kill switch.
    async fn wait_resume(&self) -> Result<(), String> {
        while self.ctl.paused() {
            self.nap(SLICE).await?;
        }
        Ok(())
    }

    fn check_same(&self, info: &Info) -> Result<(), String> {
        let total_ok = match (self.total, info.total) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let valid_ok = match (&self.validator, &info.validator) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        if total_ok && valid_ok {
            Ok(())
        } else {
            Err("source changed".into())
        }
    }

    /// A new connection after a pause or a failure: resume where the file
    /// ends when the server allows it, otherwise start again from zero.
    async fn reconnect(&mut self, renew: Option<&Renew>) -> Result<Response, Pump> {
        let mut want_resume = self.resumable && self.done > 0;
        let mut renewals = 0;
        let mut renewed = false;
        loop {
            let from = if want_resume { self.done } else { 0 };
            let (resp, info) = match request(&self.client, &self.url, &self.headers, from).await {
                Ok(x) => x,
                Err(ReqErr::Denied(code)) => {
                    let Some(renew) = renew.filter(|_| renewals < RENEWALS) else {
                        return Err(Pump::Fatal(if code == 403 {
                            "forbidden".into()
                        } else {
                            status_reason(code)
                        }));
                    };
                    renewals += 1;
                    self.url = renew_url(renew).await.map_err(Pump::Fatal)?;
                    renewed = true;
                    continue;
                }
                Err(ReqErr::Retry(e)) => return Err(Pump::Retry(e)),
                Err(ReqErr::Fatal(e)) => return Err(Pump::Fatal(e)),
            };
            if renewed {
                self.check_same(&info).map_err(Pump::Fatal)?;
                renewed = false;
            }
            if want_resume && info.start == from {
                if self.check_same(&info).is_ok() {
                    return Ok(resp);
                }
                // The file on the server is not the one we started: begin
                // again, once. A second change is final.
                self.restarts += 1;
                if self.restarts > 1 {
                    return Err(Pump::Fatal("source changed".into()));
                }
                drop(resp);
                self.reset_file().map_err(Pump::Fatal)?;
                self.validator = None;
                self.total = None;
                want_resume = false;
                continue;
            }
            if info.start != 0 {
                return Err(Pump::Fatal("source changed".into()));
            }
            // From zero: a fresh start, a server without ranges, or the
            // restart above. Whatever was written is dropped from our own
            // handle only.
            self.reset_file().map_err(Pump::Fatal)?;
            self.total = info.total;
            self.validator = info.validator;
            self.resumable = info.resumable;
            self.publish();
            return Ok(resp);
        }
    }

    async fn run(
        &mut self,
        first: Response,
        head: Vec<u8>,
        final_path: &Path,
        part: &Path,
        renew: Option<&Renew>,
    ) -> Result<PathBuf, String> {
        self.write(&head)?;
        let mut cur = Some(first);
        let mut fails = 0u32;
        let mut high = self.done;
        loop {
            if let Some(e) = self.gone() {
                return Err(e);
            }
            if self.ctl.paused() {
                // Hang up while paused; resume asks for the rest.
                cur = None;
                self.ctl.bps.store(0, Ordering::SeqCst);
                self.wait_resume().await?;
                self.window = (Instant::now(), self.done);
                self.last_free_check = Instant::now();
                continue;
            }
            let mut resp = match cur.take() {
                Some(r) => r,
                None => match self.reconnect(renew).await {
                    Ok(r) => r,
                    Err(Pump::Fatal(e)) => return Err(e),
                    Err(Pump::Retry(e)) => {
                        fails += 1;
                        if fails > MAX_FAILS {
                            return Err(e);
                        }
                        self.back_off(fails).await?;
                        continue;
                    }
                    Err(_) => continue,
                },
            };
            let outcome = self.pump(&mut resp).await;
            drop(resp);
            if self.done > high {
                high = self.done;
                fails = 0;
            }
            match outcome {
                Pump::Paused => {}
                Pump::Fatal(e) => return Err(e),
                Pump::Retry(e) => {
                    fails += 1;
                    if fails > MAX_FAILS {
                        return Err(e);
                    }
                    self.back_off(fails).await?;
                }
                Pump::Eof => match self.total {
                    Some(t) if self.done < t => {
                        fails += 1;
                        if fails > MAX_FAILS {
                            return Err("incomplete download".into());
                        }
                        self.back_off(fails).await?;
                    }
                    _ => return self.finish(final_path, part),
                },
            }
        }
    }

    async fn back_off(&self, attempt: u32) -> Result<(), String> {
        let base = Duration::from_millis(RETRY_BASE_MS.load(Ordering::SeqCst).max(1));
        let wait = (base * (1 << attempt.min(4))).min(Duration::from_secs(8));
        self.nap(jitter(wait, u64::from(attempt))).await
    }

    /// Check the whole file, then give it its name without replacing
    /// anything.
    fn finish(&mut self, final_path: &Path, part: &Path) -> Result<PathBuf, String> {
        self.ctl.verifying.store(true, Ordering::SeqCst);
        if let Some(t) = self.total
            && self.done != t
        {
            return Err("incomplete download".into());
        }
        self.file.sync_all().map_err(|e| e.to_string())?;
        let parent = final_path.parent().unwrap_or(Path::new("."));
        if std::fs::canonicalize(parent).ok().as_deref() != Some(self.dir0.as_path())
            || !same_file(&self.file, part)
        {
            return Err("folder changed".into());
        }
        let mut target = final_path.to_path_buf();
        let mut tries = 0;
        loop {
            match rename_noclobber(part, &target) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 3 => {
                    tries += 1;
                    let stem = final_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("video");
                    let ext = final_path
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("bin");
                    target = unique_path(&self.dir0, stem, ext);
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        match std::fs::metadata(&target) {
            Ok(m) if m.len() == self.done => Ok(target),
            _ => {
                // Ours, moved: take it back out rather than leave a short file.
                let _ = remove_own_part(&self.file, &target);
                Err("incomplete download".into())
            }
        }
    }
}

/// Ask for a fresh address and wait for the answer without blocking the
/// runtime.
async fn renew_url(renew: &Renew) -> Result<String, String> {
    let rx = renew();
    tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(60)))
        .await
        .map_err(|_| "expired".to_string())?
        .map_err(|_| "expired".to_string())?
        .map_err(|_| "expired".to_string())
}
