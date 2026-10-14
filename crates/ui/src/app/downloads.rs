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
use jizzcraft_fetch::pick::{Pick, pick_video};
use jizzcraft_fetch::service::{
    Listed, SearchOutcome, Source, StreamTicket, TorrentState, TorrentStatus,
};
use jizzcraft_fetch::{FetchConfig, SearchResult};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Steps of "press Play": list, maybe ask which file, add, open.
enum Flow {
    Listing(Receiver<Result<Listed, String>>),
    Files {
        id: usize,
        rx: Receiver<Result<Vec<(String, u64)>, String>>,
    },
    Choosing {
        listed: Option<Listed>,
        id: Option<usize>,
        files: Vec<(String, u64)>,
        picks: Vec<usize>,
    },
    Adding {
        rx: Receiver<Result<usize, String>>,
        file: usize,
    },
    Opening(Receiver<Result<StreamTicket, String>>),
}

pub(super) struct Downloads {
    host: FetchHost,
    open: bool,
    input: String,
    results: Vec<SearchResult>,
    searching: Option<Receiver<SearchOutcome>>,
    adding: Vec<Receiver<Result<usize, String>>>,
    flow: Option<Flow>,
    torrents: Vec<TorrentStatus>,
    done: HashSet<usize>,
    last_poll: Option<Instant>,
    /// Ready to open once the poll has the whole picture (set by the flow).
    ready: Option<StreamTicket>,
    detecting: Option<Receiver<Vec<jizzcraft_fetch::detect::Detected>>>,
    // setup form
    ix_url: String,
    ix_key: String,
    tested: Option<Receiver<Result<(), String>>>,
    // advanced
    dir: String,
    down_limit: String,
    up_limit: String,
}

