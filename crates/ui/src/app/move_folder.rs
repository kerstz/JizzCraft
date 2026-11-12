//! "This folder's files moved…": a library folder whose files are now at
//! another path (a drive that mounts elsewhere, a folder renamed outside the
//! app). The user picks the new place; a dry run counts what would follow;
//! the dialog shows the counts and asks; the move runs as a job with its own
//! connection and ends with one toast of counts. No name or path is shown in
//! discreet mode, none ever reaches a toast or a log.

use super::{JizzApp, JobMsg, JobTicket};
use crate::theme::{MUTED, S, TEXT};
use crate::widgets;
use egui::{Align2, vec2};
use jizzcraft_core::Registry;
use jizzcraft_core::relocate::{MoveReport, move_root};
use serde_json::{Value, json};
use std::path::PathBuf;

/// A counted move waiting for the user's answer.
pub(super) struct MoveAsk {
    from: PathBuf,
    to: PathBuf,
    /// The folder's own name, shown unless discreet mode is on.
    name: String,
    rep: MoveReport,
}

/// The dialog's text, built from counts only.
pub(super) fn summary(rep: &MoveReport) -> Vec<String> {
    let mut lines = vec![crate::i18n::trf(
        "{} of {} items are at the new place.",
        &[&rep.found.to_string(), &rep.items.to_string()],
    )];
    if rep.different + rep.not_found > 0 {
        lines.push(crate::i18n::trf(
            "{} are other files there and {} are not there: they stay in Missing.",
            &[&rep.different.to_string(), &rep.not_found.to_string()],
        ));
    }
    lines.push(
        crate::i18n::tr(
            "Ratings, history, markers, tags and playlists follow the items. No file is changed, and a backup of the library is kept.",
        )
        .to_owned(),
    );
    lines
}

/// The toast after the move: counts only.
pub(super) fn done_text(rep: &MoveReport) -> String {
    format!(
        "{} moved · {} different · {} not found",
        rep.found, rep.different, rep.not_found
    )
}

