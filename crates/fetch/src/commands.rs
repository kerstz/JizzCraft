//! `fetch.*` commands for the shared registry. Only built with `network`.
//!
//! Indexer keys never appear in a result. Download links from an indexer can
//! carry the key, so search results expose only the magnet (when there is
//! one) and `has_link`; the link itself stays inside the host and is used by
//! `fetch.add {result}` / `fetch.grab`.

use crate::config::{FetchConfig, Indexer};
use crate::redact::redact_url;
use crate::secret::Secret;
use crate::service::{FetchService, Source, TorrentState};
use crate::torznab::SearchResult;
use jizzcraft_core::commands::{HasLibrary, Registry};
use jizzcraft_core::{Error, Result, scan};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub struct FetchHost {
    cfg_path: PathBuf,
    cfg: FetchConfig,
    svc: Option<FetchService>,
    last: Vec<SearchResult>,
    pub cancel: Arc<AtomicBool>,
}

impl FetchHost {
    pub fn new(cfg_path: PathBuf) -> Self {
        let cfg = FetchConfig::load(&cfg_path);
        Self {
            cfg_path,
            cfg,
            svc: None,
            last: Vec::new(),
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

pub fn register_fetch<C: HasFetch>(reg: &mut Registry<C>) {
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
        "Advanced: {dir?, down_mibps?, up_mibps?, seed?, dht?, proxy?: socks5://…, require_proxy?, resume_on_start?: never|ask|always, seed_ratio?, seed_minutes?, bind_interface?}",
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
