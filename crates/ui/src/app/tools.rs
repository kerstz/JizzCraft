//! Library maintenance jobs (fingerprints, integrity, backup, metadata
//! export/import, cache clearing) and the menu that starts them. Every job
//! runs on its own thread with its own database connection and reports one
//! toast with counts only, never file names.
//!
//! A toast says "412 checked", never which 412. Discretion is a number.

use super::{JizzApp, JobMsg};
use crate::theme::{self, MUTED};
use egui::Ui;
use jizzcraft_core::integrity::Scope;
use jizzcraft_core::{Db, Error, Registry};
use jizzcraft_media::integrity::{CacheKind, CheckMode, IntegrityOpts, clear_cache, run_integrity};
use serde_json::{Value, json};
use std::path::PathBuf;

impl JizzApp {
    /// Run `f` with a private connection, off the UI thread.
    pub(super) fn spawn_job(
        &mut self,
        label: &'static str,
        f: impl FnOnce(&mut Db) -> Result<String, String> + Send + 'static,
    ) {
        let (tx, ctx, opener) = (self.tx.clone(), self.ctx.clone(), self.opener.clone());
        let ticket = super::JobTicket::new(&self.jobs);
        self.busy += 1;
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = opener
                .open()
                .map_err(|e| e.to_string())
                .and_then(|mut db| f(&mut db));
            let _ = tx.send(JobMsg::Done { label, result });
            ctx.request_repaint();
        });
    }

    /// Menu content; returns commands to push once the menu closes.
    pub(super) fn tools_menu(&self, ui: &mut Ui) -> Vec<(&'static str, Value)> {
        let mut out: Vec<(&'static str, Value)> = Vec::new();
        let mut item = |ui: &mut Ui, label: &str, cmd: &'static str, args: Value| {
            if ui.button(label).clicked() {
                out.push((cmd, args));
                ui.close();
            }
        };
        item(
            ui,
            crate::i18n::tr("Rescan folders"),
            "library.rescan",
            json!({}),
        );
        if self.remove_cancel.is_some() {
            item(
                ui,
                crate::i18n::tr("Cancel folder removal"),
                "library.remove.cancel",
                json!({}),
            );
        }
        ui.separator();
        item(
            ui,
            crate::i18n::tr("Fingerprint files (for move detection)"),
            "library.fingerprint",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Compute visual fingerprints (for duplicates)"),
            "library.phash",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Find possible duplicates…"),
            "duplicates.open",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Check integrity (quick)"),
            "library.integrity",
            json!({ "mode": "quick" }),
        );
        item(
            ui,
            crate::i18n::tr("Check integrity (full decode + checksum)"),
            "library.integrity",
            json!({ "mode": "full", "checksum": true, "scope": "all" }),
        );
        ui.separator();
        item(
            ui,
            crate::i18n::tr("Back up library…"),
            "library.backup",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Export favorites, ratings, markers…"),
            "meta.export",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Import metadata…"),
            "meta.import",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Import from other apps…"),
            "interop.open",
            json!({}),
        );
        ui.separator();
        ui.label(theme::hud(crate::i18n::tr("Privacy"), MUTED));
        if self.opener.key.is_some() {
            item(
                ui,
                crate::i18n::tr("Lock library"),
                "app.vault.lock",
                json!({}),
            );
        }
        let paused = self.db.setting("history.enabled").ok().flatten().as_deref() == Some("0");
        item(
            ui,
            if paused {
                crate::i18n::tr("Resume recording history")
            } else {
                crate::i18n::tr("Pause recording history")
            },
            "history.pause",
            json!({ "paused": !paused }),
        );
        item(
            ui,
            if self.cfg.hide_history {
                crate::i18n::tr("Show history in the app")
            } else {
                crate::i18n::tr("Hide history in the app")
            },
            "history.visible.toggle",
            json!({}),
        );
        item(
            ui,
            crate::i18n::tr("Clear watch history"),
            "history.clear",
            json!({}),
        );
        item(
            ui,
            if self.cfg.disguise {
                crate::i18n::tr("Launch as JizzCraft (next start)")
            } else {
                crate::i18n::tr("Launch as \"Viewer\" (next start)")
            },
            "app.disguise.set",
            json!({ "on": !self.cfg.disguise }),
        );
        item(
            ui,
            &format!(
                "{}: {}",
                crate::i18n::tr("Playful messages"),
                if self.cfg.no_jokes {
                    crate::i18n::tr("off")
                } else {
                    crate::i18n::tr("on")
                }
            ),
            "app.jokes.toggle",
            json!({}),
        );
        item(
            ui,
            &format!(
                "{}: {}",
                crate::i18n::tr("Skip the age gate"),
                if self.cfg.skip_age_gate {
                    crate::i18n::tr("on")
                } else {
                    crate::i18n::tr("off")
                }
            ),
            "app.age.skip",
            json!({}),
        );
        item(
            ui,
            &format!(
                "{}: {}",
                crate::i18n::tr("Panic video"),
                if self.cfg.panic_video.is_some() {
                    crate::i18n::tr("set")
                } else {
                    crate::i18n::tr("none")
                }
            ),
            "app.panic.set",
            json!({}),
        );
        if self.cfg.panic_video.is_some() {
            item(
                ui,
                crate::i18n::tr("Forget the panic video"),
                "app.panic.set",
                json!({ "clear": true }),
            );
        }
        ui.separator();
        for l in crate::i18n::ALL {
            let mark = if crate::i18n::current() == l {
                "● "
            } else {
                "   "
            };
            let label = format!("{mark}{}", l.native_name());
            item(ui, &label, "app.language", json!({ "lang": l.code() }));
        }
        item(
            ui,
            crate::i18n::tr("Clear preview cache"),
            "cache.clear",
            json!({ "what": "previews" }),
        );
        item(
            ui,
            crate::i18n::tr("Clear thumbnail cache"),
            "cache.clear",
            json!({ "what": "thumbs" }),
        );
        out
    }
}

