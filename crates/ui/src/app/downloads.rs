//! Downloads window (feature `network`): Torznab search, torrent list.
//!
//! Nothing here blocks the UI thread: the service API is channel based and
//! every receiver is polled with `try_recv`. No query, title, URL or path ever
//! reaches a toast or a log; toasts carry counts only.

use super::{JizzApp, ToastKind};
use crate::theme::{FAINT, MUTED};
use crate::widgets;
use egui::{ProgressBar, RichText, TextEdit};
use jizzcraft_core::Registry;
use jizzcraft_fetch::commands::FetchHost;
use jizzcraft_fetch::service::{SearchOutcome, Source, TorrentState, TorrentStatus};
use jizzcraft_fetch::{FetchConfig, SearchResult};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub(super) struct Downloads {
    host: FetchHost,
    open: bool,
    query: String,
    results: Vec<SearchResult>,
    searching: Option<Receiver<SearchOutcome>>,
    adding: Vec<Receiver<Result<usize, String>>>,
    torrents: Vec<TorrentStatus>,
    done: HashSet<usize>,
    last_poll: Option<Instant>,
    // form state
    ix_name: String,
    ix_url: String,
    ix_key: String,
    magnet: String,
    dir: String,
}

impl Downloads {
    pub(super) fn new() -> Self {
        let host = FetchHost::new(FetchConfig::default_path());
        let dir = host
            .config()
            .download_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            host,
            open: false,
            query: String::new(),
            results: Vec::new(),
            searching: None,
            adding: Vec::new(),
            torrents: Vec::new(),
            done: HashSet::new(),
            last_poll: None,
            ix_name: String::new(),
            ix_url: String::new(),
            ix_key: String::new(),
            magnet: String::new(),
            dir,
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
        "Search the configured indexers: {query, limit?}",
        |app, a| {
            let q = a
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| jizzcraft_core::Error::Invalid("missing `query`".into()))?
                .to_owned();
            let limit = a
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .min(200) as u32;
            let d = &mut app.downloads;
            d.open = true;
            d.host
                .cancel
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let cancel = d.host.cancel.clone();
            let rx = d.host.service()?.search(None, &q, &[], limit, cancel);
            d.query = q;
            d.searching = Some(rx);
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
        "fetch.add",
        "Add a torrent: {magnet | result: index of the last search}",
        |app, a| {
            let d = &mut app.downloads;
            let src = if let Some(m) = a.get("magnet").and_then(Value::as_str) {
                Source::Magnet(m.to_owned())
            } else if let Some(i) = a.get("result").and_then(Value::as_u64) {
                let r = d
                    .results
                    .get(i as usize)
                    .ok_or_else(|| jizzcraft_core::Error::NotFound("result".into()))?;
                match (&r.magnet, &r.link) {
                    (Some(m), _) => Source::Magnet(m.clone()),
                    (None, Some(l)) => Source::TorrentUrl(l.clone()),
                    _ => return Err(jizzcraft_core::Error::Invalid("no magnet or link".into())),
                }
            } else {
                return Err(jizzcraft_core::Error::Invalid(
                    "give `magnet` or `result`".into(),
                ));
            };
            if d.host.config().download_dir.is_none() {
                return Err(jizzcraft_core::Error::Invalid(
                    "set a download folder first".into(),
                ));
            }
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
        "Add a Torznab indexer: {name, url, api_key}",
        |app, a| {
            let g = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| jizzcraft_core::Error::Invalid(format!("missing `{k}`")))
            };
            app.downloads
                .host
                .add_indexer(g("name")?, g("url")?, g("api_key")?)?;
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
        "fetch.dir.set",
        "Set the download folder (also imported as a library folder): {path}",
        |app, a| {
            let p = a
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| jizzcraft_core::Error::Invalid("missing `path`".into()))?
                .to_owned();
            app.downloads
                .host
                .set_download_dir(std::path::PathBuf::from(&p))?;
            app.downloads.dir = p.clone();
            app.push("library.import", json!({ "path": p }));
            Ok(json!({}))
        },
    );
}

fn act(
    app: &mut JizzApp,
    a: &Value,
    f: impl Fn(&jizzcraft_fetch::service::FetchService, usize),
) -> jizzcraft_core::Result<Value> {
    let id = a
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| jizzcraft_core::Error::Invalid("missing `id`".into()))?;
    f(app.downloads.host.service()?, id as usize);
    Ok(json!({}))
}

