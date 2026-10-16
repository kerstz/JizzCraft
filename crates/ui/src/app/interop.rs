//! "Import from other apps": a window to read other tools' files (Kodi-style
//! NFO files, Stash exports) onto matching library items. Always a
//! preview first (the same code as the import, rolled back, so its counts are
//! exact); the Import button stays disabled until a preview of the same
//! folder and options has run. Counts only, never names; files are only read.

use super::{JizzApp, JobMsg};
use crate::i18n::tr;
use crate::theme::{self, FAINT, MUTED, S};
use crate::widgets;
use jizzcraft_core::interop::{ImportOpts, InteropReport, opts_from_args};
use jizzcraft_core::{Error, Registry};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum Source {
    #[default]
    Nfo,
    Stash,
}

/// What a result was computed for.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Key {
    source: Source,
    folder: Option<PathBuf>,
    overwrite: bool,
    ratings: bool,
    history: bool,
}

type Slot = Arc<Mutex<Option<(Key, InteropReport)>>>;

#[derive(Default)]
pub(super) struct InteropUi {
    pub open: bool,
    source: Source,
    folder: Option<PathBuf>,
    overwrite: bool,
    ratings: bool,
    history: bool,
    slot: Slot,
    running: Arc<AtomicBool>,
}

impl InteropUi {
    fn key(&self) -> Key {
        Key {
            source: self.source,
            folder: self.folder.clone(),
            overwrite: self.overwrite,
            ratings: self.ratings,
            history: self.history,
        }
    }
}

fn toast_text(r: &InteropReport) -> String {
    format!(
        "{} matched · {} unmatched · {} ambiguous · {} rejected",
        r.matched, r.unmatched, r.ambiguous, r.rejected
    )
}

impl JizzApp {
    /// The folder dialog runs on its own thread and answers with a message.
    fn interop_pick_folder(&mut self) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let folder = rfd::FileDialog::new()
                .set_title(tr("Choose…"))
                .pick_folder();
            let _ = tx.send(JobMsg::InteropFolder(folder));
            ctx.request_repaint();
        });
    }

    pub(super) fn interop_folder_chosen(&mut self, folder: Option<PathBuf>) {
        if let Some(f) = folder {
            self.interop.folder = Some(f);
        }
    }

    pub(super) fn interop_window(&mut self, ctx: &egui::Context) {
        if !self.interop.open {
            return;
        }
        let discreet = self.cfg.discreet || self.opts.disguised;
        let mut open = true;
        let mut cmds: Vec<(&'static str, Value)> = Vec::new();
        let mut pick = false;
        let result = self.interop.slot.lock().ok().and_then(|g| g.clone());
        let running = self.interop.running.load(Ordering::Relaxed);
        let editable = !self.read_only && !running;
        let folder_text = match (&self.interop.folder, discreet) {
            (_, true) => "—".to_owned(),
            (Some(p), false) => p.to_string_lossy().into_owned(),
            (None, false) if self.interop.source == Source::Nfo => tr("Library folders").to_owned(),
            (None, false) => "—".to_owned(),
        };
        let mut ui_state = std::mem::take(&mut self.interop);
        egui::Window::new(tr("Import from other apps"))
            .open(&mut open)
            .id(egui::Id::new("interop-window"))
            .default_width(460.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(theme::hud(tr("Source"), FAINT));
                ui.radio_value(
                    &mut ui_state.source,
                    Source::Nfo,
                    tr("NFO files (Kodi, Jellyfin, Emby, Plex)"),
                );
                ui.add_enabled_ui(editable, |ui| {
                    ui.radio_value(
                        &mut ui_state.source,
                        Source::Stash,
                        tr("Stash export folder"),
                    );
                });
                ui.add_space(S);
                ui.label(theme::hud(tr("Folder"), FAINT));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&folder_text).size(12.0).color(MUTED))
                            .truncate(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(editable, |ui| {
                        if ui.button(tr("Choose…")).clicked() {
                            pick = true;
                        }
                        if ui_state.source == Source::Nfo
                            && ui_state.folder.is_some()
                            && ui.button(tr("Library folders")).clicked()
                        {
                            ui_state.folder = None;
                        }
                    });
                });
                ui.add_space(S);
                ui.add_enabled_ui(editable, |ui| {
                    ui.checkbox(&mut ui_state.overwrite, tr("Replace existing titles"));
                    ui.checkbox(&mut ui_state.ratings, tr("Import ratings"));
                    ui.checkbox(&mut ui_state.history, tr("Import play history"));
                });
                ui.add_space(S);
                ui.add(
                    egui::Label::new(egui::RichText::new(tr(
                        "Files are only read. Nothing changes until you press Import, and the counts are exact.",
                    ))
                    .size(11.0)
                    .color(FAINT))
                    .wrap(),
                );
                ui.add_space(S);
                let previewed = result
                    .as_ref()
                    .is_some_and(|(k, r)| *k == ui_state.key() && r.dry_run);
                let (cmd, ready) = match ui_state.source {
                    Source::Nfo => ("meta.import_nfo", true),
                    Source::Stash => ("meta.import_stash", ui_state.folder.is_some()),
                };
                let args = |dry: bool| {
                    let mut a = json!({
                        "dry_run": dry, "overwrite": ui_state.overwrite,
                        "ratings": ui_state.ratings, "history": ui_state.history,
                    });
                    if let Some(f) = &ui_state.folder {
                        a["dir"] = json!(f.to_string_lossy());
                    }
                    a
                };
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(editable && ready, |ui| {
                        if widgets::button_outline(ui, tr("Preview")).clicked() {
                            cmds.push((cmd, args(true)));
                        }
                    });
                    ui.add_enabled_ui(editable && ready && previewed, |ui| {
                        if widgets::button_solid(ui, tr("Import")).clicked() {
                            cmds.push((cmd, args(false)));
                        }
                    });
                });
                if let Some((k, r)) = &result
                    && *k == ui_state.key()
                {
                    ui.add_space(S);
                    widgets::hairline_h(ui);
                    ui.add_space(S);
                    ui.label(theme::hud(tr("Result"), FAINT));
                    let created = r.studios_created + r.people_created + r.tags_created;
                    for (label, n) in [
                        (tr("Matched"), r.matched),
                        (tr("Changed"), r.applied),
                        (tr("Unmatched"), r.unmatched),
                        (tr("Ambiguous"), r.ambiguous),
                        (tr("Rejected"), r.rejected),
                        (tr("Created"), created),
                    ] {
                        widgets::kv(ui, label, &n.to_string());
                    }
                }
                ui.add_space(S);
                if widgets::button_outline(ui, tr("Close")).clicked() {
                    ui_state.open = false;
                }
            });
        ui_state.open = ui_state.open && open;
        self.interop = ui_state;
        if pick {
            self.interop_pick_folder();
        }
        for (n, a) in cmds {
            self.push(n, a);
        }
    }
}

