//! `fetch.*` commands for the shared registry. Only built with `network`.
//!
//! Indexer keys never appear in a result. Download links from an indexer can
//! carry the key, so search results expose only the magnet (when there is
//! one) and `has_link`; the link itself stays inside the host and is used by
//! `fetch.add {result}` / `fetch.grab`.

use crate::config::{FetchConfig, Indexer};
use crate::download::manager::{DownloadRequest, SaveSource, pick_format};
use crate::download::{JobId, JobState, JobView};
use crate::errors::classify;
use crate::formats::{MAX_ENTRIES, muxed};
use crate::pick::{Prefer, Quality};
use crate::redact::redact_url;
use crate::secret::Secret;
use crate::service::{ExtractMode, Extraction, FetchService, Source, TorrentState, Via};
use crate::torznab::SearchResult;
use jizzcraft_core::commands::{HasLibrary, Registry};
use jizzcraft_core::{Error, Result, scan};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub struct FetchHost {
    cfg_path: PathBuf,
    cfg: FetchConfig,
    svc: Option<FetchService>,
    last: Vec<SearchResult>,
    /// The latest page readings, newest last, for `download.start {token}`.
    /// Memory only: the addresses in them never reach a result.
    extractions: VecDeque<(u64, Extraction)>,
    pub cancel: Arc<AtomicBool>,
}

/// How many page readings are kept for a later `download.start`.
const KEEP_EXTRACTIONS: usize = 16;

impl FetchHost {
    pub fn new(cfg_path: PathBuf) -> Self {
        let cfg = FetchConfig::load(&cfg_path);
        Self {
            cfg_path,
            cfg,
            svc: None,
            last: Vec::new(),
            extractions: VecDeque::new(),
            cancel: Arc::default(),
        }
    }

    pub fn config(&self) -> &FetchConfig {
        &self.cfg
    }

    fn save(&mut self) -> Result<()> {
        self.cfg.save(&self.cfg_path)?;
        // Keep the running service (and its torrents); hand it the new config.
        if let Some(svc) = self.svc.as_mut() {
            svc.set_config(self.cfg.clone());
        }
        Ok(())
    }

    /// True once the service exists (a runtime, not yet necessarily a session).
    pub fn host_started(&self) -> bool {
        self.svc.is_some()
    }

    pub fn add_indexer(&mut self, name: String, url: String, api_key: String) -> Result<u32> {
        let id = self.cfg.next_indexer_id();
        // One field is enough: the name defaults to the host of the URL.
        // Naming things is hard; we delegated it to DNS.
        let name = if name.trim().is_empty() {
            url.split("://")
                .nth(1)
                .unwrap_or(&url)
                .split(['/', ':'])
                .next()
                .unwrap_or("indexer")
                .to_owned()
        } else {
            name
        };
        self.cfg.indexers.push(Indexer {
            id,
            name,
            url,
            api_key: Secret::new(api_key),
            enabled: true,
        });
        self.save()?;
        Ok(id)
    }

    /// Add a source from a definition the user wrote. Secrets are never read
    /// from the definition: set them with [`Self::set_source_secret`].
    pub fn import_source(&mut self, json: &[u8]) -> Result<u32> {
        let mut d = crate::sources::parse_import(json).map_err(|e| Error::Invalid(e.into()))?;
        d.id = self
            .cfg
            .sources
            .iter()
            .map(|s| s.id)
            .max()
            .map_or(1, |m| m + 1);
        let id = d.id;
        self.cfg.sources.push(d);
        self.save()?;
        Ok(id)
    }

    pub fn add_resolver(&mut self, mut d: crate::resolver::ResolverDef) -> Result<u32> {
        crate::resolver::validate(&d).map_err(|e| Error::Invalid(e.into()))?;
        d.id = self
            .cfg
            .resolvers
            .iter()
            .map(|r| r.id)
            .max()
            .map_or(1, |m| m + 1);
        let id = d.id;
        self.cfg.resolvers.push(d);
        self.save()?;
        Ok(id)
    }

    pub fn remove_resolver(&mut self, id: u32) -> Result<()> {
        self.cfg.resolvers.retain(|r| r.id != id);
        self.save()
    }

    /// Add extractor rules from a file the user wrote (one rule or a list).
    /// Returns the new ids. Nothing is added when any rule is refused.
    pub fn import_rules(&mut self, bytes: &[u8]) -> Result<Vec<u32>> {
        use crate::extract::rules;
        let mut new = rules::parse_import(bytes).map_err(|e| Error::Invalid(e.into()))?;
        if self.cfg.extract_rules.len() + new.len() > rules::MAX_RULES {
            return Err(Error::Invalid("there are too many rules".into()));
        }
        let first = self
            .cfg
            .extract_rules
            .iter()
            .map(|r| r.id)
            .max()
            .map_or(1, |m| m + 1);
        let mut ids = Vec::with_capacity(new.len());
        for (r, id) in new.iter_mut().zip(first..) {
            r.id = id;
            ids.push(id);
        }
        self.cfg.extract_rules.extend(new);
        self.save()?;
        Ok(ids)
    }

