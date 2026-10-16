//! The download queue: in memory only, nothing about a job is persisted.
//!
//! A job is Queued, then (when a slot is free) Probing while a page is read,
//! then Running while a saver writes a `.part`, Verifying while the file is
//! checked and given its name, and finally Done, Failed or Cancelled. The
//! scheduler asks the kill switch before it starts anything: a blocked job
//! fails with `proxy required` and zero requests. Views carry numbers and
//! one of the fixed sentences, never an address, a title in a log or a path.

use super::{
    FreeSpace, JobCtl, JobId, JobKind, JobState, JobView, RateLimiter, Renew, eta, real_free_space,
    to_start,
};
use crate::config::FetchConfig;
use crate::errors::{self, classify};
use crate::formats::{Entry, Format, MAX_ENTRIES, Meta, muxed};
use crate::pick::{Prefer, Quality};
use crate::savename::stem_from_title;
use crate::service::{ExtractMode, FetchService, Via, extract_task, proxy_blocked};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::runtime::Handle;

/// Where the bytes come from.
#[derive(Clone)]
pub enum SaveSource {
    /// An address typed or pasted: a file, a manifest or a page.
    Url(String),
    /// A format the user already picked from an extraction.
    Picked {
        format: Format,
        meta: Meta,
        /// The page it came from, to renew an expired link.
        page: Option<String>,
        via: Via,
        /// Position of the entry on the page and of the format in the
        /// entry's muxed list, to find the same file again.
        entry: usize,
        index: usize,
    },
}

impl std::fmt::Debug for SaveSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Url(_) => "SaveSource::Url",
            Self::Picked { .. } => "SaveSource::Picked",
        })
    }
}

#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub source: SaveSource,
    pub quality: Quality,
    /// Absolute and existing; `None` is the configured save folder.
    pub dir: Option<PathBuf>,
    /// For a page with several videos: queue every one, not only the first.
    pub all_entries: bool,
}

impl DownloadRequest {
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            source: SaveSource::Url(url.into()),
            quality: Quality::default(),
            dir: None,
            all_entries: false,
        }
    }
}

/// What ended since the last call. Counts and folders, never names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finished {
    pub done: usize,
    pub failed: usize,
    pub dirs: Vec<PathBuf>,
}

struct Rec {
    id: JobId,
    kind: JobKind,
    state: JobState,
    ctl: Arc<JobCtl>,
    source: SaveSource,
    quality: Quality,
    dir: Option<PathBuf>,
    all: bool,
    title: Option<String>,
    height: Option<u32>,
    embed: bool,
    error: Option<&'static str>,
}

#[derive(Default)]
struct Mgr {
    next: JobId,
    jobs: Vec<Rec>,
    done: usize,
    failed: usize,
    dirs: BTreeSet<PathBuf>,
}

pub(crate) struct Downloads {
    inner: Arc<Inner>,
}

struct Inner {
    handle: Handle,
    cfg: Mutex<Arc<FetchConfig>>,
    limiter: Arc<RateLimiter>,
    free: Mutex<FreeSpace>,
    mgr: Mutex<Mgr>,
    closing: AtomicBool,
    active: AtomicUsize,
}

struct Active(Arc<Inner>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn bytes_per_sec(cfg: &FetchConfig) -> Option<u64> {
    cfg.down_limit_mibps
        .map(|m| u64::from(m).saturating_mul(1024 * 1024))
}

fn kind_of(f: &Format) -> JobKind {
    match f.protocol.as_str() {
        "hls" => JobKind::Hls,
        "dash" => JobKind::Dash,
        _ => JobKind::Direct,
    }
}

/// Which of the formats (best first) to save. `Smallest` takes the lowest
/// known height; `Best` the first not taller than the cap, else the best.
pub fn pick_format(fmts: &[Format], q: Quality) -> usize {
    match q.prefer {
        Prefer::Smallest => fmts
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.height.map(|h| (h, i)))
            .min()
            .map_or(0, |(_, i)| i),
        Prefer::Best => match q.max_height {
            None => 0,
            Some(m) => fmts
                .iter()
                .position(|f| f.height.is_some_and(|h| h <= m))
                .or_else(|| fmts.iter().position(|f| f.height.is_none()))
                .unwrap_or(0),
        },
    }
}

