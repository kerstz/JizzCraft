//! "Find possible duplicates": visual fingerprints of the videos (computed
//! in a job, decoded in memory only) grouped by how alike they look. Review
//! only: there is no delete action here. Counts in toasts, names in the
//! window only (and "—" in discreet mode).

use super::{JizzApp, ToastKind, fmt_duration, fmt_size};
use crate::i18n::{tr, trf};
use crate::theme::{self, FAINT, MUTED, S};
use crate::widgets;
use jizzcraft_core::phash::{DupReport, MAX_DISTANCE};
use jizzcraft_core::{Error, Registry};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Offered similarity levels: label, largest Hamming distance.
const LEVELS: [(&str, u32); 4] = [
    ("Exact", 0),
    ("High similarity", 4),
    ("Medium similarity", 8),
    ("Low similarity", 10),
];

/// At most this many groups are drawn (the list is already capped at 1000).
const MAX_SHOWN: usize = 200;

type Slot = Arc<Mutex<Option<(u32, DupReport)>>>;

pub(super) struct DupUi {
    pub open: bool,
    distance: u32,
    slot: Slot,
    running: Arc<AtomicBool>,
}

impl Default for DupUi {
    fn default() -> Self {
        DupUi {
            open: false,
            distance: 8,
            slot: Slot::default(),
            running: Arc::default(),
        }
    }
}

impl JizzApp {
    pub(super) fn duplicates_window(&mut self, ctx: &egui::Context) {
        if !self.dup.open {
            return;
        }
        let discreet = self.cfg.discreet || self.opts.disguised;
        let running = self.dup.running.load(Ordering::Relaxed);
        let result = self.dup.slot.lock().ok().and_then(|g| g.clone());
        let mut open = true;
        let mut distance = self.dup.distance;
        let mut cmds: Vec<(&'static str, Value)> = Vec::new();
        egui::Window::new(tr("Possible duplicates"))
            .open(&mut open)
            .id(egui::Id::new("duplicates-window"))
            .default_width(520.0)
            .default_height(480.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(tr(
                            "Videos that look alike, found by comparing 25 frames of each. Nothing is deleted here.",
                        ))
                        .size(11.0)
                        .color(FAINT),
                    )
                    .wrap(),
                );
                ui.add_space(S);
                ui.horizontal(|ui| {
                    ui.label(tr("Similarity"));
                    let shown = LEVELS
                        .iter()
                        .find(|(_, d)| *d == distance)
                        .map_or("—", |(l, _)| tr(l));
                    ui.add_enabled_ui(!running, |ui| {
                        egui::ComboBox::from_id_salt("dup-level")
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                for (label, d) in LEVELS {
                                    ui.selectable_value(&mut distance, d, tr(label));
                                }
                            });
                        if widgets::button_outline(ui, tr("Scan again")).clicked() {
                            cmds.push(("duplicates.find", json!({ "distance": distance })));
                        }
                    });
                });
                ui.add_space(S);
                widgets::hairline_h(ui);
                ui.add_space(S);
                if running {
                    widgets::mono_label(ui, tr("Working…"), MUTED);
                }
                match &result {
                    None if !running => {
                        widgets::mono_label(ui, tr("No result yet"), FAINT);
                    }
                    None => {}
                    Some((_, rep)) => {
                        widgets::kv(ui, tr("Fingerprinted videos"), &rep.hashed.to_string());
                        widgets::kv(ui, tr("Groups"), &rep.groups.len().to_string());
                        ui.add_space(S);
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(
                            ui,
                            |ui| {
                                for (n, g) in rep.groups.iter().take(MAX_SHOWN).enumerate() {
                                    ui.label(theme::hud(
                                        &trf(
                                            "Group {} · {} items",
                                            &[&(n + 1).to_string(), &g.items.len().to_string()],
                                        ),
                                        MUTED,
                                    ));
                                    for it in &g.items {
                                        ui.horizontal(|ui| {
                                            let name = if discreet { "—" } else { &it.name };
                                            ui.add(
                                                egui::Label::new(egui::RichText::new(name).size(12.0))
                                                    .truncate(),
                                            );
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} · {}",
                                                    it.duration_ms.map_or("—".into(), fmt_duration),
                                                    fmt_size(it.size)
                                                ))
                                                .size(11.0)
                                                .color(MUTED),
                                            );
                                            if ui.small_button(tr("Open")).clicked() {
                                                cmds.push(("duplicates.item", json!({ "id": it.id })));
                                            }
                                        });
                                    }
                                    let ids: Vec<i64> = g.items.iter().map(|i| i.id).collect();
                                    if ui.small_button(tr("Add group to a new playlist")).clicked() {
                                        cmds.push(("duplicates.playlist", json!({ "ids": ids })));
                                    }
                                    ui.add_space(S);
                                    widgets::hairline_h(ui);
                                    ui.add_space(S);
                                }
                                if rep.groups.is_empty() && !running {
                                    widgets::mono_label(ui, tr("No duplicates found"), FAINT);
                                }
                            },
                        );
                    }
                }
            });
        self.dup.open = open;
        if distance != self.dup.distance {
            self.dup.distance = distance;
            cmds.push((
                "duplicates.find",
                json!({ "distance": distance, "compute": false }),
            ));
        }
        for (n, a) in cmds {
            self.push(n, a);
        }
    }

    fn start_duplicates(&mut self, distance: u32, compute: bool) {
        self.dup.distance = distance;
        let (slot, running, cancel) = (
            self.dup.slot.clone(),
            self.dup.running.clone(),
            self.lock_cancel.clone(),
        );
        if running.swap(true, Ordering::Relaxed) {
            return;
        }
        self.spawn_job("Duplicates", move |db| {
            let mut work = || -> Result<String, String> {
                let (mut done, mut failed) = (0, 0);
                if compute {
                    let r = jizzcraft_media::phash::phash_pending(
                        db,
                        false,
                        None,
                        &cancel,
                        &mut |_, _| {},
                    )
                    .map_err(|e| e.to_string())?;
                    (done, failed) = (r.hashed, r.failed);
                }
                let rep = db
                    .duplicates(distance, None, 1000)
                    .map_err(|e| e.to_string())?;
                let text = format!(
                    "{} groups · {} fingerprinted · {} failed",
                    rep.groups.len(),
                    done,
                    failed
                );
                if let Ok(mut g) = slot.lock() {
                    *g = Some((distance, rep));
                }
                Ok(text)
            };
            let out = work();
            running.store(false, Ordering::Relaxed);
            out
        });
    }
}