    pub fn remove_rule(&mut self, id: u32) -> Result<()> {
        self.cfg.extract_rules.retain(|r| r.id != id);
        self.save()
    }

    pub fn set_rule_enabled(&mut self, id: u32, on: bool) -> Result<()> {
        if let Some(r) = self.cfg.extract_rules.iter_mut().find(|r| r.id == id) {
            r.enabled = on;
        }
        self.save()
    }

    pub fn remove_source(&mut self, id: u32) -> Result<()> {
        self.cfg.sources.retain(|s| s.id != id);
        self.save()
    }

    pub fn set_source_enabled(&mut self, id: u32, on: bool) -> Result<()> {
        if let Some(s) = self.cfg.sources.iter_mut().find(|s| s.id == id) {
            s.enabled = on;
        }
        self.save()
    }

    pub fn set_source_secret(&mut self, id: u32, value: String) -> Result<()> {
        let s = self
            .cfg
            .sources
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| Error::Invalid("no such source".into()))?;
        s.secret = Secret::new(value);
        self.save()
    }

    pub fn remove_indexer(&mut self, id: u32) -> Result<()> {
        self.cfg.indexers.retain(|i| i.id != id);
        self.save()
    }

    /// Neutral names (download folder `Viewer`) while the window is disguised.
    pub fn set_disguised(&mut self, on: bool) {
        self.cfg.disguised = on;
        if let Some(svc) = self.svc.as_mut() {
            svc.set_config(self.cfg.clone());
        }
    }

    /// Edit the advanced settings in one go and save.
    pub fn update_settings(&mut self, f: impl FnOnce(&mut FetchConfig)) -> Result<()> {
        let before = (
            self.cfg.require_proxy,
            self.cfg.proxy.clone(),
            self.cfg.bind_interface.clone(),
        );
        f(&mut self.cfg);
        // A running session keeps its network options, so a change to the
        // proxy, the kill switch or the bound interface ends it: nothing
        // keeps a direct connection.
        if before
            != (
                self.cfg.require_proxy,
                self.cfg.proxy.clone(),
                self.cfg.bind_interface.clone(),
            )
        {
            self.svc = None;
        }
        self.save()
    }

    /// Apply a `fetch.settings` argument object. Absent keys are untouched.
    /// Keys: `dir`, `down_mibps`, `up_mibps` (0 = unlimited), `seed`, `dht`,
    /// `proxy` (`socks5://...`, empty = none), `require_proxy`,
    /// `save_dir` (absolute, empty = the download folder, created),
    /// `save_concurrency` (1 to 4), `import_after_save`,
    /// `extractor` (`first|fallback|off`), `follow_embeds`,
    /// `page_user_agent` (empty = the generic default),
    /// `resume_on_start` (`never|ask|always`), `seed_ratio`, `seed_minutes`
    /// (0 = off), `bind_interface` (empty = none).
    pub fn apply_settings(&mut self, a: &Value) -> Result<()> {
        let num = |k: &str| a.get(k).and_then(Value::as_u64).map(|n| n as u32);
        let text = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
        let flag = |k: &str| a.get(k).and_then(Value::as_bool);
        // Blanks around the address (a paste, the CLI) are not part of it;
        // blanks only mean "no proxy".
        let proxy = text("proxy").map(|p| p.trim().to_owned());
        if let Some(p) = &proxy
            && !p.is_empty()
            && !p.starts_with("socks5://")
        {
            return Err(Error::Invalid("the proxy must start with socks5://".into()));
        }
        let resume = match text("resume_on_start") {
            Some(r) => Some(crate::config::ResumeOnStart::parse(&r).ok_or_else(|| {
                Error::Invalid("resume_on_start must be never, ask or always".into())
            })?),
            None => None,
        };
        let save_dir = match text("save_dir") {
            Some(p) if !p.is_empty() => {
                let p = PathBuf::from(p);
                if !p.is_absolute() {
                    return Err(Error::Invalid(
                        "the save folder must be an absolute path".into(),
                    ));
                }
                std::fs::create_dir_all(&p)
                    .map_err(|_| Error::Invalid("the save folder could not be created".into()))?;
                Some(Some(p))
            }
            Some(_) => Some(None),
            None => None,
        };
        let concurrency = match a.get("save_concurrency") {
            None => None,
            Some(v) => match v.as_u64() {
                Some(n @ 1..=4) => Some(n as u8),
                _ => return Err(Error::Invalid("save_concurrency must be 1 to 4".into())),
            },
        };
        let extractor = match text("extractor") {
            Some(e) => Some(crate::config::ExtractorMode::parse(&e).ok_or_else(|| {
                Error::Invalid("extractor must be first, fallback or off".into())
            })?),
            None => None,
        };
        let agent = match text("page_user_agent") {
            Some(u) => {
                let u = u.trim().to_owned();
                if u.len() > 256 || !u.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                    return Err(Error::Invalid(
                        "page_user_agent must be short printable text".into(),
                    ));
                }
                Some((!u.is_empty()).then_some(u))
            }
            None => None,
        };
        let ratio = a.get("seed_ratio").and_then(Value::as_f64);
        if let Some(r) = ratio
            && (!r.is_finite() || r < 0.0)
        {
            return Err(Error::Invalid("seed_ratio must be 0 or more".into()));
        }
        self.update_settings(|c| {
            if let Some(p) = text("dir") {
                c.download_dir = (!p.is_empty()).then(|| p.into());
            }
            if a.get("down_mibps").is_some() {
                c.down_limit_mibps = num("down_mibps").filter(|n| *n > 0);
            }
            if a.get("up_mibps").is_some() {
                c.up_limit_mibps = num("up_mibps").filter(|n| *n > 0);
            }
            if let Some(p) = proxy {
                c.proxy = (!p.is_empty()).then(|| Secret::new(p));
            }
            if let Some(r) = flag("require_proxy") {
                c.require_proxy = r;
            }
            if let Some(s) = flag("seed") {
                c.seed_after_complete = s;
            }
            if let Some(d) = flag("dht") {
                c.dht = d;
            }
            if let Some(r) = resume {
                c.resume_on_start = r;
            }
            if a.get("seed_ratio").is_some() {
                c.seed_ratio = ratio.filter(|r| *r > 0.0).map(|r| r as f32);
            }
            if a.get("seed_minutes").is_some() {
                c.seed_minutes = num("seed_minutes").filter(|n| *n > 0);
            }
            if let Some(b) = text("bind_interface") {
                let b = b.trim().to_owned();
                c.bind_interface = (!b.is_empty()).then_some(b);
            }
            if let Some(d) = save_dir {
                c.save.dir = d;
            }
            if let Some(n) = concurrency {
                c.save.concurrency = n;
            }
            if let Some(i) = flag("import_after_save") {
                c.save.import_after = i;
            }
            if let Some(e) = extractor {
                c.save.extractor = e;
            }
            if let Some(f) = flag("follow_embeds") {
                c.save.follow_embeds = f;
            }
            if let Some(u) = agent {
                c.save.page_user_agent = u;
            }
        })
    }

    /// Resume every paused, unfinished torrent. Yields how many resumed.
    pub fn resume_all(&mut self) -> Result<usize> {
        let svc = self.service()?;
        let _ = svc.start().recv_timeout(Duration::from_secs(30));
        svc.resume_all()
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Error::Invalid("resume timed out".into()))
    }

    pub fn set_download_dir(&mut self, dir: PathBuf) -> Result<()> {
        std::fs::create_dir_all(&dir)?;
        self.cfg.download_dir = Some(dir);
        self.save()
    }

    /// Open any address (manifest, file or page) with the user's resolvers.
    /// The receiver yields a `StreamTicket`; `Timed` carries HLS/DASH.
    pub fn open_any(
        &mut self,
        url: String,
        opts: crate::service::StreamOpts,
    ) -> Result<std::sync::mpsc::Receiver<std::result::Result<crate::service::StreamTicket, String>>>
    {
        Ok(self.service()?.open_any(url, opts))
    }

    /// Cancel every stream that is still opening. No-op before first use.
    pub fn cancel_streams(&self) {
        if let Some(s) = &self.svc {
            s.cancel_streams();
        }
    }

    /// Remember a page reading and hand back its token.
    pub fn keep_extraction(&mut self, ex: Extraction) -> u64 {
        use std::hash::{BuildHasher, Hasher};
        let mut token;
        loop {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
            );
            token = h.finish();
            if token != 0 && !self.extractions.iter().any(|(t, _)| *t == token) {
                break;
            }
        }
        self.extractions.push_back((token, ex));
        while self.extractions.len() > KEEP_EXTRACTIONS {
            self.extractions.pop_front();
        }
        token
    }

    fn extraction(&self, token: u64) -> Result<Extraction> {
        self.extractions
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, e)| e.clone())
            .ok_or_else(|| Error::NotFound("extraction".into()))
    }

    /// The service when it already exists (no runtime is started to answer).
    fn running(&self) -> Option<&FetchService> {
        self.svc.as_ref()
    }

    pub fn service(&mut self) -> Result<&FetchService> {
        if self.svc.is_none() {
            let st = jizzcraft_core::paths::state_dir().join("fetch");
            self.svc = Some(FetchService::new(self.cfg.clone(), st)?);
        }
        Ok(self.svc.as_ref().expect("just set"))
    }
}

