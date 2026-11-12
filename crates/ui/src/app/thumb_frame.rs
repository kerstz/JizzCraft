//! Choosing the frame a video's thumbnail is made from: "Use this frame" in
//! the inspector while the video plays, "Automatic" to go back. Only the
//! database changes; the grid asks for the new thumbnail at once. Toasts
//! name no file.

use super::{JizzApp, ToastKind, View, fmt_duration};
use crate::theme::{self, FAINT, S, TEXT};
use crate::widgets;
use egui::Ui;
use jizzcraft_core::{Error, Media, MediaKind, Registry};
use serde_json::{Value, json};

impl JizzApp {
    /// Drop what is shown for `id` so its thumbnail is asked for again.
    pub(super) fn forget_thumb(&mut self, id: i64) {
        self.textures.remove(&id);
        self.thumb_failed.remove(&id);
        self.refresh();
    }

    /// The playing library video and the position on screen, if any.
    fn playing_frame(&self) -> Option<(i64, u64)> {
        match &self.view {
            View::Player(pv) => pv
                .media_id
                .map(|id| (id, pv.player.position().as_millis() as u64)),
            _ => None,
        }
    }

    /// Inspector section for a video: where its thumbnail comes from.
    pub(super) fn thumb_section(&mut self, ui: &mut Ui, m: &Media) {
        if m.kind != MediaKind::Video {
            return;
        }
        ui.add_space(S);
        ui.label(theme::hud(crate::i18n::tr("Thumbnail"), FAINT));
        let source = match m.thumb_ms {
            Some(ms) => crate::i18n::trf("From the frame at {}", &[&fmt_duration(ms)]),
            None => crate::i18n::tr("Automatic").to_owned(),
        };
        ui.label(egui::RichText::new(source).size(11.0).color(TEXT));
        let playing_this = self.playing_frame().is_some_and(|(id, _)| id == m.id);
        ui.horizontal(|ui| {
            if playing_this
                && widgets::button_outline(ui, crate::i18n::tr("Use this frame"))
                    .on_hover_text(crate::i18n::tr(
                        "Make the thumbnail from the picture on screen now",
                    ))
                    .clicked()
            {
                self.push("media.thumb.here", json!({}));
            }
            if m.thumb_ms.is_some()
                && widgets::button_outline(ui, crate::i18n::tr("Automatic")).clicked()
            {
                self.push("media.thumb.auto", json!({ "id": m.id }));
            }
        });
    }
}

pub(super) fn register(reg: &mut Registry<JizzApp>) {
    reg.register(
        "media.thumb.here",
        "Make the playing video's thumbnail from the frame on screen",
        |app, _| {
            let (id, ms) = app
                .playing_frame()
                .ok_or_else(|| Error::Invalid("play a video of the library first".into()))?;
            app.db.set_thumb_ms(id, Some(ms))?;
            app.forget_thumb(id);
            app.toast(
                ToastKind::Info,
                crate::i18n::tr("The thumbnail now shows this frame"),
            );
            Ok(json!({ "id": id, "thumb_ms": ms }))
        },
    );
    reg.register(
        "media.thumb.auto",
        "Make a video's thumbnail automatically again: {id?} (default: the selected item)",
        |app, args| {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .or(app.selected)
                .ok_or_else(|| Error::Invalid("select a video first".into()))?;
            app.db.set_thumb_ms(id, None)?;
            app.forget_thumb(id);
            Ok(json!({ "id": id, "thumb_ms": null }))
        },
    );
}
