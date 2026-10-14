//! Downloads window (feature `network`).
//!
//! One box: type a search, or paste a magnet link or a `.torrent` path, then
//! press Play. Everything else (indexer setup, folder, speed caps) is folded
//! away. Nothing here blocks the UI thread: the service API is channel based
//! and every receiver is polled with `try_recv`. No query, title, URL or path
//! reaches a toast or a log; failures are mapped to fixed sentences by
//! `jizzcraft_fetch::errors::classify`.

use super::{JizzApp, ToastKind};
use crate::theme::{FAINT, MUTED};
use crate::widgets;
use egui::{ProgressBar, RichText, TextEdit};
use jizzcraft_core::Registry;
use jizzcraft_fetch::commands::FetchHost;
use jizzcraft_fetch::errors::classify;
use jizzcraft_fetch::service::{
    Listed, SearchOutcome, Source, StreamTicket, TorrentState, TorrentStatus,
};
use jizzcraft_fetch::{FetchConfig, SearchResult};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use super::play_flow::{
    Banner, FileList, FlowBackend, FlowEvent, P_LISTING, PlayFlow, Rx, banners, is_retryable,
    probe_line,
};
use jizzcraft_fetch::config::ResumeOnStart;
use jizzcraft_fetch::service::{ProbeState, TorrentProbe};
use std::sync::Arc;

/// "Press Play": list, maybe ask which file, add, open (see `play_flow`).
type Flow = PlayFlow<Listed, StreamTicket>;

impl FileList for Listed {
    fn files(&self) -> &[(String, u64)] {
        &self.files
    }
}

/// The real backend: the fetch service behind the host.
struct FetchBackend<'a> {
    host: &'a mut FetchHost,
}

/// A channel that already holds an error (the service could not start).
fn failed<T>(e: impl ToString) -> Rx<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(Err(e.to_string()));
    rx
}

impl FlowBackend for FetchBackend<'_> {
    type Listed = Listed;
    type Ticket = StreamTicket;
    fn files(&mut self, id: usize) -> Rx<Vec<(String, u64)>> {
        match self.host.service() {
            Ok(s) => s.torrent_files(id),
            Err(e) => failed(e),
        }
    }
    fn add(&mut self, l: Listed, file: usize) -> Rx<usize> {
        match self.host.service() {
            Ok(s) => s.add_listed(l, Some(vec![file])),
            Err(e) => failed(e),
        }
    }
    fn open(&mut self, id: usize, file: usize) -> Rx<StreamTicket> {
        match self.host.service() {
            Ok(s) => s.open_reader(id, file),
            Err(e) => failed(e),
        }
    }
    fn cancel(&mut self) {
        self.host.cancel_streams();
    }
}

pub(super) struct Downloads {
    pub(super) host: FetchHost,
    open: bool,
    input: String,
    results: Vec<SearchResult>,
    searching: Option<Receiver<SearchOutcome>>,
    adding: Vec<Receiver<Result<usize, String>>>,
    flow: Flow,
    torrents: Vec<TorrentStatus>,
    done: HashSet<usize>,
    last_poll: Option<Instant>,
    /// Ready to open once the poll has the whole picture (set by the flow).
    ready: Option<StreamTicket>,
    detecting: Option<Receiver<Vec<jizzcraft_fetch::detect::Detected>>>,
    /// Plain-HTTP downloads started with "Download" (errors only are shown).
    http_adding: Vec<Receiver<Result<StreamTicket, String>>>,
    /// The next ticket goes to an external player, not the built-in one.
    external: bool,
    inbox_checked: Option<Instant>,
    // setup form
    ix_url: String,
    ix_key: String,
    tested: Option<Receiver<Result<(), String>>>,
    // advanced
    dir: String,
    down_limit: String,
    up_limit: String,
    proxy: String,
    seed_ratio: String,
    seed_minutes: String,
    bind: String,
    /// Key being typed per source (never stored, cleared after Set).
    src_key: std::collections::HashMap<u32, String>,
    /// Live peer count of the magnet being resolved.
    probe: Option<Arc<TorrentProbe>>,
    /// The last Play command, in memory only, for `fetch.play.retry`.
    pub(super) last_play: Option<(&'static str, Value)>,
    /// A retryable failure (fixed sentence) shown with a Retry button.
    retry_note: Option<String>,
    load_error_dismissed: bool,
    ask_dismissed: bool,
}

impl Downloads {
    /// Drop every stream that is still opening. Magnets handed over by
    /// another launch wait for the user instead of opening by themselves.
    pub(super) fn cancel_pending(&mut self) {
        // Requests still being made stop too, not only their result.
        FlowBackend::cancel(&mut FetchBackend {
            host: &mut self.host,
        });
        self.flow = Flow::default();
        self.ready = None;
        self.external = false;
        self.inbox_checked = Some(Instant::now() + Duration::from_secs(3600));
    }

    pub(super) fn new(disguised: bool) -> Self {
        let mut host = FetchHost::new(FetchConfig::default_path());
        host.set_disguised(disguised);
        let c = host.config();
        let dir = c
            .download_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lim = |v: Option<u32>| v.map(|n| n.to_string()).unwrap_or_default();
        let (down_limit, up_limit) = (lim(c.down_limit_mibps), lim(c.up_limit_mibps));
        let seed_ratio = c.seed_ratio.map(|r| r.to_string()).unwrap_or_default();
        let seed_minutes = lim(c.seed_minutes);
        let bind = c.bind_interface.clone().unwrap_or_default();
        Self {
            host,
            open: false,
            input: String::new(),
            results: Vec::new(),
            searching: None,
            adding: Vec::new(),
            flow: Flow::default(),
            torrents: Vec::new(),
            done: HashSet::new(),
            last_poll: None,
            ready: None,
            detecting: None,
            http_adding: Vec::new(),
            external: false,
            inbox_checked: None,
            ix_url: String::new(),
            ix_key: String::new(),
            tested: None,
            dir,
            down_limit,
            up_limit,
            proxy: String::new(),
            seed_ratio,
            seed_minutes,
            bind,
            src_key: std::collections::HashMap::new(),
            probe: None,
            last_play: None,
            retry_note: None,
            load_error_dismissed: false,
            ask_dismissed: false,
        }
    }