pub trait HasFetch: HasLibrary {
    fn fetch(&mut self) -> &mut FetchHost;
}

fn s(a: &Value, k: &str) -> Result<String> {
    a.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("missing string argument `{k}`")))
}

fn u(a: &Value, k: &str) -> Option<u64> {
    a.get(k).and_then(Value::as_u64)
}

fn public(r: &SearchResult, i: usize) -> Value {
    json!({
        "index": i, "title": r.title, "size": r.size, "seeders": r.seeders,
        "peers": r.peers, "magnet": r.magnet, "has_link": r.link.is_some(),
        "indexer": r.indexer_id, "published": r.pub_date,
    })
}

fn do_search(h: &mut FetchHost, a: &Value) -> Result<Vec<SearchResult>> {
    let q = s(a, "query")?;
    let cats: Vec<u32> = a
        .get("cats")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();
    let limit = u(a, "limit").unwrap_or(50).min(200) as u32;
    let ix = u(a, "indexer").map(|n| n as u32);
    h.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    let cancel = h.cancel.clone();
    let rx = h.service()?.search(ix, &q, &cats, limit, cancel);
    let out = rx
        .recv_timeout(Duration::from_secs(60))
        .map_err(|_| Error::Invalid("search timed out".into()))?;
    if out.queried == 0 {
        return Err(Error::Invalid("no enabled indexer configured".into()));
    }
    h.last = out.results.clone();
    Ok(out.results)
}