fn dest_or_dialog(args: &Value, key: &str, default_name: &str) -> Option<PathBuf> {
    match args.get(key).and_then(Value::as_str) {
        Some(p) => Some(PathBuf::from(p)),
        None => rfd::FileDialog::new()
            .set_file_name(default_name)
            .save_file(),
    }
}

pub(super) fn register_tool_commands(reg: &mut Registry<JizzApp>) {
    reg.register(
        "library.fingerprint",
        "Fingerprint files for move detection: {all?, limit?}",
        |app, args| {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let limit = args.get("limit").and_then(Value::as_u64);
            let cancel = app.lock_cancel.clone();
            app.spawn_job("Fingerprint", move |db| {
                let r = jizzcraft_core::fingerprint::fingerprint_pending(
                    db,
                    !all,
                    limit,
                    &cancel,
                    &mut |_, _| {},
                )
                .map_err(|e| e.to_string())?;
                Ok(format!(
                    "{} fingerprinted · {} failed · {} left",
                    r.hashed, r.failed, r.remaining
                ))
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "library.phash",
        "Compute visual fingerprints of videos for duplicate review: {all?, limit?}",
        |app, args| {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let limit = args.get("limit").and_then(Value::as_u64);
            let cancel = app.lock_cancel.clone();
            app.spawn_job("Visual fingerprints", move |db| {
                let r =
                    jizzcraft_media::phash::phash_pending(db, all, limit, &cancel, &mut |_, _| {})
                        .map_err(|e| e.to_string())?;
                Ok(format!(
                    "{} fingerprinted · {} failed · {} left",
                    r.hashed, r.failed, r.remaining
                ))
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "library.integrity",
        "Decode-test files: {mode?: quick|full, checksum?, scope?: unchecked|all, limit?}",
        |app, args| {
            let mut o = IntegrityOpts::default();
            match args.get("mode").and_then(Value::as_str) {
                None | Some("quick") => {}
                Some("full") => o.mode = CheckMode::Full,
                Some(m) => return Err(Error::Invalid(format!("unknown mode `{m}`"))),
            }
            match args.get("scope").and_then(Value::as_str) {
                None | Some("unchecked") => {}
                Some("all") => o.scope = Scope::All,
                Some(s) => return Err(Error::Invalid(format!("unknown scope `{s}`"))),
            }
            o.checksum = args
                .get("checksum")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            o.limit = args.get("limit").and_then(Value::as_u64);
            let cancel = app.lock_cancel.clone();
            app.spawn_job("Integrity", move |db| {
                let r =
                    run_integrity(db, &o, &cancel, &mut |_, _| {}).map_err(|e| e.to_string())?;
                Ok(format!(
                    "{} checked · {} ok · {} damaged · {} unreadable",
                    r.checked, r.ok, r.damaged, r.unreadable
                ))
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "library.backup",
        "Back up the library: {dest?} (save dialog when omitted)",
        |app, args| {
            let args2 = args.clone();
            app.spawn_job("Backup", move |db| {
                let dest = dest_or_dialog(&args2, "dest", "jizzcraft-backup.db")
                    .ok_or_else(|| "cancelled".to_owned())?;
                let n =
                    jizzcraft_core::portability::backup(db, &dest).map_err(|e| e.to_string())?;
                Ok(format!("Backup written ({} KB)", n / 1024))
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "meta.export",
        "Export favorites, ratings, markers: {dest?, include_history?, include_names?}",
        |app, args| {
            // An encrypted library exports a clear file: say so first, once,
            // in front of the file dialog. Scripted calls with a `dest` (or
            // `ack_clear`) are the user's explicit choice and go through.
            if app.opener.key.is_some()
                && args.get("dest").is_none()
                && args.get("ack_clear").and_then(Value::as_bool) != Some(true)
            {
                app.vault_ui.warn_export(args.clone());
                return Ok(json!({ "warning": "export is not encrypted" }));
            }
            let args2 = args.clone();
            let o = jizzcraft_core::portability::ExportOpts {
                include_history: args
                    .get("include_history")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                include_names: args
                    .get("include_names")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            app.spawn_job("Export", move |db| {
                let dest = dest_or_dialog(&args2, "dest", "jizzcraft-metadata.json")
                    .ok_or_else(|| "cancelled".to_owned())?;
                let r = jizzcraft_core::portability::export_meta(db, &dest, &o)
                    .map_err(|e| e.to_string())?;
                let mut t = format!("{} items exported", r.items);
                if r.skipped_no_fp > 0 {
                    t += &format!(" · {} skipped (run Fingerprint first)", r.skipped_no_fp);
                }
                Ok(t)
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "meta.import",
        "Import an export onto matching files: {src?, dry_run?}",
        |app, args| {
            let dry = args
                .get("dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let src = args.get("src").and_then(Value::as_str).map(PathBuf::from);
            app.spawn_job("Import", move |db| {
                let src = src
                    .or_else(|| {
                        rfd::FileDialog::new()
                            .add_filter("JSON", &["json"])
                            .pick_file()
                    })
                    .ok_or_else(|| "cancelled".to_owned())?;
                let r = jizzcraft_core::portability::import_meta(db, &src, dry)
                    .map_err(|e| e.to_string())?;
                Ok(format!(
                    "{} matched · {} applied · {} unmatched · {} ambiguous",
                    r.matched, r.applied, r.unmatched, r.ambiguous
                ))
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "cache.clear",
        "Delete cached previews/thumbnails: {what?: previews|thumbs|all}",
        |app, args| {
            let what = match args.get("what").and_then(Value::as_str) {
                None | Some("all") => CacheKind::All,
                Some("previews") => CacheKind::Previews,
                Some("thumbs") => CacheKind::Thumbs,
                Some(w) => return Err(Error::Invalid(format!("unknown cache `{w}`"))),
            };
            app.spawn_job("Cache", move |_| {
                let n = clear_cache(&jizzcraft_core::paths::cache_dir(), what)?;
                Ok(format!("{} KB freed", n / 1024))
            });
            Ok(json!({ "started": true }))
        },
    );
}