    fn source_of(&self, r: &SearchResult) -> Option<Source> {
        match (&r.magnet, &r.link) {
            (Some(m), _) => Some(Source::Magnet(m.clone())),
            (None, Some(l)) => Some(Source::TorrentUrl(l.clone())),
            _ => None,
        }
    }
}

fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

/// What the single input box holds.
enum Typed {
    Magnet(String),
    Url(String),
    TorrentFile(String),
    /// A local `.json` source definition.
    Source(String),
    Search(String),
}

fn parse_input(s: &str) -> Option<Typed> {
    let t = s.trim().trim_matches('"');
    if t.is_empty() {
        None
    } else if t.starts_with("magnet:") {
        Some(Typed::Magnet(t.to_owned()))
    } else if t.starts_with("http://") || t.starts_with("https://") {
        Some(Typed::Url(t.to_owned()))
    } else if t.to_ascii_lowercase().ends_with(".torrent") {
        Some(Typed::TorrentFile(t.to_owned()))
    } else if t.to_ascii_lowercase().ends_with(".json")
        && (t.starts_with('/') || t.starts_with("~/") || t.starts_with("./"))
    {
        Some(Typed::Source(t.to_owned()))
    } else {
        Some(Typed::Search(t.to_owned()))
    }
}

fn is_torrent_url(u: &str) -> bool {
    u.split(['?', '#'])
        .next()
        .is_some_and(|p| p.to_ascii_lowercase().ends_with(".torrent"))
}

type Res = jizzcraft_core::Result<Value>;
fn invalid(msg: &str) -> jizzcraft_core::Error {
    jizzcraft_core::Error::Invalid(msg.into())
}

fn start_search(app: &mut JizzApp, q: String) -> Res {
    let d = &mut app.downloads;
    d.open = true;
    d.host
        .cancel
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let cancel = d.host.cancel.clone();
    let rx = d.host.service()?.search(None, &q, &[], 50, cancel);
    d.input = q;
    d.searching = Some(rx);
    Ok(json!({}))
}

fn start_play(app: &mut JizzApp, src: Source) -> Res {
    let d = &mut app.downloads;
    d.open = true;
    d.retry_note = None;
    // Listing goes through the probed variant so the window can show the
    // number of peers while a magnet resolves.
    let (rx, probe) = d.host.service()?.list_files_probed(src, Vec::new());
    d.probe = Some(probe);
    d.flow.begin_listing(rx);
    Ok(json!({}))
}

fn act(
    app: &mut JizzApp,
    a: &Value,
    f: impl Fn(&jizzcraft_fetch::service::FetchService, usize),
) -> Res {
    let id = a
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing `id`"))?;
    f(app.downloads.host.service()?, id as usize);
    Ok(json!({}))
}

pub(super) fn register(reg: &mut Registry<JizzApp>) {
    reg.register(
        "view.downloads",
        "Show or hide the Downloads window (network build)",
        |app, _| {
            app.downloads.open = !app.downloads.open;
            Ok(json!({ "open": app.downloads.open }))
        },
    );
    reg.register(
        "fetch.search",
        "Search the configured indexers: {query}",
        |app, a| {
            let q = a
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing `query`"))?
                .to_owned();
            start_search(app, q)
        },
    );
    reg.register(
        "fetch.play.cancel",
        "Cancel the Play flow that is listing, adding or opening",
        |app, _| {
            let d = &mut app.downloads;
            d.flow.cancel(&mut FetchBackend { host: &mut d.host });
            d.ready = None;
            d.external = false;
            d.probe = None;
            d.retry_note = None;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.resume_all",
        "Resume every paused, unfinished torrent",
        |app, _| {
            // The session is up when the banner shows; the count is not needed.
            drop(app.downloads.host.service()?.resume_all());
            app.downloads.ask_dismissed = true;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.play.retry",
        "Run the last Play again (the source is kept in memory only)",
        |app, _| {
            let (name, args) = app
                .downloads
                .last_play
                .clone()
                .ok_or_else(|| invalid("nothing to retry"))?;
            app.downloads.retry_note = None;
            app.push(name, args);
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.search.cancel",
        "Cancel the running search",
        |app, _| {
            app.downloads
                .host
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.play",
        "Play while it downloads: {magnet | url | torrent_path | result: index of the last search | id: an added torrent}",
        |app, a| {
            app.downloads.last_play = Some(("fetch.play", a.clone()));
            if let Some(id) = a.get("id").and_then(Value::as_u64) {
                let d = &mut app.downloads;
                d.host.service()?;
                d.flow
                    .start_files(id as usize, &mut FetchBackend { host: &mut d.host });
                return Ok(json!({}));
            }
            if let Some(u) = a.get("url").and_then(Value::as_str)
                && !is_torrent_url(u)
            {
                let rx = app
                    .downloads
                    .host
                    .open_any(u.to_owned(), jizzcraft_fetch::service::StreamOpts::default())?;
                app.downloads.flow.begin_opening(rx);
                app.downloads.open = true;
                return Ok(json!({}));
            }
            if let Some(i) = a.get("result").and_then(Value::as_u64)
                && let Some(r) = app.downloads.results.get(i as usize)
                && r.indexer_id >= jizzcraft_fetch::sources::ID_BASE
            {
                let u = r
                    .link
                    .clone()
                    .ok_or_else(|| invalid("result has no address"))?;
                let opts = jizzcraft_fetch::service::StreamOpts::for_result(
                    r,
                    jizzcraft_fetch::pick::Quality::default(),
                );
                let rx = app.downloads.host.open_any(u, opts)?;
                app.downloads.flow.begin_opening(rx);
                app.downloads.open = true;
                return Ok(json!({}));
            }
            let src = if let Some(m) = a.get("magnet").and_then(Value::as_str) {
                Source::Magnet(m.to_owned())
            } else if let Some(u) = a.get("url").and_then(Value::as_str) {
                Source::TorrentUrl(u.to_owned())
            } else if let Some(p) = a.get("torrent_path").and_then(Value::as_str) {
                Source::TorrentFile(p.into())
            } else if let Some(i) = a.get("result").and_then(Value::as_u64) {
                let r = app
                    .downloads
                    .results
                    .get(i as usize)
                    .ok_or_else(|| invalid("no such result"))?;
                app.downloads
                    .source_of(r)
                    .ok_or_else(|| invalid("result has no magnet or link"))?
            } else {
                return Err(invalid("give `magnet`, `torrent_path`, `result` or `id`"));
            };
            start_play(app, src)
        },
    );
    reg.register(
        "sources.list",
        "List the streaming sources you configured (no secrets)",
        |app, _| {
            let v: Vec<Value> = app
                .downloads
                .host
                .config()
                .sources
                .iter()
                .map(|s| {
                    json!({ "id": s.id, "name": s.name, "enabled": s.enabled,
                                 "has_secret": !s.secret.is_empty() })
                })
                .collect();
            Ok(json!({ "sources": v }))
        },
    );
    reg.register(
        "sources.import",
        "Add a source from a JSON file you wrote: {path?} (asks when absent)",
        |app, a| {
            let p = match a.get("path").and_then(Value::as_str) {
                Some(p) => std::path::PathBuf::from(p),
                None => match rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    Some(p) => p,
                    None => return Ok(json!({ "cancelled": true })),
                },
            };
            let bytes = std::fs::read(&p).map_err(|_| invalid("cannot read the file"))?;
            if bytes.len() > 64 * 1024 {
                return Err(invalid("definition too large"));
            }
            let id = app.downloads.host.import_source(&bytes)?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register("sources.remove", "Remove a source: {id}", |app, a| {
        let id = a
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("missing `id`"))?;
        app.downloads.host.remove_source(id as u32)?;
        Ok(json!({}))
    });
    reg.register(
        "sources.enable",
        "Turn a source on or off: {id, on}",
        |app, a| {
            let id = a
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("missing `id`"))?;
            let on = a.get("on").and_then(Value::as_bool).unwrap_or(true);
            app.downloads.host.set_source_enabled(id as u32, on)?;
            Ok(json!({}))
        },
    );
    reg.register(
        "sources.secret.set",
        "Set a source's key or token (never echoed): {id, value}",
        |app, a| {
            let id = a
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("missing `id`"))?;
            let v = a
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing `value`"))?;
            app.downloads
                .host
                .set_source_secret(id as u32, v.to_owned())?;
            Ok(json!({}))
        },
    );
    reg.register(
        "resolvers.add",
        "Register a program you installed: {name, argv: [\"/abs/path\", ..., \"{url}\"], timeout_secs?}",
        |app, a| {
            let def: jizzcraft_fetch::resolver::ResolverDef =
                serde_json::from_value(a).map_err(|_| invalid("not a resolver definition"))?;
            let id = app.downloads.host.add_resolver(def)?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register("resolvers.remove", "Forget a resolver: {id}", |app, a| {
        let id = a
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("missing `id`"))?;
        app.downloads.host.remove_resolver(id as u32)?;
        Ok(json!({}))
    });
    reg.register(
        "stream.resolve",
        "Stream a page address through one of your resolvers: {url, resolver}",
        |app, a| {
            let u = a
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing `url`"))?;
            let id = a
                .get("resolver")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("missing `resolver`"))?;
            let rx = app
                .downloads
                .host
                .service()?
                .stream_via_resolver(id as u32, u.to_owned());
            app.downloads.flow.begin_opening(rx);
            app.downloads.open = true;
            Ok(json!({}))
        },
    );
    reg.register(
        "stream.open",
        "Stream anything you point it at, without saving it: {url} (video file, .m3u8, .mpd, or a page one of your resolvers understands)",
        |app, a| {
            let u = a
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing `url`"))?;
            if is_torrent_url(u) {
                return Err(invalid("a torrent is not a direct video"));
            }
            app.downloads.last_play = Some(("stream.open", a.clone()));
            let rx = app
                .downloads
                .host
                .open_any(u.to_owned(), jizzcraft_fetch::service::StreamOpts::default())?;
            app.downloads.flow.begin_opening(rx);
            app.downloads.open = true;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.add",
        "Download without playing: {magnet | torrent_path | result}",
        |app, a| {
            let d = &mut app.downloads;
            if let Some(u) = a.get("url").and_then(Value::as_str)
                && !is_torrent_url(u)
            {
                let rx = d.host.service()?.play_http(u.to_owned());
                d.http_adding.push(rx);
                return Ok(json!({}));
            }
            if let Some(i) = a.get("result").and_then(Value::as_u64)
                && let Some(r) = d.results.get(i as usize)
                && r.indexer_id >= jizzcraft_fetch::sources::ID_BASE
            {
                let u = r
                    .link
                    .clone()
                    .ok_or_else(|| invalid("result has no address"))?;
                let rx = d.host.service()?.play_http(u);
                d.http_adding.push(rx);
                return Ok(json!({}));
            }
            let src = if let Some(m) = a.get("magnet").and_then(Value::as_str) {
                Source::Magnet(m.to_owned())
            } else if let Some(u) = a.get("url").and_then(Value::as_str) {
                Source::TorrentUrl(u.to_owned())
            } else if let Some(p) = a.get("torrent_path").and_then(Value::as_str) {
                Source::TorrentFile(p.into())
            } else if let Some(i) = a.get("result").and_then(Value::as_u64) {
                let r = d
                    .results
                    .get(i as usize)
                    .ok_or_else(|| invalid("no such result"))?;
                d.source_of(r)
                    .ok_or_else(|| invalid("result has no magnet or link"))?
            } else {
                return Err(invalid("give `magnet`, `torrent_path` or `result`"));
            };
            let rx = d.host.service()?.add(src);
            d.adding.push(rx);
            Ok(json!({}))
        },
    );
    for (name, help) in [
        ("fetch.pause", "Pause a torrent: {id}"),
        ("fetch.resume", "Resume a torrent: {id}"),
        (
            "fetch.remove",
            "Stop tracking a torrent, files are kept: {id}",
        ),
    ] {
        let h: jizzcraft_core::commands::Handler<JizzApp> = match name {
            "fetch.pause" => |app, a| act(app, &a, |s, id| s.pause(id)),
            "fetch.resume" => |app, a| act(app, &a, |s, id| s.resume(id)),
            _ => |app, a| act(app, &a, |s, id| s.forget(id)),
        };
        reg.register(name, help, h);
    }
    reg.register(
        "fetch.indexers.add",
        "Add a Torznab indexer: {url, api_key, name?}",
        |app, a| {
            let g = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(&format!("missing `{k}`")))
            };
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            app.downloads
                .host
                .add_indexer(name, g("url")?, g("api_key")?)?;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.indexers.remove",
        "Remove an indexer: {id}",
        |app, a| {
            let id = a.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
            app.downloads.host.remove_indexer(id)?;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.external",
        "Open a finished torrent in mpv or VLC: {id}. That program is not covered by the network guard.",
        |app, a| {
            let id = a
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("missing `id`"))? as usize;
            let d = &mut app.downloads;
            d.host.service()?;
            d.external = true;
            d.flow
                .start_files(id, &mut FetchBackend { host: &mut d.host });
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.indexers.test",
        "Check an indexer's address and key: {id}",
        |app, a| {
            let id = a.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
            let ix = app
                .downloads
                .host
                .config()
                .indexers
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .ok_or_else(|| jizzcraft_core::Error::NotFound("indexer".into()))?;
            let rx = app.downloads.host.service()?.test_indexer(&ix);
            app.downloads.tested = Some(rx);
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.indexers.detect",
        "Look for a local Jackett or Prowlarr on 127.0.0.1 (only when asked)",
        |app, _| {
            let rx = app.downloads.host.service()?.detect_local();
            app.downloads.detecting = Some(rx);
            app.downloads.open = true;
            Ok(json!({}))
        },
    );
    reg.register(
        "fetch.settings",
        "Advanced: {dir?, down_mibps?, up_mibps?, seed?, dht?, proxy?: socks5://…, require_proxy?, resume_on_start?: never|ask|always, seed_ratio?, seed_minutes?, bind_interface?}",
        |app, a| {
            app.downloads.host.apply_settings(&a)?;
            Ok(json!({}))
        },
    );
}

impl JizzApp {
    fn poll_flow(&mut self) {
        let d = &mut self.downloads;
        let ev = d.flow.step(&mut FetchBackend { host: &mut d.host });
        match ev {
            FlowEvent::Error(m) => {
                self.downloads.probe = None;
                if is_retryable(&m) && self.downloads.last_play.is_some() {
                    self.downloads.retry_note = Some(m);
                } else {
                    self.toast(ToastKind::Error, m);
                }
            }
            FlowEvent::Open(t) => self.downloads.ready = Some(t),
            FlowEvent::Idle | FlowEvent::Progress(_) | FlowEvent::Choose => {}
        }
        if let Some(t) = self.downloads.ready.take() {
            let external = std::mem::take(&mut self.downloads.external);
            self.open_ticket(t, external);
        }
    }

    fn poll_downloads(&mut self, ctx: &egui::Context) {
        self.poll_flow();
        let mut toasts: Vec<(ToastKind, String)> = Vec::new();
        let mut import: Option<String> = None;
        {
            let d = &mut self.downloads;
            if let Some(rx) = &d.searching
                && let Ok(out) = rx.try_recv()
            {
                let msg = if out.queried == 0 {
                    "Add an indexer first.".to_owned()
                } else if out.failed == out.queried {
                    jizzcraft_fetch::errors::NOT_REACHABLE.to_owned()
                } else if out.results.is_empty() {
                    "No results.".to_owned()
                } else {
                    format!("{} results", out.results.len())
                };
                toasts.push((ToastKind::Info, msg));
                d.results = out.results;
                d.searching = None;
            }
            d.adding.retain(|rx| match rx.try_recv() {
                Ok(Ok(_)) => {
                    toasts.push((ToastKind::Info, "Download started".into()));
                    false
                }
                Ok(Err(e)) => {
                    toasts.push((ToastKind::Error, classify(&e).to_owned()));
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(_) => false,
            });
            d.http_adding.retain(|rx| match rx.try_recv() {
                Ok(Ok(_)) => {
                    toasts.push((ToastKind::Info, "Download started".into()));
                    false
                }
                Ok(Err(e)) => {
                    toasts.push((ToastKind::Error, classify(&e).to_owned()));
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(_) => false,
            });
            if let Some(rx) = &d.detecting
                && let Ok(found) = rx.try_recv()
            {
                d.detecting = None;
                let mut added = 0;
                for f in found {
                    let known = d.host.config().indexers.iter().any(|i| i.url == f.url);
                    if !known && d.host.add_indexer(f.name, f.url, f.api_key).is_ok() {
                        added += 1;
                    }
                }
                toasts.push((
                    if added > 0 {
                        ToastKind::Info
                    } else {
                        ToastKind::Error
                    },
                    if added > 0 {
                        format!("{added} indexer(s) added")
                    } else {
                        "No local Jackett or Prowlarr found.".into()
                    },
                ));
            }
            if let Some(rx) = &d.tested
                && let Ok(r) = rx.try_recv()
            {
                d.tested = None;
                toasts.push(match r {
                    Ok(()) => (ToastKind::Info, "Indexer works.".into()),
                    Err(e) => (ToastKind::Error, classify(&e).to_owned()),
                });
            }
            let active = d.torrents.iter().any(|t| t.progress() < 1.0)
                || !d.adding.is_empty()
                || d.flow.busy();
            let due = d
                .last_poll
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
            if (d.open || active) && due && d.host.host_started() {
                if let Ok(svc) = d.host.service() {
                    d.torrents = svc.list();
                }
                d.last_poll = Some(Instant::now());
                let mut finished = 0;
                for t in &d.torrents {
                    if t.progress() >= 1.0 && t.total_bytes > 0 && d.done.insert(t.id) {
                        finished += 1;
                    }
                }
                if finished > 0 {
                    toasts.push((
                        ToastKind::Info,
                        format!("Download complete · {finished} torrent(s)"),
                    ));
                    import = Some(
                        d.host
                            .config()
                            .effective_download_dir()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            if d.open || active || d.searching.is_some() {
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
        for (k, t) in toasts {
            self.toast(k, t);
        }
        // Magnet links handed over by another launch (desktop handler).
        let due = self
            .downloads
            .inbox_checked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if due {
            self.downloads.inbox_checked = Some(Instant::now());
            for m in jizzcraft_fetch::inbox::take_all() {
                self.downloads.open = true;
                self.push("fetch.play", json!({ "magnet": m }));
            }
        }
        if let Some(p) = import {
            self.push("library.import", json!({ "path": p }));
        }
    }

    pub(super) fn downloads_window(&mut self, ctx: &egui::Context) {
        self.poll_downloads(ctx);
        if !self.downloads.open {
            return;
        }
        let discreet = self.discreet() || self.opts.disguised;
        let mut open = true;
        let mut cmds: Vec<(&'static str, Value)> = Vec::new();
        let mut choose: Option<usize> = None;
        let mut web_open = self.web_pages.shown;
        egui::Window::new(crate::i18n::tr("DOWNLOADS"))
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                let d = &mut self.downloads;
                let busy = d.flow.busy();
                let progress = d.flow.progress_key();
                let has_ix = !d.host.config().indexers.is_empty();

                // ---- banners ----------------------------------------------
                let paused_unfinished = d
                    .torrents
                    .iter()
                    .filter(|t| t.state == TorrentState::Paused && t.progress() < 1.0)
                    .count();
                for b in banners(
                    d.host.config().load_error,
                    d.load_error_dismissed,
                    d.host.config().resume_on_start,
                    paused_unfinished,
                    d.ask_dismissed,
                ) {
                    ui.horizontal_wrapped(|ui| match b {
                        Banner::ConfigUnreadable => {
                            ui.label(RichText::new(crate::i18n::tr(
                                "fetch.json could not be read; a copy was kept and your keys were not overwritten.",
                            ))
                            .color(MUTED));
                            if ui.small_button(crate::i18n::tr("Dismiss")).clicked() {
                                d.load_error_dismissed = true;
                            }
                        }
                        Banner::ResumeAsk(n) => {
                            ui.label(RichText::new(
                                crate::i18n::tr_dyn(&format!("Resume {n} paused downloads?"))
                                    .into_owned(),
                            )
                            .color(MUTED));
                            if ui.small_button(crate::i18n::tr("Resume all")).clicked() {
                                cmds.push(("fetch.resume_all", json!({})));
                                d.ask_dismissed = true;
                            }
                            if ui.small_button(crate::i18n::tr("Not now")).clicked() {
                                d.ask_dismissed = true;
                            }
                        }
                    });
                }
                if let Some(m) = d.retry_note.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(crate::i18n::tr_dyn(&m).into_owned()).color(MUTED));
                        if ui.small_button(crate::i18n::tr("Retry")).clicked() {
                            cmds.push(("fetch.play.retry", json!({})));
                        }
                        if ui.small_button(crate::i18n::tr("Dismiss")).clicked() {
                            d.retry_note = None;
                        }
                    });
                }

                // ---- the one box ------------------------------------------
                ui.horizontal(|ui| {
                    let r = ui.add(
                        TextEdit::singleline(&mut d.input)
                            .hint_text(crate::i18n::tr(
                                "search, or paste a magnet link / .torrent / video address",
                            ))
                            .desired_width(330.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let typed = parse_input(&d.input);
                    let play = ui.add_enabled(
                        typed.is_some() && !busy,
                        egui::Button::new(crate::i18n::tr("Play")),
                    );
                    let dl = ui.add_enabled(
                        matches!(
                            typed,
                            Some(
                                Typed::Magnet(_)
                                    | Typed::TorrentFile(_)
                                    | Typed::Url(_)
                                    | Typed::Source(_),
                            )
                        ),
                        egui::Button::new(crate::i18n::tr("Download")),
                    );
                    if play.clicked() || enter {
                        match &typed {
                            Some(Typed::Magnet(m)) => {
                                cmds.push(("fetch.play", json!({ "magnet": m })))
                            }
                            Some(Typed::TorrentFile(p)) => {
                                cmds.push(("fetch.play", json!({ "torrent_path": p })));
                            }
                            Some(Typed::Source(p)) => {
                                cmds.push(("sources.import", json!({ "path": p })));
                            }
                            // A plain video address streams without saving; a
                            // .torrent link keeps the torrent flow.
                            Some(Typed::Url(u)) if !is_torrent_url(u) => {
                                cmds.push(("stream.open", json!({ "url": u })))
                            }
                            Some(Typed::Url(u)) => cmds.push(("fetch.play", json!({ "url": u }))),
                            Some(Typed::Search(q)) => {
                                cmds.push(("fetch.search", json!({ "query": q })))
                            }
                            None => {}
                        }
                    }
                    if dl.clicked() {
                        match &typed {
                            Some(Typed::Magnet(m)) => {
                                cmds.push(("fetch.add", json!({ "magnet": m })))
                            }
                            Some(Typed::TorrentFile(p)) => {
                                cmds.push(("fetch.add", json!({ "torrent_path": p })));
                            }
                            Some(Typed::Source(p)) => {
                                cmds.push(("sources.import", json!({ "path": p })));
                            }
                            Some(Typed::Url(u)) => cmds.push(("fetch.add", json!({ "url": u }))),
                            _ => {}
                        }
                    }
                    if d.searching.is_some() && ui.button(crate::i18n::tr("Cancel")).clicked() {
                        cmds.push(("fetch.search.cancel", json!({})));
                    }
                });
                if let Some(key) = progress {
                    ui.horizontal(|ui| {
                        let peers = d
                            .probe
                            .as_ref()
                            .filter(|p| p.state() == ProbeState::Resolving)
                            .map(|p| p.peers());
                        let line = match probe_line(peers) {
                            Some(l) if key == P_LISTING => {
                                crate::i18n::tr_dyn(&l).into_owned()
                            }
                            _ => crate::i18n::tr(key).to_owned(),
                        };
                        ui.label(RichText::new(line).color(MUTED));
                        if ui.button(crate::i18n::tr("Cancel")).clicked() {
                            cmds.push(("fetch.play.cancel", json!({})));
                        }
                    });
                    if let Some(q) = crate::fun::steady(crate::fun::Quip::Starting) {
                        ui.label(RichText::new(q).italics().color(FAINT));
                    }
                }
                if let Some((files, picks)) = d.flow.choices() {
                    ui.add_space(4.0);
                    widgets::eyebrow(ui, "", crate::i18n::tr("Which one?"), "");
                    for &i in picks {
                        let label = if discreet {
                            format!("File {}  ·  {}", i + 1, human(files[i].1))
                        } else {
                            format!("{}  ·  {}", files[i].0, human(files[i].1))
                        };
                        if ui.button(label).clicked() {
                            choose = Some(i);
                        }
                    }
                    if ui.button(crate::i18n::tr("Cancel")).clicked() {
                        cmds.push(("fetch.play.cancel", json!({})));
                    }
                }

                // ---- setup, only while there is no indexer -----------------
                if !has_ix {
                    ui.add_space(8.0);
                    widgets::eyebrow(ui, "", crate::i18n::tr("Search needs an indexer"), "");
                    ui.label(
                        RichText::new(
                            "Nothing is bundled. Use your own Jackett or Prowlarr (Torznab). \
                             Magnet links and .torrent files work without one.",
                        )
                        .color(MUTED),
                    );
                    if ui
                        .button(crate::i18n::tr("Find on this computer"))
                        .clicked()
                    {
                        cmds.push(("fetch.indexers.detect", json!({})));
                    }
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut d.ix_url)
                                .hint_text(crate::i18n::tr("Torznab URL"))
                                .desired_width(280.0),
                        );
                        ui.add(
                            TextEdit::singleline(&mut d.ix_key)
                                .hint_text(crate::i18n::tr("API key"))
                                .password(true)
                                .desired_width(120.0),
                        );
                        if ui.button(crate::i18n::tr("Add")).clicked()
                            && !d.ix_url.trim().is_empty()
                        {
                            cmds.push((
                                "fetch.indexers.add",
                                json!({ "url": d.ix_url.trim(), "api_key": d.ix_key }),
                            ));
                            d.ix_url.clear();
                            d.ix_key.clear();
                        }
                    });
                }

                // ---- results -----------------------------------------------
                if !d.results.is_empty() {
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .id_salt("dl-results")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for (i, r) in d.results.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let title = if discreet {
                                        format!("Result {}", i + 1)
                                    } else {
                                        r.title.clone()
                                    };
                                    ui.label(title);
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  S{}",
                                            r.size.map(human).unwrap_or_default(),
                                            r.seeders.unwrap_or(0)
                                        ))
                                        .color(MUTED),
                                    );
                                    if ui
                                        .add_enabled(
                                            !busy,
                                            egui::Button::new(crate::i18n::tr("Play")),
                                        )
                                        .clicked()
                                    {
                                        cmds.push(("fetch.play", json!({ "result": i })));
                                    }
                                    if ui.small_button(crate::i18n::tr("Download")).clicked() {
                                        cmds.push(("fetch.add", json!({ "result": i })));
                                    }
                                });
                            }
                        });
                }