fn source_of(r: &SearchResult) -> Result<Source> {
    match (&r.magnet, &r.link) {
        (Some(m), _) => Ok(Source::Magnet(m.clone())),
        (None, Some(l)) => Ok(Source::TorrentUrl(l.clone())),
        _ => Err(Error::Invalid("result has no magnet or link".into())),
    }
}

fn add_and_maybe_wait(h: &mut FetchHost, src: Source, a: &Value) -> Result<Value> {
    let wait = u(a, "wait_secs").unwrap_or(0);
    let svc = h.service()?;
    let id = svc
        .add(src)
        .recv_timeout(Duration::from_secs(90))
        .map_err(|_| Error::Invalid("add timed out".into()))?
        .map_err(Error::Invalid)?;
    if wait > 0 {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(wait) {
            std::thread::sleep(Duration::from_millis(500));
            if let Some(t) = svc.list().iter().find(|t| t.id == id)
                && (t.progress() >= 1.0 || t.state == TorrentState::Error)
            {
                break;
            }
        }
    }
    Ok(json!({ "id": id }))
}

/// Add the files a batch of saves produced to the library: a rescan of the
/// library folder that holds `dir`, or a new library folder when none does.
/// Counts only.
pub fn import_saved(db: &mut jizzcraft_core::Db, dir: &Path) -> Result<scan::ScanReport> {
    let canon = jizzcraft_core::pathbytes::canonical(dir)
        .map_err(|_| Error::Invalid("the save folder is not available".into()))?;
    let roots = db.folders()?;
    if let Some((id, root)) = roots.iter().find(|(_, r)| canon.starts_with(r)) {
        return scan::scan_folder(db, *id, root);
    }
    // A folder above an existing library folder would nest two roots.
    if roots.iter().any(|(_, r)| r.starts_with(&canon)) {
        return Err(Error::Invalid(
            "the save folder holds library folders".into(),
        ));
    }
    scan::import_folder(db, &canon).map(|(_, r)| r)
}

fn quality_of(a: &Value) -> Result<Quality> {
    let bad = || Error::Invalid("quality must be best, worst or a height".into());
    match a.get("quality") {
        None | Some(Value::Null) => Ok(Quality::default()),
        Some(Value::String(q)) => match q.as_str() {
            "best" => Ok(Quality::default()),
            "worst" => Ok(Quality {
                max_height: None,
                prefer: Prefer::Smallest,
            }),
            n => n
                .trim_end_matches('p')
                .parse::<u32>()
                .ok()
                .filter(|h| (1..=8640).contains(h))
                .map(|h| Quality {
                    max_height: Some(h),
                    prefer: Prefer::Best,
                })
                .ok_or_else(bad),
        },
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|h| (1..=8640).contains(h))
            .map(|h| Quality {
                max_height: Some(h as u32),
                prefer: Prefer::Best,
            })
            .ok_or_else(bad),
        _ => Err(bad()),
    }
}