impl JizzApp {
    /// The folder dialog closed: count what would follow, off the UI thread.
    pub(super) fn move_folder_picked(&mut self, from: PathBuf, name: String, to: Option<PathBuf>) {
        let Some(to) = to else { return };
        let (tx, ctx, opener) = (self.tx.clone(), self.ctx.clone(), self.opener.clone());
        let cancel = self.lock_cancel.clone();
        let ticket = JobTicket::new(&self.jobs);
        self.busy += 1;
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = opener
                .open()
                .and_then(|mut db| move_root(&mut db, &from, &to, true, &cancel))
                .map_err(sentence);
            let _ = tx.send(JobMsg::MoveCounted {
                from,
                to,
                name,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// The dry run ended: ask, or say why not.
    pub(super) fn move_folder_counted(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        name: String,
        result: Result<MoveReport, String>,
    ) {
        self.busy = self.busy.saturating_sub(1);
        match result {
            Ok(rep) if rep.cancelled => {}
            Ok(rep) => {
                self.move_folder = Some(MoveAsk {
                    from,
                    to,
                    name,
                    rep,
                });
            }
            Err(e) => {
                let e = crate::i18n::tr_dyn(&e).into_owned();
                self.toast(super::ToastKind::Error, e);
            }
        }
    }

    pub(super) fn move_folder_dialog(&mut self, ctx: &egui::Context) {
        let Some(ask) = self.move_folder.take() else {
            return;
        };
        let (mut keep, mut confirm) = (true, false);
        let discreet = self.discreet();
        egui::Window::new(crate::i18n::tr("FOLDER MOVED"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let shown = if discreet { "—" } else { ask.name.as_str() };
                ui.label(egui::RichText::new(shown).color(TEXT));
                ui.add_space(S);
                for line in summary(&ask.rep) {
                    ui.label(egui::RichText::new(line).color(MUTED));
                }
                ui.add_space(S);
                ui.horizontal(|ui| {
                    if widgets::button_solid(ui, crate::i18n::tr("Move the items")).clicked() {
                        confirm = true;
                    }
                    if widgets::button_outline(ui, crate::i18n::tr("Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep = false;
        }
        if confirm {
            self.push(
                "library.move_root.apply",
                json!({ "from": ask.from, "to": ask.to }),
            );
        } else if keep {
            self.move_folder = Some(ask);
        }
    }
}

/// What a refusal says: the engine's own sentence (translated where it is
/// shown), never a path.
fn sentence(e: jizzcraft_core::Error) -> String {
    match e {
        jizzcraft_core::Error::Invalid(s) => s,
        jizzcraft_core::Error::NotFound(_) => "this folder is no longer in the library".into(),
        other => other.to_string(),
    }
}

fn path_arg(args: &Value, key: &str) -> jizzcraft_core::Result<PathBuf> {
    args.get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| jizzcraft_core::Error::Invalid(format!("missing `{key}`")))
}

pub(super) fn register(reg: &mut Registry<JizzApp>) {
    reg.register(
        "library.move_root.ask",
        "Pick where the files of a library folder are now, count what follows, then ask: {path, name?}",
        |app, args| {
            let from = path_arg(&args, "path")?;
            if !app.db.folders()?.iter().any(|(_, p)| *p == from) {
                return Err(jizzcraft_core::Error::NotFound("imported folder".into()));
            }
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let (tx, ctx) = (app.tx.clone(), app.ctx.clone());
            std::thread::spawn(move || {
                let to = rfd::FileDialog::new()
                    .set_title(crate::i18n::tr("Where are this folder's files now?"))
                    .pick_folder();
                let _ = tx.send(JobMsg::MovePicked { from, name, to });
                ctx.request_repaint();
            });
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "library.move_root.apply",
        "Move a library folder to where its files are now, as a job: {from, to}",
        |app, args| {
            let from = path_arg(&args, "from")?;
            let to = path_arg(&args, "to")?;
            if app.busy > 0 {
                return Err(jizzcraft_core::Error::Invalid(
                    crate::i18n::tr("Wait for the scan to finish first").into(),
                ));
            }
            let cancel = app.lock_cancel.clone();
            app.spawn_job("Folder moved", move |db| {
                move_root(db, &from, &to, false, &cancel)
                    .map(|rep| done_text(&rep))
                    .map_err(sentence)
            });
            Ok(json!({ "started": true }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(found: u64, different: u64, not_found: u64) -> MoveReport {
        MoveReport {
            items: found + different + not_found,
            found,
            different,
            not_found,
            ..MoveReport::default()
        }
    }

    #[test]
    fn texts_are_counts_and_translated() {
        crate::i18n::set(crate::i18n::Lang::Fr);
        assert_eq!(
            summary(&rep(3, 0, 0)),
            [
                "3 éléments sur 3 sont au nouvel emplacement.",
                "Notes, historique, repères, étiquettes et listes de lecture suivent les éléments. Aucun fichier n'est modifié et une sauvegarde de la bibliothèque est conservée.",
            ]
        );
        assert_eq!(
            summary(&rep(2, 1, 4))[1],
            "1 y sont d'autres fichiers et 4 n'y sont pas : ils restent dans Manquants."
        );
        assert_eq!(
            crate::i18n::tr_dyn(&done_text(&rep(2, 1, 4))),
            "2 déplacés · 1 différents · 4 introuvables"
        );
        for refusal in [
            "none of this folder's items is at the new place: nothing was changed",
            "the new folder is, contains or lies inside a library folder",
            "the new folder cannot be opened",
            "the new place is not a folder",
            "the library folder changed meanwhile: nothing was changed",
        ] {
            assert_ne!(crate::i18n::tr_dyn(refusal), refusal, "{refusal}");
            // The engine says exactly this, so the UI can translate it.
            assert_eq!(
                sentence(jizzcraft_core::Error::Invalid(refusal.into())),
                refusal
            );
        }
        assert_ne!(
            crate::i18n::tr_dyn(&sentence(jizzcraft_core::Error::NotFound("x".into()))),
            "this folder is no longer in the library"
        );
        crate::i18n::set(crate::i18n::Lang::En);
        assert_eq!(
            done_text(&rep(2, 1, 4)),
            "2 moved · 1 different · 4 not found"
        );
    }

    /// The UI translates the engine's refusals: they must stay the same text.
    #[test]
    fn engine_refusals_are_the_translated_sentences() {
        let src = include_str!("../../../core/src/relocate.rs");
        for refusal in [
            "none of this folder's items is at the new place: nothing was changed",
            "the new folder is, contains or lies inside a library folder",
            "the new folder cannot be opened",
            "the new place is not a folder",
            "the library folder changed meanwhile: nothing was changed",
        ] {
            assert!(
                src.contains(refusal),
                "relocate.rs no longer says: {refusal}"
            );
        }
    }
}