pub(super) fn register(reg: &mut Registry<JizzApp>) {
    reg.register(
        "duplicates.open",
        "Open the duplicates window and refresh it (fingerprints videos that have none): {distance?}",
        |app, args| {
            let d = distance_arg(&args, app.dup.distance)?;
            app.dup.open = true;
            app.start_duplicates(d, true);
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "duplicates.find",
        "Group the fingerprinted videos: {distance?=8 (0..=16), compute?=true}",
        |app, args| {
            let d = distance_arg(&args, app.dup.distance)?;
            let compute = args.get("compute").and_then(Value::as_bool).unwrap_or(true);
            app.start_duplicates(d, compute);
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "duplicates.playlist",
        "Put a group in a new playlist: {ids}",
        |app, args| {
            if app.read_only {
                return Err(Error::Invalid("the library is read-only".into()));
            }
            let ids: Vec<i64> = args
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default();
            if ids.is_empty() {
                return Err(Error::Invalid("missing `ids`".into()));
            }
            let base = tr("Possible duplicates");
            let mut made = None;
            for n in 1..=99 {
                let name = if n == 1 {
                    base.to_owned()
                } else {
                    format!("{base} {n}")
                };
                match app.db.playlist_create(&name, None) {
                    Ok(id) => {
                        made = Some(id);
                        break;
                    }
                    Err(Error::Invalid(_)) => continue,
                    Err(e) => return Err(e),
                }
            }
            let id = made.ok_or_else(|| Error::Invalid("could not name the playlist".into()))?;
            let (added, _) = app.db.playlist_add(id, &ids, None)?;
            app.refresh();
            app.toast(
                ToastKind::Info,
                trf("{} items added to a new playlist", &[&added.to_string()]),
            );
            Ok(json!({ "id": id, "added": added }))
        },
    );
    reg.register(
        "duplicates.item",
        "Play one item of a group: {id}",
        |app, args| open_item(app, &args),
    );
}

fn distance_arg(args: &Value, default: u32) -> jizzcraft_core::Result<u32> {
    match args.get("distance") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|d| *d <= u64::from(MAX_DISTANCE))
            .map(|d| d as u32)
            .ok_or_else(|| Error::Invalid("`distance` must be 0 to 16".into())),
    }
}

fn open_item(app: &mut JizzApp, args: &Value) -> jizzcraft_core::Result<Value> {
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Invalid("missing integer argument `id`".into()))?;
    app.open_by_id(id);
    Ok(json!({}))
}