fn dir_of(a: &Value) -> Result<Option<PathBuf>> {
    match a.get("dir").and_then(Value::as_str) {
        None | Some("") => Ok(None),
        Some(d) => {
            let p = PathBuf::from(d);
            if p.is_absolute() && p.is_dir() {
                Ok(Some(p))
            } else {
                Err(Error::Invalid(
                    "the folder must be an absolute path to an existing folder".into(),
                ))
            }
        }
    }
}

fn state_name(s: JobState) -> &'static str {
    match s {
        JobState::Queued => "queued",
        JobState::Probing => "probing",
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Verifying => "verifying",
        JobState::Done => "done",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

/// A job as the commands show it: numbers and a fixed sentence. No address,
/// no title, no path.
fn job_json(j: &JobView) -> Value {
    json!({
        "id": j.id,
        "state": state_name(j.state),
        "kind": match j.kind {
            crate::download::JobKind::Direct => "direct",
            crate::download::JobKind::Hls => "hls",
            crate::download::JobKind::Dash => "dash",
        },
        "done_bytes": j.done_bytes, "total_bytes": j.total_bytes,
        "fraction": j.fraction, "bps": j.bps, "eta_secs": j.eta_secs,
        "height": j.height, "resumable": j.resumable, "error": j.error,
    })
}

fn download_start(h: &mut FetchHost, a: &Value) -> Result<Value> {
    let quality = quality_of(a)?;
    let dir = dir_of(a)?;
    let all = a.get("all").and_then(Value::as_bool).unwrap_or(false);
    let start = |svc: &FetchService, req: DownloadRequest| -> Result<Vec<JobId>> {
        svc.download_start(req)
            .map_err(|e| Error::Invalid(classify(&e).to_owned()))
    };
    if let Some(url) = a.get("url").and_then(Value::as_str) {
        let mut req = DownloadRequest::url(url);
        req.quality = quality;
        req.dir = dir;
        req.all_entries = all;
        let ids = start(h.service()?, req)?;
        return Ok(json!({ "ids": ids }));
    }
    let token = u(a, "token").ok_or_else(|| Error::Invalid("give `url` or `token`".into()))?;
    let ex = h.extraction(token)?;
    let entry = u(a, "entry").unwrap_or(0) as usize;
    let format = u(a, "format").map(|f| f as usize);
    let which: Vec<usize> = if all {
        (0..ex.entries.len().min(MAX_ENTRIES)).collect()
    } else {
        vec![entry]
    };
    let mut reqs = Vec::new();
    for i in which {
        let e = ex
            .entries
            .get(i)
            .ok_or_else(|| Error::NotFound(format!("entry {i}")))?;
        let Ok(fmts) = muxed(e.formats.clone()) else {
            if all {
                continue;
            }
            return Err(Error::NotFound("format".into()));
        };
        let idx = match (format, all) {
            (Some(f), false) => f,
            _ => pick_format(&fmts, quality),
        };
        let f = fmts
            .get(idx)
            .ok_or_else(|| Error::NotFound(format!("format {idx}")))?;
        reqs.push(DownloadRequest {
            source: SaveSource::Picked {
                format: f.clone(),
                meta: e.meta.clone(),
                page: Some(ex.page.clone()),
                via: ex.via,
                entry: i,
                index: idx,
            },
            quality,
            dir: dir.clone(),
            all_entries: false,
        });
    }
    let svc = h.service()?;
    let mut ids = Vec::new();
    for r in reqs {
        ids.extend(start(svc, r)?);
    }
    Ok(json!({ "ids": ids }))
}

fn job_id(a: &Value) -> Result<JobId> {
    u(a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))
}

fn on_job(
    c: &mut impl HasFetch,
    a: &Value,
    f: impl Fn(&FetchService, JobId) -> bool,
) -> Result<Value> {
    let id = job_id(a)?;
    let ok = c.fetch().running().is_some_and(|s| f(s, id));
    if ok {
        Ok(json!({}))
    } else {
        Err(Error::NotFound(format!("job {id}")))
    }
}