async fn cancel_wait(ctl: &JobCtl) {
    while !ctl.cancelled() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Downloads {
    pub(crate) fn new(handle: Handle, cfg: Arc<FetchConfig>) -> Self {
        let limiter = Arc::new(RateLimiter::new(bytes_per_sec(&cfg)));
        Self {
            inner: Arc::new(Inner {
                handle,
                cfg: Mutex::new(cfg),
                limiter,
                free: Mutex::new(real_free_space()),
                mgr: Mutex::new(Mgr::default()),
                closing: AtomicBool::new(false),
                active: AtomicUsize::new(0),
            }),
        }
    }

    pub(crate) fn set_config(&self, cfg: Arc<FetchConfig>) {
        self.inner.limiter.set(bytes_per_sec(&cfg));
        *self.inner.cfg.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
        self.inner.kick();
    }

    pub(crate) fn set_free_space(&self, f: FreeSpace) {
        *self.inner.free.lock().unwrap_or_else(|e| e.into_inner()) = f;
    }

    pub(crate) fn start(&self, req: DownloadRequest) -> Result<Vec<JobId>, String> {
        if self.inner.closing.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        if let SaveSource::Url(u) = &req.source
            && !(u.starts_with("https://") || u.starts_with("http://"))
        {
            return Err("unsupported address".into());
        }
        if let Some(d) = &req.dir
            && !(d.is_absolute() && d.is_dir())
        {
            return Err("no folder".into());
        }
        let id = self.inner.add(req);
        self.inner.kick();
        Ok(vec![id])
    }

    pub(crate) fn pause(&self, id: JobId) -> bool {
        let mut m = self.inner.lock();
        let Some(r) = m.jobs.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        match r.state {
            JobState::Queued => {
                r.state = JobState::Paused;
                true
            }
            JobState::Probing | JobState::Running => {
                r.ctl.disk_low.store(false, Ordering::SeqCst);
                r.ctl.pause.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn resume(&self, id: JobId) -> bool {
        let ok = {
            let mut m = self.inner.lock();
            let Some(r) = m.jobs.iter_mut().find(|r| r.id == id) else {
                return false;
            };
            match r.state {
                JobState::Paused => {
                    r.state = JobState::Queued;
                    true
                }
                JobState::Probing | JobState::Running => {
                    r.ctl.resume();
                    true
                }
                _ => false,
            }
        };
        self.inner.kick();
        ok
    }

    pub(crate) fn cancel(&self, id: JobId) -> bool {
        let ok = {
            let mut m = self.inner.lock();
            let Some(r) = m.jobs.iter_mut().find(|r| r.id == id) else {
                return false;
            };
            match r.state {
                JobState::Queued | JobState::Paused => {
                    r.ctl.cancel.store(true, Ordering::SeqCst);
                    r.state = JobState::Cancelled;
                    true
                }
                JobState::Probing | JobState::Running | JobState::Verifying => {
                    r.ctl.cancel.store(true, Ordering::SeqCst);
                    true
                }
                _ => false,
            }
        };
        self.inner.kick();
        ok
    }

    /// Drop a finished job from the list. Never touches the saved file.
    pub(crate) fn forget(&self, id: JobId) -> bool {
        let mut m = self.inner.lock();
        let Some(i) = m.jobs.iter().position(|r| r.id == id) else {
            return false;
        };
        if !m.jobs[i].state.is_over() {
            return false;
        }
        m.jobs.remove(i);
        true
    }

    pub(crate) fn list(&self) -> Vec<JobView> {
        self.inner.lock().jobs.iter().map(view).collect()
    }

    pub(crate) fn take_finished(&self) -> Finished {
        let mut m = self.inner.lock();
        if m.jobs.iter().any(|r| !r.state.is_over()) {
            return Finished::default();
        }
        Finished {
            done: std::mem::take(&mut m.done),
            failed: std::mem::take(&mut m.failed),
            dirs: std::mem::take(&mut m.dirs).into_iter().collect(),
        }
    }

    /// The panic switch: everything running or waiting is held. Files stay.
    pub(crate) fn pause_all(&self) {
        let mut m = self.inner.lock();
        for r in &mut m.jobs {
            match r.state {
                JobState::Queued => r.state = JobState::Paused,
                JobState::Probing | JobState::Running => {
                    r.ctl.pause.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    /// Cancel everything and wait (briefly) for the savers to remove the
    /// `.part` files they made. Called before the runtime goes away.
    pub(crate) fn shutdown(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        {
            let mut m = self.inner.lock();
            for r in &mut m.jobs {
                r.ctl.cancel.store(true, Ordering::SeqCst);
                if matches!(r.state, JobState::Queued | JobState::Paused) {
                    r.state = JobState::Cancelled;
                }
            }
        }
        let t0 = std::time::Instant::now();
        while self.inner.active.load(Ordering::SeqCst) > 0 && t0.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// What the job shows right now.
fn effective(r: &Rec) -> JobState {
    match r.state {
        JobState::Running if r.ctl.verifying.load(Ordering::SeqCst) => JobState::Verifying,
        JobState::Running if r.ctl.paused() => JobState::Paused,
        s => s,
    }
}

fn view(r: &Rec) -> JobView {
    let state = effective(r);
    let done = r.ctl.done.load(Ordering::SeqCst);
    let total = r.ctl.total.load(Ordering::SeqCst);
    let fraction = if r.state == JobState::Done {
        1.0
    } else if r.ctl.timed.load(Ordering::SeqCst) {
        r.ctl.fraction_ppm.load(Ordering::SeqCst) as f32 / 1_000_000.0
    } else if total > 0 {
        (done as f64 / total as f64).min(1.0) as f32
    } else {
        0.0
    };
    let bps = if state == JobState::Running {
        r.ctl.bps.load(Ordering::SeqCst)
    } else {
        0
    };
    let error = match state {
        JobState::Paused if r.ctl.disk_low.load(Ordering::SeqCst) => Some(errors::DISK_FULL),
        _ => r.error,
    };
    JobView {
        id: r.id,
        state,
        kind: r.kind,
        done_bytes: done,
        total_bytes: total,
        fraction,
        bps,
        eta_secs: if total > done && state == JobState::Running {
            eta(total - done, bps)
        } else {
            None
        },
        height: r.height,
        resumable: r.ctl.resumable.load(Ordering::SeqCst),
        error,
        title: r.title.clone(),
        embed: r.embed,
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Mgr> {
        self.mgr.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cfg(&self) -> Arc<FetchConfig> {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn add(&self, req: DownloadRequest) -> JobId {
        let (kind, title, height, embed) = match &req.source {
            SaveSource::Url(_) => (JobKind::Direct, None, None, false),
            SaveSource::Picked {
                format, meta, via, ..
            } => (
                kind_of(format),
                meta.title.clone(),
                format.height,
                matches!(via, Via::Builtin { embed: true }),
            ),
        };
        let mut m = self.lock();
        m.next += 1;
        let id = m.next;
        m.jobs.push(Rec {
            id,
            kind,
            state: JobState::Queued,
            ctl: JobCtl::new(),
            source: req.source,
            quality: req.quality,
            dir: req.dir,
            all: req.all_entries,
            title,
            height,
            embed,
            error: None,
        });
        id
    }

    /// Start whatever the limit allows.
    fn kick(self: &Arc<Self>) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let ids = {
            let mut m = self.lock();
            let cur: Vec<(JobId, JobState)> = m.jobs.iter().map(|r| (r.id, effective(r))).collect();
            let ids = to_start(&cur, self.cfg().save.concurrency());
            for r in &mut m.jobs {
                if ids.contains(&r.id) {
                    r.state = JobState::Probing;
                }
            }
            ids
        };
        for id in ids {
            self.active.fetch_add(1, Ordering::SeqCst);
            let guard = Active(self.clone());
            let me = self.clone();
            self.handle.spawn(async move {
                let _g = guard;
                me.run(id).await;
            });
        }
    }

    async fn run(self: &Arc<Self>, id: JobId) {
        let res = self.drive(id).await;
        {
            let mut m = self.lock();
            let (mut ok_dir, mut failed) = (None, false);
            if let Some(r) = m.jobs.iter_mut().find(|r| r.id == id) {
                match res {
                    Ok(p) => {
                        r.state = JobState::Done;
                        r.ctl
                            .done
                            .store(r.ctl.done.load(Ordering::SeqCst), Ordering::SeqCst);
                        ok_dir = p.parent().map(|d| d.to_path_buf());
                    }
                    Err(e) => {
                        let sentence = classify(&e);
                        if sentence == errors::CANCELLED {
                            r.state = JobState::Cancelled;
                        } else {
                            r.state = JobState::Failed;
                            r.error = Some(sentence);
                            failed = true;
                        }
                    }
                }
            }
            if let Some(d) = ok_dir {
                m.done += 1;
                m.dirs.insert(d);
            }
            if failed {
                m.failed += 1;
            }
        }
        self.kick();
    }

    fn update(&self, id: JobId, f: impl FnOnce(&mut Rec)) {
        if let Some(r) = self.lock().jobs.iter_mut().find(|r| r.id == id) {
            f(r);
        }
    }

    async fn drive(self: &Arc<Self>, id: JobId) -> Result<PathBuf, String> {
        let (ctl, source, quality, dir, all) = {
            let m = self.lock();
            let r = m.jobs.iter().find(|r| r.id == id).ok_or("cancelled")?;
            (
                r.ctl.clone(),
                r.source.clone(),
                r.quality,
                r.dir.clone(),
                r.all,
            )
        };
        if ctl.cancelled() {
            return Err("cancelled".into());
        }
        // The kill switch first: a blocked job makes no request at all.
        if proxy_blocked() {
            return Err("proxy required".into());
        }
        let cfg = self.cfg();
        let (format, meta, page, entry, index) = match source {
            SaveSource::Picked {
                format,
                meta,
                page,
                entry,
                index,
                ..
            } => (format, meta, page, entry, index),
            SaveSource::Url(url) => {
                let ex = tokio::select! {
                    r = extract_task(cfg.clone(), url.clone(), ExtractMode::Auto) => r?,
                    _ = cancel_wait(&ctl) => return Err("cancelled".into()),
                };
                let embed = matches!(ex.via, Via::Builtin { embed: true });
                let mut first = None;
                for (i, e) in ex.entries.iter().enumerate().take(MAX_ENTRIES) {
                    let Some((fmt, idx)) = best_of(e, quality) else {
                        continue;
                    };
                    if first.is_none() {
                        first = Some((fmt, e.meta.clone(), i, idx));
                    } else if all {
                        self.add_extra(&fmt, &e.meta, &url, ex.via, i, idx, quality, &dir);
                    }
                }
                let (fmt, meta, i, idx) = first.ok_or("no media")?;
                self.update(id, |r| r.embed = embed);
                self.kick();
                (fmt, meta, Some(url), i, idx)
            }
        };
        self.update(id, |r| {
            r.kind = kind_of(&format);
            r.height = format.height;
            r.title = meta.title.clone();
            r.state = JobState::Running;
        });
        let dir = match dir {
            Some(d) => d,
            None => {
                let d = cfg.effective_save_dir();
                if !d.exists() {
                    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
                }
                d
            }
        };
        let stem = stem_from_title(meta.title.as_deref(), cfg.disguised);
        let free = self.free.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match kind_of(&format) {
            JobKind::Direct => {
                let renew = page.map(|p| self.renewer(p, entry, index));
                super::direct::save_direct(
                    &format.url,
                    &format.headers,
                    &dir,
                    &stem,
                    &ctl,
                    &self.limiter,
                    &free,
                    renew.as_ref(),
                )
                .await
            }
            k => {
                let client = FetchService::builder()?
                    .connect_timeout(Duration::from_secs(15))
                    .redirect(crate::service::same_origin_policy())
                    .build()
                    .map_err(|e| e.without_url().to_string())?;
                let handle = Handle::current();
                let headers = format.headers.clone();
                let pref = crate::segmented::AudioPref::default();
                let open = async {
                    if k == JobKind::Hls {
                        crate::segmented::open_hls_audio(
                            handle,
                            client,
                            format.url,
                            quality,
                            headers,
                            crate::segmented::LiveTuning::default(),
                            pref,
                        )
                        .await
                    } else {
                        crate::segmented::open_dash_audio(
                            handle, client, format.url, quality, headers, pref,
                        )
                        .await
                    }
                };
                let reader = tokio::select! {
                    r = open => r?,
                    _ = cancel_wait(&ctl) => return Err("cancelled".into()),
                }
                .for_download();
                let (tx, rx) = tokio::sync::oneshot::channel();
                let (c2, l2) = (ctl.clone(), self.limiter.clone());
                std::thread::Builder::new()
                    .name("jizz-save".into())
                    .spawn(move || {
                        let r =
                            super::segmented::save_segmented(reader, &dir, &stem, &c2, &l2, &free);
                        let _ = tx.send(r);
                    })
                    .map_err(|e| e.to_string())?;
                rx.await.map_err(|_| "cancelled".to_string())?
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_extra(
        &self,
        format: &Format,
        meta: &Meta,
        page: &str,
        via: Via,
        entry: usize,
        index: usize,
        quality: Quality,
        dir: &Option<PathBuf>,
    ) {
        self.add(DownloadRequest {
            source: SaveSource::Picked {
                format: format.clone(),
                meta: meta.clone(),
                page: Some(page.to_owned()),
                via,
                entry,
                index,
            },
            quality,
            dir: dir.clone(),
            all_entries: false,
        });
    }

    /// Ask the page again for the same file: an expired address is replaced.
    fn renewer(self: &Arc<Self>, page: String, entry: usize, index: usize) -> Renew {
        let me = self.clone();
        Arc::new(move || {
            let (tx, rx) = channel();
            let (cfg, p) = (me.cfg(), page.clone());
            me.handle.spawn(async move {
                let r = extract_task(cfg, p, ExtractMode::Auto)
                    .await
                    .ok()
                    .and_then(|x| x.entries.into_iter().nth(entry))
                    .and_then(|e| muxed(e.formats).ok())
                    .and_then(|v| v.into_iter().nth(index))
                    .map(|f| f.url)
                    .ok_or_else(|| "expired".to_string());
                let _ = tx.send(r);
            });
            rx
        })
    }
}

/// The format to save from one entry and its position in the muxed list.
fn best_of(e: &Entry, q: Quality) -> Option<(Format, usize)> {
    let fmts = muxed(e.formats.clone()).ok()?;
    let i = pick_format(&fmts, q);
    fmts.into_iter().nth(i).map(|f| (f, i))
}

impl FetchService {
    /// Queue a download. Nothing connects before the scheduler starts it.
    pub fn download_start(&self, req: DownloadRequest) -> Result<Vec<JobId>, String> {
        self.downloads.start(req)
    }

    pub fn download_pause(&self, id: JobId) -> bool {
        self.downloads.pause(id)
    }

    pub fn download_resume(&self, id: JobId) -> bool {
        self.downloads.resume(id)
    }

    pub fn download_cancel(&self, id: JobId) -> bool {
        self.downloads.cancel(id)
    }

    /// Remove a finished job from the list. Never deletes the saved file.
    pub fn download_forget(&self, id: JobId) -> bool {
        self.downloads.forget(id)
    }

    pub fn download_list(&self) -> Vec<JobView> {
        self.downloads.list()
    }

    /// What ended since the last call; empty while anything is still going.
    pub fn download_take_finished(&self) -> Finished {
        self.downloads.take_finished()
    }

    /// Tests only: replace the free-space probe.
    #[doc(hidden)]
    #[must_use]
    pub fn with_free_space(self, f: FreeSpace) -> Self {
        self.downloads.set_free_space(f);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(h: Option<u32>, proto: &str) -> Format {
        Format {
            url: "x".into(),
            height: h,
            protocol: proto.into(),
            headers: vec![],
            audio_only: false,
            video_only: false,
            bitrate: None,
        }
    }

    #[test]
    fn quality_picks_the_tallest_that_fits() {
        let v = [
            f(Some(1080), "http"),
            f(Some(720), "http"),
            f(Some(360), "http"),
        ];
        let q = |max, prefer| Quality {
            max_height: max,
            prefer,
        };
        assert_eq!(pick_format(&v, q(None, Prefer::Best)), 0);
        assert_eq!(pick_format(&v, q(Some(720), Prefer::Best)), 1);
        assert_eq!(pick_format(&v, q(Some(500), Prefer::Best)), 2);
        // Nothing fits: the best there is.
        assert_eq!(pick_format(&v, q(Some(100), Prefer::Best)), 0);
        assert_eq!(pick_format(&v, q(None, Prefer::Smallest)), 2);
        // Unknown heights are taken when nothing known fits.
        let u = [f(None, "hls"), f(Some(1080), "http")];
        assert_eq!(pick_format(&u, q(Some(480), Prefer::Best)), 0);
    }
}
