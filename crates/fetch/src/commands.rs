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
        Self { cfg_path, cfg, svc: None, last: Vec::new(), cancel: Arc::default() }
    }

    pub fn config(&self) -> &FetchConfig {
        &self.cfg
    }

    fn save(&mut self) -> Result<()> {
        self.cfg.save(&self.cfg_path)?;
        // Next use rebuilds the service with the new config.
        self.svc = None;
        Ok(())
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
        .map(|v| v.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect())
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
    reg.register("fetch.indexers.list", "Configured indexers (keys are never shown)", |c, _| {
        let v: Vec<Value> = c
            .fetch()
            .config()
            .indexers
            .iter()
            .map(|i| json!({ "id": i.id, "name": i.name, "url": redact_url(&i.url),
                             "has_key": !i.api_key.is_empty(), "enabled": i.enabled }))
            .collect();
        Ok(json!({ "indexers": v }))
    });
    reg.register(
        "fetch.indexers.add",
        "Add a Torznab indexer: {name, url, api_key}. Nothing is bundled.",
        |c, a| {
            let h = c.fetch();
            let id = h.cfg.next_indexer_id();
            h.cfg.indexers.push(Indexer {
                id,
                name: s(&a, "name")?,
                url: s(&a, "url")?,
                api_key: Secret::new(s(&a, "api_key")?),
                enabled: true,
            });
            h.save()?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register("fetch.indexers.remove", "Remove an indexer: {id}", |c, a| {
        let id = u(&a, "id").ok_or_else(|| Error::Invalid("missing `id`".into()))? as u32;
        let h = c.fetch();
        let n = h.cfg.indexers.len();
        h.cfg.indexers.retain(|i| i.id != id);
        if h.cfg.indexers.len() == n {
            return Err(Error::NotFound(format!("indexer {id}")));
        }
        h.save()?;
        Ok(json!({}))
    });
    reg.register("fetch.indexers.test", "Check endpoint and key: {id}", |c, a| {
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
    });
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
    reg.register("fetch.search.cancel", "Cancel the running search", |c, _| {
        c.fetch().cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(json!({}))
    });
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
                return Err(Error::Invalid("give `magnet`, `torrent_path` or `result`".into()));
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
            .map(|t| json!({ "id": t.id, "progress": t.progress(), "state": format!("{:?}", t.state),
                             "down_bps": t.down_bps, "up_bps": t.up_bps, "eta_secs": t.eta_secs,
                             "total_bytes": t.total_bytes }))
            .collect();
        Ok(json!({ "torrents": v }))
    });
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