/// Saving and extractor rules: everything that never blocks.
pub fn register_download<C: HasFetch>(reg: &mut Registry<C>) {
    reg.register(
        "download.start",
        "Save a video: {url} or {token, entry?, format?} from extract.page; quality?: best|worst|<height>, dir?: absolute existing folder, all?",
        |c, a| download_start(c.fetch(), &a),
    );
    reg.register("download.pause", "Pause a download: {id}", |c, a| {
        on_job(c, &a, |s, id| s.download_pause(id))
    });
    reg.register("download.resume", "Resume a download: {id}", |c, a| {
        on_job(c, &a, |s, id| s.download_resume(id))
    });
    reg.register("download.cancel", "Cancel a download: {id}", |c, a| {
        on_job(c, &a, |s, id| s.download_cancel(id))
    });
    reg.register(
        "download.remove",
        "Forget a finished download: {id}. The saved file is never deleted.",
        |c, a| on_job(c, &a, |s, id| s.download_forget(id)),
    );
    reg.register(
        "download.list",
        "Downloads and their progress (no address, title or path)",
        |c, _| {
            let jobs: Vec<JobView> = c
                .fetch()
                .running()
                .map(FetchService::download_list)
                .unwrap_or_default();
            let running = jobs.iter().filter(|j| j.state.holds_slot()).count();
            let queued = jobs.iter().filter(|j| j.state == JobState::Queued).count();
            let v: Vec<Value> = jobs.iter().map(job_json).collect();
            Ok(json!({ "jobs": v, "running": running, "queued": queued }))
        },
    );
    reg.register(
        "extract.rules.import",
        "Add extractor rules from a JSON file you wrote: {path} (up to 64 KiB). Nothing is fetched.",
        |c, a| {
            let p = PathBuf::from(s(&a, "path")?);
            let len = std::fs::metadata(&p)?.len();
            if len > crate::extract::rules::MAX_FILE as u64 {
                return Err(Error::Invalid("the rules file is too large".into()));
            }
            let bytes = std::fs::read(&p)?;
            let ids = c.fetch().import_rules(&bytes)?;
            Ok(json!({ "ids": ids }))
        },
    );
    reg.register(
        "extract.rules.list",
        "Extractor rules (names and counts; patterns are never shown)",
        |c, _| {
            let v: Vec<Value> = c
                .fetch()
                .config()
                .extract_rules
                .iter()
                .map(|r| json!({ "id": r.id, "name": r.name, "enabled": r.enabled, "hosts": r.hosts.len() }))
                .collect();
            Ok(json!({ "rules": v }))
        },
    );
    reg.register("extract.rules.remove", "Remove a rule: {id}", |c, a| {
        let id = job_id(&a)? as u32;
        if !c.fetch().config().extract_rules.iter().any(|r| r.id == id) {
            return Err(Error::NotFound(format!("rule {id}")));
        }
        c.fetch().remove_rule(id)?;
        Ok(json!({}))
    });
    reg.register(
        "extract.rules.enable",
        "Turn a rule on or off: {id, on}",
        |c, a| {
            let id = job_id(&a)? as u32;
            let on = a
                .get("on")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::Invalid("missing `on`".into()))?;
            if !c.fetch().config().extract_rules.iter().any(|r| r.id == id) {
                return Err(Error::NotFound(format!("rule {id}")));
            }
            c.fetch().set_rule_enabled(id, on)?;
            Ok(json!({}))
        },
    );
}

fn via_name(v: Via) -> &'static str {
    match v {
        Via::Direct => "direct",
        Via::Builtin { embed: false } => "builtin",
        Via::Builtin { embed: true } => "embed",
        Via::Resolver(_) => "resolver",
    }
}

fn extract_page(h: &mut FetchHost, a: &Value) -> Result<Value> {
    let url = s(a, "url")?;
    let mode = match a.get("mode").and_then(Value::as_str) {
        None | Some("auto") => ExtractMode::Auto,
        Some("builtin") => ExtractMode::Builtin,
        Some("resolvers") => ExtractMode::Resolvers,
        Some(_) => {
            return Err(Error::Invalid(
                "mode must be auto, builtin or resolvers".into(),
            ));
        }
    };
    let show = a.get("show_urls").and_then(Value::as_bool).unwrap_or(false);
    let ex = h
        .service()?
        .extract(url, mode)
        .recv_timeout(Duration::from_secs(90))
        .map_err(|_| Error::Invalid(crate::errors::TIMEOUT.into()))?
        .map_err(|e| Error::Invalid(classify(&e).to_owned()))?;
    let mut entries = Vec::new();
    for (i, e) in ex.entries.iter().enumerate() {
        let Ok(fmts) = muxed(e.formats.clone()) else {
            continue;
        };
        let formats: Vec<Value> = fmts
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let mut o = json!({
                    "index": k, "height": f.height, "protocol": f.protocol,
                    "bitrate": f.bitrate,
                });
                if show {
                    o["url"] = json!(f.url);
                }
                o
            })
            .collect();
        entries.push(json!({ "index": i, "title": e.meta.title, "formats": formats }));
    }
    let first = ex.entries.first();
    let out = json!({
        "via": via_name(ex.via),
        "title": first.and_then(|e| e.meta.title.clone()),
        "duration": first.and_then(|e| e.meta.duration_secs),
        "has_thumbnail": first.is_some_and(|e| e.meta.thumbnail.is_some()),
        "entries": entries,
    });
    let token = h.keep_extraction(ex);
    let mut out = out;
    out["token"] = json!(token);
    Ok(out)
}