impl Downloads {
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
        Self {
            host,
            open: false,
            input: String::new(),
            results: Vec::new(),
            searching: None,
            adding: Vec::new(),
            flow: None,
            torrents: Vec::new(),
            done: HashSet::new(),
            last_poll: None,
            ready: None,
            detecting: None,
            ix_url: String::new(),
            ix_key: String::new(),
            tested: None,
            dir,
            down_limit,
            up_limit,
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
    TorrentFile(String),
    Search(String),
}

fn parse_input(s: &str) -> Option<Typed> {
    let t = s.trim().trim_matches('"');
    if t.is_empty() {
        None
    } else if t.starts_with("magnet:") {
        Some(Typed::Magnet(t.to_owned()))
    } else if t.to_ascii_lowercase().ends_with(".torrent") {
        Some(Typed::TorrentFile(t.to_owned()))
    } else {
        Some(Typed::Search(t.to_owned()))
    }
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
    let rx = d.host.service()?.list_files(src);
    d.flow = Some(Flow::Listing(rx));
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
        "Play a torrent while it downloads: {magnet | torrent_path | result: index of the last search | id: an added torrent}",
        |app, a| {
            if let Some(id) = a.get("id").and_then(Value::as_u64) {
                let rx = app.downloads.host.service()?.torrent_files(id as usize);
                app.downloads.flow = Some(Flow::Files { id: id as usize, rx });
                return Ok(json!({}));
            }
            let src = if let Some(m) = a.get("magnet").and_then(Value::as_str) {
                Source::Magnet(m.to_owned())
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
        "fetch.add",
        "Download without playing: {magnet | torrent_path | result}",
        |app, a| {
            let d = &mut app.downloads;
            let src = if let Some(m) = a.get("magnet").and_then(Value::as_str) {
                Source::Magnet(m.to_owned())
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
        "Advanced: {dir?, down_mibps?, up_mibps?, seed?, dht?}",
        |app, a| {
            let d = &mut app.downloads;
            let num = |k: &str| a.get(k).and_then(Value::as_u64).map(|n| n as u32);
            let (dir, seed, dht) = (
                a.get("dir").and_then(Value::as_str).map(str::to_owned),
                a.get("seed").and_then(Value::as_bool),
                a.get("dht").and_then(Value::as_bool),
            );
            let (down, up) = (num("down_mibps"), num("up_mibps"));
            d.host.update_settings(|c| {
                if let Some(p) = &dir {
                    c.download_dir = (!p.is_empty()).then(|| p.into());
                }
                if a.get("down_mibps").is_some() {
                    c.down_limit_mibps = down.filter(|n| *n > 0);
                }
                if a.get("up_mibps").is_some() {
                    c.up_limit_mibps = up.filter(|n| *n > 0);
                }
                if let Some(s) = seed {
                    c.seed_after_complete = s;
                }
                if let Some(h) = dht {
                    c.dht = h;
                }
            })?;
            Ok(json!({}))
        },
    );
}

impl JizzApp {
    /// Open a stream in the player. Name is hidden when discreet or disguised.
    fn open_stream_player(
        &mut self,
        name: String,
        reader: jizzcraft_fetch::service::TorrentReader,
        len: u64,
    ) {
        let shown = if self.discreet() || self.opts.disguised {
            "Video".to_owned()
        } else {
            name
        };
        self.close_view();
        let ctx = self.ctx.clone();
        let vol = f64::from(self.cfg.volume.unwrap_or(0.8));
        let wake = std::sync::Arc::new(move || ctx.request_repaint_after(Duration::from_millis(1)));
        let player = jizzcraft_media::player::Player::open_stream(Box::new(reader), len, vol, wake);
        let mut pv = crate::player_view::PlayerView::new(None, shown, player);
        pv.autoplay_on = false;
        // Scrub previews read the file from disk: not available while streaming.
        pv.previews_enabled = false;
        self.view = super::View::Player(Box::new(pv));
        self.autoplay_at = None;
    }

    fn fail_toast(&mut self, raw: &str) {
        self.toast(ToastKind::Error, classify(raw));
    }

    fn poll_flow(&mut self) {
        let Some(flow) = self.downloads.flow.take() else {
            return;
        };
        match flow {
            Flow::Listing(rx) => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.downloads.flow = Some(Flow::Listing(rx)),
                Err(_) => self.fail_toast("error"),
                Ok(Err(e)) => self.fail_toast(&e),
                Ok(Ok(listed)) => match pick_video(&listed.files) {
                    Pick::None => self.toast(ToastKind::Error, "No video file in this torrent."),
                    Pick::One(i) => self.add_for_play(listed, i),
                    Pick::Ask(picks) => {
                        let files = listed.files.clone();
                        self.downloads.flow = Some(Flow::Choosing {
                            listed: Some(listed),
                            id: None,
                            files,
                            picks,
                        });
                    }
                },
            },
            Flow::Files { id, rx } => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.downloads.flow = Some(Flow::Files { id, rx }),
                Err(_) => self.fail_toast("error"),
                Ok(Err(e)) => self.fail_toast(&e),
                Ok(Ok(files)) => match pick_video(&files) {
                    Pick::None => self.toast(ToastKind::Error, "No video file in this torrent."),
                    Pick::One(i) => self.open_file(id, i),
                    Pick::Ask(picks) => {
                        self.downloads.flow = Some(Flow::Choosing {
                            listed: None,
                            id: Some(id),
                            files,
                            picks,
                        });
                    }
                },
            },
            Flow::Adding { rx, file } => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.downloads.flow = Some(Flow::Adding { rx, file }),
                Err(_) => self.fail_toast("error"),
                Ok(Err(e)) => self.fail_toast(&e),
                Ok(Ok(id)) => self.open_file(id, file),
            },
            Flow::Opening(rx) => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.downloads.flow = Some(Flow::Opening(rx)),
                Err(_) => self.fail_toast("error"),
                Ok(Err(e)) => self.fail_toast(&e),
                Ok(Ok(ticket)) => self.downloads.ready = Some(ticket),
            },
            // Waiting for the user's click in the window.
            c @ Flow::Choosing { .. } => self.downloads.flow = Some(c),
        }
        if let Some(t) = self.downloads.ready.take() {
            match t {
                StreamTicket::Done(path) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.open_player(None, name, &path, None);
                }
                StreamTicket::Live { reader, len, name } => {
                    self.open_stream_player(name, reader, len)
                }
            }
        }
    }

    fn add_for_play(&mut self, listed: Listed, file: usize) {
        match self.downloads.host.service() {
            Ok(svc) => {
                let rx = svc.add_listed(listed, Some(vec![file]));
                self.downloads.flow = Some(Flow::Adding { rx, file });
            }
            Err(e) => self.fail_toast(&e.to_string()),
        }
    }

    fn open_file(&mut self, id: usize, file: usize) {
        match self.downloads.host.service() {
            Ok(svc) => self.downloads.flow = Some(Flow::Opening(svc.open_reader(id, file))),
            Err(e) => self.fail_toast(&e.to_string()),
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
                || d.flow.is_some();
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
        egui::Window::new("DOWNLOADS")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                let d = &mut self.downloads;
                let busy = d.flow.is_some();
                let has_ix = !d.host.config().indexers.is_empty();

                // ---- the one box ------------------------------------------
                ui.horizontal(|ui| {
                    let r = ui.add(
                        TextEdit::singleline(&mut d.input)
                            .hint_text("search, or paste a magnet link / .torrent path")
                            .desired_width(330.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let typed = parse_input(&d.input);
                    let play = ui.add_enabled(typed.is_some() && !busy, egui::Button::new("Play"));
                    let dl = ui.add_enabled(
                        matches!(typed, Some(Typed::Magnet(_) | Typed::TorrentFile(_))),
                        egui::Button::new("Download"),
                    );
                    if play.clicked() || enter {
                        match &typed {
                            Some(Typed::Magnet(m)) => {
                                cmds.push(("fetch.play", json!({ "magnet": m })))
                            }
                            Some(Typed::TorrentFile(p)) => {
                                cmds.push(("fetch.play", json!({ "torrent_path": p })));
                            }
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
                            _ => {}
                        }
                    }
                    if d.searching.is_some() && ui.button("Cancel").clicked() {
                        cmds.push(("fetch.search.cancel", json!({})));
                    }
                });
                if busy {
                    ui.label(RichText::new("Starting playback…").color(MUTED));
                }
                if let Some(Flow::Choosing { files, picks, .. }) = &d.flow {
                    ui.add_space(4.0);
                    widgets::eyebrow(ui, "", "Which one?", "");
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
                }

                // ---- setup, only while there is no indexer -----------------
                if !has_ix {
                    ui.add_space(8.0);
                    widgets::eyebrow(ui, "", "Search needs an indexer", "");
                    ui.label(
                        RichText::new(
                            "Nothing is bundled. Use your own Jackett or Prowlarr (Torznab). \
                             Magnet links and .torrent files work without one.",
                        )
                        .color(MUTED),
                    );
                    if ui.button("Find on this computer").clicked() {
                        cmds.push(("fetch.indexers.detect", json!({})));
                    }
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut d.ix_url)
                                .hint_text("Torznab URL")
                                .desired_width(280.0),
                        );
                        ui.add(
                            TextEdit::singleline(&mut d.ix_key)
                                .hint_text("API key")
                                .password(true)
                                .desired_width(120.0),
                        );
                        if ui.button("Add").clicked() && !d.ix_url.trim().is_empty() {
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
                                    if ui.add_enabled(!busy, egui::Button::new("Play")).clicked() {
                                        cmds.push(("fetch.play", json!({ "result": i })));
                                    }
                                    if ui.small_button("Download").clicked() {
                                        cmds.push(("fetch.add", json!({ "result": i })));
                                    }
                                });
                            }
                        });
                }

                // ---- torrents ---------------------------------------------
                ui.add_space(8.0);
                widgets::eyebrow(ui, "", "Downloads", "");
                if d.torrents.is_empty() {
                    ui.label(RichText::new("Nothing downloading.").color(FAINT));
                    if !d.host.host_started()
                        && ui.small_button("Resume saved downloads").clicked()
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
                            TorrentState::Initializing => "starting".to_owned(),
                            TorrentState::Downloading => format!("↓ {}/s", human(t.down_bps)),
                            TorrentState::Seeding => "done".to_owned(),
                            TorrentState::Paused => "paused".to_owned(),
                            TorrentState::Error => "error".to_owned(),
                        };
                        ui.label(RichText::new(state).color(MUTED));
                        if ui.add_enabled(!busy, egui::Button::new("Play")).clicked() {
                            cmds.push(("fetch.play", json!({ "id": t.id })));
                        }
                        let paused = t.state == TorrentState::Paused;
                        if ui
                            .small_button(if paused { "Resume" } else { "Pause" })
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
                            .small_button("Forget")
                            .on_hover_text("Stops tracking. Files are kept.")
                            .clicked()
                        {
                            cmds.push(("fetch.remove", json!({ "id": t.id })));
                        }
                    });
                }

                // ---- advanced, folded ---------------------------------------
                ui.add_space(8.0);
                ui.collapsing("Advanced", |ui| {
                    ui.label(
                        RichText::new(
                            "Connects only to the indexers you add and to BitTorrent peers. \
                             Your IP is visible to them.",
                        )
                        .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Folder");
                        ui.add(
                            TextEdit::singleline(&mut d.dir)
                                .hint_text("default: Downloads/…")
                                .desired_width(300.0),
                        );
                        if ui.button("Set").clicked() {
                            cmds.push(("fetch.settings", json!({ "dir": d.dir.trim() })));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Max MB/s  down");
                        ui.add(TextEdit::singleline(&mut d.down_limit).desired_width(40.0));
                        ui.label("up");
                        ui.add(TextEdit::singleline(&mut d.up_limit).desired_width(40.0));
                        if ui.button("Apply").clicked() {
                            let n = |s: &str| s.trim().parse::<u32>().unwrap_or(0);
                            cmds.push((
                            "fetch.settings",
                            json!({ "down_mibps": n(&d.down_limit), "up_mibps": n(&d.up_limit) }),
                        ));
                        }
                    });
                    let seed = d.host.config().seed_after_complete;
                    if widgets::toggle_chip(ui, "Keep seeding when done", seed).clicked() {
                        cmds.push(("fetch.settings", json!({ "seed": !seed })));
                    }
                    ui.label(
                        RichText::new("Speed caps apply the next time the session starts.")
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
                            if ui.small_button("Remove").clicked() {
                                cmds.push(("fetch.indexers.remove", json!({ "id": id })));
                            }
                        });
                    }
                    if has_ix && ui.small_button("Find local Jackett / Prowlarr").clicked() {
                        cmds.push(("fetch.indexers.detect", json!({})));
                    }
                });
            });
        self.downloads.open = open;
        if let Some(i) = choose
            && let Some(Flow::Choosing { listed, id, .. }) = self.downloads.flow.take()
        {
            match (listed, id) {
                (Some(l), _) => self.add_for_play(l, i),
                (None, Some(id)) => self.open_file(id, i),
                _ => {}
            }
        }
        for (n, a) in cmds {
            self.push(n, a);
        }
    }
}