                // ---- torrents ---------------------------------------------
                ui.add_space(8.0);
                widgets::eyebrow(ui, "", crate::i18n::tr("Downloads"), "");
                if d.torrents.is_empty() {
                    ui.label(RichText::new(crate::i18n::tr("Nothing downloading.")).color(FAINT));
                    if let Some(q) = crate::fun::steady(crate::fun::Quip::NothingDownloading) {
                        ui.label(RichText::new(q).italics().color(FAINT));
                    }
                    if !d.host.host_started()
                        && ui
                            .small_button(crate::i18n::tr("Resume saved downloads"))
                            .clicked()
                        && let Ok(svc) = d.host.service()
                    {
                        drop(svc.start());
                    }
                }
                for t in &d.torrents {
                    ui.horizontal(|ui| {
                        let name = if discreet {
                            format!("Torrent {}", t.id)
                        } else {
                            t.name.clone()
                        };
                        ui.label(name);
                        ui.add(
                            ProgressBar::new(t.progress())
                                .desired_width(120.0)
                                .show_percentage(),
                        );
                        let state = match t.state {
                            _ if t.error.is_some() => crate::i18n::tr_dyn(classify(
                                t.error.as_deref().unwrap_or("error"),
                            ))
                            .into_owned(),
                            TorrentState::Initializing => crate::i18n::tr("starting").to_owned(),
                            TorrentState::Downloading => format!("↓ {}/s", human(t.down_bps)),
                            TorrentState::Seeding => crate::i18n::tr("done").to_owned(),
                            TorrentState::Paused => crate::i18n::tr("paused").to_owned(),
                            TorrentState::Error => crate::i18n::tr("error").to_owned(),
                        };
                        ui.label(RichText::new(state).color(MUTED));
                        if ui
                            .add_enabled(!busy, egui::Button::new(crate::i18n::tr("Play")))
                            .clicked()
                        {
                            cmds.push(("fetch.play", json!({ "id": t.id })));
                        }
                        if t.progress() >= 1.0
                            && ui
                                .small_button(crate::i18n::tr("External"))
                                .on_hover_text(
                                    "Open in mpv or VLC. That program is not covered by \
                                     JizzCraft's network protection.",
                                )
                                .clicked()
                        {
                            cmds.push(("fetch.external", json!({ "id": t.id })));
                        }
                        let paused = t.state == TorrentState::Paused;
                        if ui
                            .small_button(if paused {
                                crate::i18n::tr("Resume")
                            } else {
                                crate::i18n::tr("Pause")
                            })
                            .clicked()
                        {
                            cmds.push((
                                if paused {
                                    "fetch.resume"
                                } else {
                                    "fetch.pause"
                                },
                                json!({ "id": t.id }),
                            ));
                        }
                        if ui
                            .small_button(crate::i18n::tr("Forget"))
                            .on_hover_text(crate::i18n::tr("Stops tracking. Files are kept."))
                            .clicked()
                        {
                            cmds.push(("fetch.remove", json!({ "id": t.id })));
                        }
                    });
                }