fn download_wait(c: &mut impl HasFetch, a: &Value) -> Result<Value> {
    let secs = u(a, "timeout_secs").unwrap_or(3600).min(86_400);
    let ids: Option<Vec<JobId>> = a
        .get("ids")
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_u64).collect());
    let t0 = Instant::now();
    let jobs = loop {
        let jobs = c
            .fetch()
            .running()
            .map(FetchService::download_list)
            .unwrap_or_default();
        let mine: Vec<&JobView> = jobs
            .iter()
            .filter(|j| ids.as_ref().is_none_or(|v| v.contains(&j.id)))
            .collect();
        if mine.iter().all(|j| j.state.is_over()) {
            break jobs;
        }
        if t0.elapsed() >= Duration::from_secs(secs) {
            return Err(Error::Invalid("download.wait timed out".into()));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let count = |st: JobState| {
        jobs.iter()
            .filter(|j| j.state == st && ids.as_ref().is_none_or(|v| v.contains(&j.id)))
            .count()
    };
    let (done, failed) = (count(JobState::Done), count(JobState::Failed));
    // The batch is over when nothing else is going: one import per batch.
    let fin = c
        .fetch()
        .running()
        .map(FetchService::download_take_finished)
        .unwrap_or_default();
    let mut imported = 0;
    if c.fetch().config().save.import_after && fin.done > 0 {
        for d in &fin.dirs {
            if let Ok(r) = import_saved(c.db(), d) {
                imported += r.added;
            }
        }
    }
    Ok(json!({ "done": done, "failed": failed, "imported": imported }))
}

pub fn register_fetch<C: HasFetch>(reg: &mut Registry<C>) {
    register_download(reg);
    reg.register(
        "extract.page",
        "List the videos on a page: {url, mode?: auto|builtin|resolvers, show_urls?}. Returns a token for download.start.",
        |c, a| extract_page(c.fetch(), &a),
    );
    reg.register(
        "download.wait",
        "Wait for downloads to end: {ids?, timeout_secs?}. Imports the saved videos once the queue is idle.",
        |c, a| download_wait(c, &a),
    );
    reg.register(
        "fetch.indexers.list",
        "Configured indexers (keys are never shown)",
        |c, _| {
            let v: Vec<Value> = c
                .fetch()
                .config()
                .indexers
                .iter()
                .map(|i| {
                    json!({ "id": i.id, "name": i.name, "url": redact_url(&i.url),
                             "has_key": !i.api_key.is_empty(), "enabled": i.enabled })
                })
                .collect();
            Ok(json!({ "indexers": v }))
        },
    );
    reg.register(
        "fetch.indexers.add",
        "Add a Torznab indexer: {url, api_key, name?}. Nothing is bundled.",
        |c, a| {
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let id = c
                .fetch()
                .add_indexer(name, s(&a, "url")?, s(&a, "api_key")?)?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register(
        "fetch.indexers.remove",
        "Remove an indexer: {id}",
        |c, a| {
            let id = u(&a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))? as u32;
            let h = c.fetch();
            let n = h.cfg.indexers.len();
            h.cfg.indexers.retain(|i| i.id != id);
            if h.cfg.indexers.len() == n {
                return Err(Error::NotFound(format!("indexer {id}")));
            }
            h.save()?;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.indexers.test",
        "Check endpoint and key: {id}",
        |c, a| {
            let id = u(&a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))? as u32;
            let h = c.fetch();
            let ix = h
                .cfg
                .indexers
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("indexer {id}")))?;
            let r = h
                .service()?
                .test_indexer(&ix)
                .recv_timeout(Duration::from_secs(30))
                .map_err(|_| Error::Invalid("test timed out".into()))?;
            r.map_err(Error::Invalid)?;
            Ok(json!({ "ok": true }))
        },
    );
    reg.register(
        "fetch.indexers.detect",
        "Find a local Jackett/Prowlarr on 127.0.0.1 and add it (explicit, loopback only)",
        |c, _| {
            let h = c.fetch();
            let found = h
                .service()?
                .detect_local()
                .recv_timeout(Duration::from_secs(20))
                .map_err(|_| Error::Invalid("detection timed out".into()))?;
            let n = found.len();
            for d in found {
                if !h.cfg.indexers.iter().any(|i| i.url == d.url) {
                    h.add_indexer(d.name, d.url, d.api_key)?;
                }
            }
            Ok(json!({ "found": n }))
        },
    );
    reg.register(
        "fetch.dir.set",
        "Set the download folder and import it as a library folder: {path}",
        |c, a| {
            let p = PathBuf::from(s(&a, "path")?);
            std::fs::create_dir_all(&p)?;
            c.fetch().cfg.download_dir = Some(p.clone());
            c.fetch().save()?;
            let (folder_id, _) = scan::import_folder(c.db(), &p)?;
            Ok(json!({ "folder_id": folder_id }))
        },
    );
    reg.register(
        "fetch.search",
        "Search indexers: {query, cats?: [..], indexer?, limit?}",
        |c, a| {
            let r = do_search(c.fetch(), &a)?;
            let items: Vec<Value> = r.iter().enumerate().map(|(i, x)| public(x, i)).collect();
            Ok(json!({ "count": items.len(), "items": items }))
        },
    );
    reg.register(
        "fetch.search.cancel",
        "Cancel the running search",
        |c, _| {
            c.fetch()
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.add",
        "Add a torrent: {magnet | torrent_path | result: index of last search, wait_secs?}",
        |c, a| {
            let h = c.fetch();
            let src = if let Some(m) = a.get("magnet").and_then(Value::as_str) {
                Source::Magnet(m.to_owned())
            } else if let Some(p) = a.get("torrent_path").and_then(Value::as_str) {
                Source::TorrentFile(PathBuf::from(p))
            } else if let Some(i) = u(&a, "result") {
                let r = h
                    .last
                    .get(i as usize)
                    .ok_or_else(|| Error::NotFound(format!("result {i}")))?;
                source_of(r)?
            } else {
                return Err(Error::Invalid(
                    "give `magnet`, `torrent_path` or `result`".into(),
                ));
            };
            add_and_maybe_wait(h, src, &a)
        },
    );
    reg.register(
        "fetch.grab",
        "Search then add one result in a single call: {query, pick?: 0, indexer?, wait_secs?}",
        |c, a| {
            let h = c.fetch();
            let r = do_search(h, &a)?;
            let pick = u(&a, "pick").unwrap_or(0) as usize;
            let item = r
                .get(pick)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("result {pick}")))?;
            add_and_maybe_wait(h, source_of(&item)?, &a)
        },
    );
    reg.register("fetch.list", "Torrents and their progress", |c, _| {
        let svc = c.fetch().service()?;
        let _ = svc.start().recv_timeout(Duration::from_secs(30));
        svc.list();
        std::thread::sleep(Duration::from_millis(300));
        let v: Vec<Value> = svc
            .list()
            .iter()
            .map(|t| {
                json!({ "id": t.id, "progress": t.progress(), "state": format!("{:?}", t.state),
                             "error": t.error,
                             "down_bps": t.down_bps, "up_bps": t.up_bps, "eta_secs": t.eta_secs,
                             "total_bytes": t.total_bytes })
            })
            .collect();
        Ok(json!({ "torrents": v }))
    });
    reg.register(
        "fetch.settings",
        "Advanced: {dir?, down_mibps?, up_mibps?, seed?, dht?, proxy?: socks5://…, require_proxy?, resume_on_start?: never|ask|always, seed_ratio?, seed_minutes?, bind_interface?, save_dir?, save_concurrency?: 1-4, import_after_save?, extractor?: first|fallback|off, follow_embeds?, page_user_agent?}",
        |c, a| {
            c.fetch().apply_settings(&a)?;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.resume_all",
        "Resume every paused, unfinished torrent (after a restart, with resume_on_start ask)",
        |c, _| Ok(json!({ "resumed": c.fetch().resume_all()? })),
    );
    reg.register("fetch.pause", "Pause a torrent: {id}", |c, a| {
        let id = u(&a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))?;
        c.fetch().service()?.pause(id as usize);
        Ok(json!({}))
    });
    reg.register("fetch.resume", "Resume a torrent: {id}", |c, a| {
        let id = u(&a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))?;
        c.fetch().service()?.resume(id as usize);
        Ok(json!({}))
    });
    reg.register(
        "fetch.remove",
        "Stop tracking a torrent: {id}. Downloaded files are never deleted.",
        |c, a| {
            let id = u(&a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))?;
            c.fetch().service()?.forget(id as usize);
            Ok(json!({}))
        },
    );
}