impl JizzApp {
    fn poll_downloads(&mut self, ctx: &egui::Context) {
        let mut toasts: Vec<(ToastKind, String)> = Vec::new();
        let mut import: Option<String> = None;
        {
            let d = &mut self.downloads;
            if let Some(rx) = &d.searching
                && let Ok(out) = rx.try_recv()
            {
                toasts.push((
                    ToastKind::Info,
                    format!(
                        "Search · {} results from {} indexers{}",
                        out.results.len(),
                        out.queried - out.failed,
                        if out.failed > 0 {
                            format!(" ({} failed)", out.failed)
                        } else {
                            String::new()
                        }
                    ),
                ));
                d.results = out.results;
                d.searching = None;
            }
            d.adding.retain(|rx| match rx.try_recv() {
                Ok(Ok(_)) => {
                    toasts.push((ToastKind::Info, "Torrent added".into()));
                    false
                }
                Ok(Err(e)) => {
                    toasts.push((ToastKind::Error, format!("Add failed: {e}")));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(_) => false,
            });
            let active = d.torrents.iter().any(|t| t.progress() < 1.0) || !d.adding.is_empty();
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
                    import = d
                        .host
                        .config()
                        .download_dir
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned());
                }
            }
            if d.open || active || d.searching.is_some() || !d.adding.is_empty() {
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
        let discreet = self.discreet();
        let mut open = true;
        let mut cmds: Vec<(&'static str, Value)> = Vec::new();
        egui::Window::new("DOWNLOADS")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                let d = &mut self.downloads;
                ui.label(
                    RichText::new(
                        "Network build. Connects only to the indexers you add and to BitTorrent \
                         peers once you add a torrent. Your IP is visible to them.",
                    )
                    .color(MUTED),
                );
                ui.add_space(6.0);
                widgets::eyebrow(ui, "", "Folder", "");
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut d.dir).hint_text("download folder").desired_width(380.0));
                    if ui.button("Set").clicked() && !d.dir.trim().is_empty() {
                        cmds.push(("fetch.dir.set", json!({ "path": d.dir.trim() })));
                    }
                });
                ui.add_space(6.0);
                widgets::eyebrow(ui, "", "Indexers", "");
                let ixs: Vec<(u32, String, bool)> = d
                    .host
                    .config()
                    .indexers
                    .iter()
                    .map(|i| (i.id, i.name.clone(), !i.api_key.is_empty()))
                    .collect();
                for (id, name, key) in &ixs {
                    ui.horizontal(|ui| {
                        ui.label(format!("{name}{}", if *key { "  · key set" } else { "" }));
                        if ui.small_button("Remove").clicked() {
                            cmds.push(("fetch.indexers.remove", json!({ "id": id })));
                        }
                    });
                }
                if ixs.is_empty() {
                    ui.label(RichText::new("None. No indexer is bundled: add your own Torznab endpoint.").color(FAINT));
                }
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut d.ix_name).hint_text("name").desired_width(90.0));
                    ui.add(TextEdit::singleline(&mut d.ix_url).hint_text("torznab url").desired_width(230.0));
                    ui.add(TextEdit::singleline(&mut d.ix_key).hint_text("api key").password(true).desired_width(110.0));
                    if ui.button("Add").clicked() && !d.ix_url.trim().is_empty() {
                        cmds.push((
                            "fetch.indexers.add",
                            json!({ "name": d.ix_name.trim(), "url": d.ix_url.trim(), "api_key": d.ix_key }),
                        ));
                        d.ix_name.clear();
                        d.ix_url.clear();
                        d.ix_key.clear();
                    }
                });
                ui.add_space(6.0);
                widgets::eyebrow(ui, "", "Search", "");
                ui.horizontal(|ui| {
                    let r = ui.add(TextEdit::singleline(&mut d.query).hint_text("query").desired_width(380.0));
                    let go = ui.button(if d.searching.is_some() { "Cancel" } else { "Search" });
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if d.searching.is_some() && go.clicked() {
                        cmds.push(("fetch.search.cancel", json!({})));
                    } else if (go.clicked() || enter) && !d.query.trim().is_empty() {
                        cmds.push(("fetch.search", json!({ "query": d.query.trim() })));
                    }
                });
                egui::ScrollArea::vertical().id_salt("dl-results").max_height(160.0).show(ui, |ui| {
                    for (i, r) in d.results.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let title = if discreet { format!("Result {}", i + 1) } else { r.title.clone() };
                            ui.label(title);
                            ui.label(
                                RichText::new(format!(
                                    "{}  S{}",
                                    r.size.map(human).unwrap_or_default(),
                                    r.seeders.unwrap_or(0)
                                ))
                                .color(MUTED),
                            );
                            if ui.small_button("Add").clicked() {
                                cmds.push(("fetch.add", json!({ "result": i })));
                            }
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut d.magnet).hint_text("magnet link").desired_width(380.0));
                    if ui.button("Add magnet").clicked() && d.magnet.trim().starts_with("magnet:") {
                        cmds.push(("fetch.add", json!({ "magnet": d.magnet.trim() })));
                        d.magnet.clear();
                    }
                });
                ui.add_space(6.0);
                widgets::eyebrow(ui, "", "Torrents", "");
                if ui.small_button("Load saved torrents").clicked()
                    && let Ok(svc) = d.host.service()
                {
                    drop(svc.start());
                }
                for t in &d.torrents {
                    ui.horizontal(|ui| {
                        let name = if discreet { format!("Torrent {}", t.id) } else { t.name.clone() };
                        ui.label(name);
                        ui.add(ProgressBar::new(t.progress()).desired_width(140.0).show_percentage());
                        ui.label(RichText::new(format!("{:?}  ↓{}/s", t.state, human(t.down_bps))).color(MUTED));
                        let paused = t.state == TorrentState::Paused;
                        if ui.small_button(if paused { "Resume" } else { "Pause" }).clicked() {
                            cmds.push((if paused { "fetch.resume" } else { "fetch.pause" }, json!({ "id": t.id })));
                        }
                        if ui.small_button("Forget").on_hover_text("Stops tracking. Files are kept.").clicked() {
                            cmds.push(("fetch.remove", json!({ "id": t.id })));
                        }
                    });
                }
            });
        self.downloads.open = open;
        for (n, a) in cmds {
            self.push(n, a);
        }
    }
}