                // ---- advanced, folded ---------------------------------------
                // Folded like a fitted sheet: technically, and with regret.
                ui.add_space(8.0);
                ui.collapsing(crate::i18n::tr("Advanced"), |ui| {
                    ui.label(
                        RichText::new(
                            "Connects only to the indexers you add and to BitTorrent peers. \
                             Your IP is visible to them.",
                        )
                        .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Folder"));
                        ui.add(
                            TextEdit::singleline(&mut d.dir)
                                .hint_text(crate::i18n::tr("default: Downloads/…"))
                                .desired_width(300.0),
                        );
                        if ui.button(crate::i18n::tr("Set")).clicked() {
                            cmds.push(("fetch.settings", json!({ "dir": d.dir.trim() })));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Max MB/s  down"));
                        ui.add(TextEdit::singleline(&mut d.down_limit).desired_width(40.0));
                        ui.label(crate::i18n::tr("up"));
                        ui.add(TextEdit::singleline(&mut d.up_limit).desired_width(40.0));
                        if ui.button(crate::i18n::tr("Apply")).clicked() {
                            let n = |s: &str| s.trim().parse::<u32>().unwrap_or(0);
                            cmds.push((
                            "fetch.settings",
                            json!({ "down_mibps": n(&d.down_limit), "up_mibps": n(&d.up_limit) }),
                        ));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Proxy"));
                        ui.add(
                            TextEdit::singleline(&mut d.proxy)
                                .hint_text(crate::i18n::tr("socks5://host:port"))
                                .password(true)
                                .desired_width(220.0),
                        );
                        if ui.button(crate::i18n::tr("Set")).clicked() {
                            cmds.push(("fetch.settings", json!({ "proxy": d.proxy.trim() })));
                        }
                    });
                    let mut req = d.host.config().require_proxy;
                    if ui
                        .checkbox(
                            &mut req,
                            crate::i18n::tr("Block the network without a proxy"),
                        )
                        .clicked()
                    {
                        cmds.push(("fetch.settings", json!({ "require_proxy": req })));
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "With a proxy, DHT and trackers are off: they cannot be proxied.",
                        ))
                        .color(FAINT),
                    );
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Paused downloads on start"));
                        let cur = d.host.config().resume_on_start;
                        for (v, name, label) in [
                            (ResumeOnStart::Never, "never", "Never"),
                            (ResumeOnStart::Ask, "ask", "Ask"),
                            (ResumeOnStart::Always, "always", "Always"),
                        ] {
                            if ui
                                .selectable_label(cur == v, crate::i18n::tr(label))
                                .clicked()
                                && cur != v
                            {
                                cmds.push((
                                    "fetch.settings",
                                    json!({ "resume_on_start": name }),
                                ));
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Seed ratio"));
                        ui.add(TextEdit::singleline(&mut d.seed_ratio).desired_width(40.0));
                        ui.label(crate::i18n::tr("Seed minutes"));
                        ui.add(TextEdit::singleline(&mut d.seed_minutes).desired_width(50.0));
                        if ui.button(crate::i18n::tr("Apply")).clicked() {
                            let ratio = d.seed_ratio.trim().parse::<f64>().unwrap_or(0.0);
                            let mins = d.seed_minutes.trim().parse::<u32>().unwrap_or(0);
                            cmds.push((
                                "fetch.settings",
                                json!({ "seed_ratio": ratio.max(0.0), "seed_minutes": mins }),
                            ));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("Network interface"));
                        ui.add(TextEdit::singleline(&mut d.bind).desired_width(120.0));
                        if ui.button(crate::i18n::tr("Set")).clicked() {
                            cmds.push((
                                "fetch.settings",
                                json!({ "bind_interface": d.bind.trim() }),
                            ));
                        }
                    });
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "Peers and trackers use only this interface, such as a VPN. Empty means any.",
                        ))
                        .color(FAINT),
                    );
                    // ---- your own sources and resolvers --------------------
                    ui.add_space(6.0);
                    widgets::eyebrow(ui, "", crate::i18n::tr("Streaming sources"), "");
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "None is included. Add a JSON file you wrote; keys stay in fetch.json.",
                        ))
                        .color(FAINT),
                    );
                    for s in &d.host.config().sources {
                        ui.horizontal(|ui| {
                            let name = if discreet {
                                format!("Source {}", s.id)
                            } else {
                                s.name.clone()
                            };
                            let mut on = s.enabled;
                            if ui.checkbox(&mut on, name).clicked() {
                                cmds.push(("sources.enable", json!({ "id": s.id, "on": on })));
                            }
                            if s.auth_header.is_some() {
                                let key = d.src_key.entry(s.id).or_default();
                                ui.add(
                                    TextEdit::singleline(key)
                                        .hint_text(if s.secret.is_empty() {
                                            crate::i18n::tr("key")
                                        } else {
                                            "••••"
                                        })
                                        .password(true)
                                        .desired_width(110.0),
                                );
                                if ui.small_button(crate::i18n::tr("Set")).clicked() {
                                    cmds.push((
                                        "sources.secret.set",
                                        json!({ "id": s.id, "value": key.trim() }),
                                    ));
                                    key.clear();
                                }
                            }
                            if ui.small_button(crate::i18n::tr("Remove")).clicked() {
                                cmds.push(("sources.remove", json!({ "id": s.id })));
                            }
                        });
                    }
                    if ui
                        .small_button(crate::i18n::tr("Add a source file…"))
                        .clicked()
                    {
                        cmds.push(("sources.import", json!({})));
                    }
                    for r in &d.host.config().resolvers {
                        ui.horizontal(|ui| {
                            let name = if discreet {
                                format!("Resolver {}", r.id)
                            } else {
                                r.name.clone()
                            };
                            ui.label(format!("{name}  #{}", r.id));
                            if ui.small_button(crate::i18n::tr("Remove")).clicked() {
                                cmds.push(("resolvers.remove", json!({ "id": r.id })));
                            }
                        });
                    }
                    if ui
                        .button(crate::i18n::tr("Set up web pages…"))
                        .clicked()
                    {
                        web_open = !web_open;
                    }
                    ui.add_space(6.0);
                    let seed = d.host.config().seed_after_complete;
                    if widgets::toggle_chip(ui, crate::i18n::tr("Keep seeding when done"), seed)
                        .clicked()
                    {
                        cmds.push(("fetch.settings", json!({ "seed": !seed })));
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "Speed caps apply the next time the session starts.",
                        ))
                        .color(FAINT),
                    );
                    let ixs: Vec<(u32, String)> = d
                        .host
                        .config()
                        .indexers
                        .iter()
                        .map(|i| (i.id, i.name.clone()))
                        .collect();
                    for (id, name) in ixs {
                        ui.horizontal(|ui| {
                            ui.label(if discreet {
                                format!("Indexer {id}")
                            } else {
                                name
                            });
                            if ui.small_button(crate::i18n::tr("Test")).clicked() {
                                cmds.push(("fetch.indexers.test", json!({ "id": id })));
                            }
                            if ui.small_button(crate::i18n::tr("Remove")).clicked() {
                                cmds.push(("fetch.indexers.remove", json!({ "id": id })));
                            }
                        });
                    }
                    if has_ix
                        && ui
                            .small_button(crate::i18n::tr("Find local Jackett / Prowlarr"))
                            .clicked()
                    {
                        cmds.push(("fetch.indexers.detect", json!({})));
                    }
                });
                // The web-pages panel needs the whole app; `d` is done.
                if web_open {
                    ui.add_space(6.0);
                    super::web_pages::panel(self, ui);
                }
            });
        self.web_pages.shown = web_open;
        self.downloads.open = open;
        if let Some(i) = choose {
            let d = &mut self.downloads;
            d.flow.choose(i, &mut FetchBackend { host: &mut d.host });
        }
        for (n, a) in cmds {
            self.push(n, a);
        }
    }
}

/// Find an executable on `PATH`.
fn on_path(name: &str) -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(&exe))
            .find(|c| c.is_file())
    })
}

/// Launch mpv, else VLC, on a finished local file. Arguments only, no shell.
/// The file is checked first: a playlist would make that program fetch URLs.
pub(super) fn open_external(path: &std::path::Path) -> Result<(), &'static str> {
    use std::io::Read;
    let abs =
        jizzcraft_core::pathbytes::canonical(path).map_err(|_| "The file is not available yet.")?;
    let mut head = vec![0u8; 4096];
    let n = std::fs::File::open(&abs)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|_| "The file is not available yet.")?;
    if jizzcraft_core::manifest::looks_like_manifest(&head[..n]) {
        return Err("Refused: this file refers to network media.");
    }
    let (bin, is_mpv) = if let Some(b) = on_path("mpv") {
        (b, true)
    } else if let Some(b) = on_path("vlc") {
        (b, false)
    } else {
        return Err("Install mpv or VLC to use an external player.");
    };
    let mut c = std::process::Command::new(bin);
    if is_mpv {
        c.arg("--");
    }
    c.arg(&abs)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "Could not start the external player.")
}