pub(super) fn register(reg: &mut Registry<JizzApp>) {
    reg.register(
        "interop.open",
        "Open the import window (NFO files, Stash exports)",
        |app, _| {
            app.interop.open = true;
            Ok(json!({}))
        },
    );
    // Same names as the core commands: in the UI the work runs as a job.
    reg.register(
        "meta.import_nfo",
        "Read Kodi-style .nfo files onto matching items (preview unless dry_run is false): {dir?, dry_run?=true, overwrite?, ratings?, history?}",
        |app, args| start(app, Source::Nfo, &args),
    );
    reg.register(
        "meta.import_stash",
        "Read a Stash JSON export folder onto matching items (preview unless dry_run is false): {dir, dry_run?=true, overwrite?, ratings?, history?}",
        |app, args| start(app, Source::Stash, &args),
    );
}

fn start(app: &mut JizzApp, source: Source, args: &Value) -> jizzcraft_core::Result<Value> {
    let o: ImportOpts = opts_from_args(args)?;
    let dir = args.get("dir").and_then(Value::as_str).map(PathBuf::from);
    if source == Source::Stash && dir.is_none() {
        return Err(Error::Invalid("missing string argument `dir`".into()));
    }
    if app.read_only && !o.dry_run {
        return Err(Error::Invalid("the library is read-only".into()));
    }
    let key = Key {
        source,
        folder: dir.clone(),
        overwrite: o.overwrite,
        ratings: o.ratings,
        history: o.history,
    };
    let (slot, running, cancel) = (
        app.interop.slot.clone(),
        app.interop.running.clone(),
        app.lock_cancel.clone(),
    );
    running.store(true, Ordering::Relaxed);
    let label = match (source, o.dry_run) {
        (Source::Nfo, true) => "NFO preview",
        (Source::Nfo, false) => "NFO import",
        (Source::Stash, true) => "Stash preview",
        (Source::Stash, false) => "Stash import",
    };
    app.spawn_job(label, move |db| {
        let res = match (source, dir.as_deref()) {
            (Source::Nfo, d) => jizzcraft_core::interop::nfo::import_nfo(db, d, &o, &cancel),
            (Source::Stash, Some(d)) => {
                jizzcraft_core::interop::stash::import_stash(db, d, &o, &cancel)
            }
            (Source::Stash, None) => Err(Error::Invalid("missing folder".into())),
        }
        .map_err(|e| e.to_string());
        running.store(false, Ordering::Relaxed);
        let rep = res?;
        let text = toast_text(&rep);
        if let Ok(mut g) = slot.lock() {
            *g = Some((key, rep));
        }
        Ok(text)
    });
    Ok(json!({ "started": true }))
}
