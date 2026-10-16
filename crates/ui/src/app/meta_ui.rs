//! Inspector section for the descriptive metadata of the selected item (title,
//! release date, studio, performers) and the tag-alias window. Every change
//! goes through a named command (`media.info.set`, `studio.assign`,
//! `person.assign`…); nothing here touches a file. Discreet mode shows "—"
//! instead of the title, studio, performers and aliases.

use super::{JizzApp, ToastKind};
use crate::i18n::tr;
use crate::theme::{self, FAINT, MUTED, S, TEXT};
use egui::{Align2, Key, Ui, vec2};
use jizzcraft_core::meta::{MediaInfo, Person, Studio};
use serde_json::json;

/// Edit buffers of the inspector. Keyed by the item they were loaded for, so a
/// half-typed title never lands on another item.
#[derive(Default)]
pub(super) struct InfoUi {
    media: i64,
    generation: u64,
    title: String,
    person: String,
    new_studio: Option<String>,
}

/// The tag whose aliases are being edited.
pub(super) struct AliasEdit {
    pub tag: i64,
    pub input: String,
}

impl JizzApp {
    /// Metadata of one item, cached per library generation.
    fn info_of(&mut self, id: i64) -> Option<MediaInfo> {
        if let Some((cid, gen_, v)) = &self.info_cache
            && *cid == id
            && *gen_ == self.generation
        {
            return Some(v.clone());
        }
        let v = self.db.media_info(id).ok()?;
        self.info_cache = Some((id, self.generation, v.clone()));
        Some(v)
    }

    fn studios_cached(&mut self) -> Vec<Studio> {
        if let Some((gen_, v)) = &self.studio_cache
            && *gen_ == self.generation
        {
            return v.clone();
        }
        let v = self.db.studios().unwrap_or_default();
        self.studio_cache = Some((self.generation, v.clone()));
        v
    }

    fn meta_error(&mut self, e: &jizzcraft_core::Error) {
        self.toast(ToastKind::Error, e.to_string());
    }

    /// Add a performer by name to one item: an existing performer (by name or
    /// alias) or a new one.
    fn info_add_person(&mut self, media: i64, text: &str) {
        let name = text.trim();
        if name.is_empty() {
            return;
        }
        let lc = name.to_lowercase();
        let matches: Vec<Person> = self
            .db
            .people(Some(name), 50)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                p.name.to_lowercase() == lc || p.aliases.iter().any(|a| a.to_lowercase() == lc)
            })
            .collect();
        let id = match matches
            .iter()
            .find(|p| p.disambiguation.is_empty())
            .or(matches.first())
        {
            Some(p) => p.id,
            None => match self.db.person_create(name, "") {
                Ok(id) => id,
                Err(e) => return self.meta_error(&e),
            },
        };
        self.push(
            "person.assign",
            json!({ "ids": [id], "media_ids": [media] }),
        );
    }

    /// An existing studio by name, or a new one, set on one item.
    fn info_new_studio(&mut self, media: i64, text: &str) {
        let name = text.trim();
        if name.is_empty() {
            return;
        }
        let lc = name.to_lowercase();
        let found = self
            .db
            .studios()
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.name.to_lowercase() == lc)
            .map(|s| s.id);
        let id = match found {
            Some(id) => id,
            None => match self.db.studio_create(name) {
                Ok(id) => id,
                Err(e) => return self.meta_error(&e),
            },
        };
        self.push(
            "studio.assign",
            json!({ "media_ids": [media], "studio": id }),
        );
    }

    /// Title, release date, studio and performers of `id`.
    pub(super) fn inspector_details(&mut self, ui: &mut Ui, id: i64, discreet: bool) {
        let Some(info) = self.info_of(id) else {
            return;
        };
        let editable = !self.read_only && !discreet;
        if self.info_ui.media != id
            || (self.info_ui.generation != self.generation && !ui.memory(|m| m.focused().is_some()))
        {
            self.info_ui.media = id;
            self.info_ui.generation = self.generation;
            self.info_ui.title = info.title.clone().unwrap_or_default();
            self.info_ui.person.clear();
            self.info_ui.new_studio = None;
        }

        ui.add_space(S);
        ui.label(theme::hud(tr("Title"), FAINT));
        if discreet {
            ui.label(egui::RichText::new("—").size(12.0).color(TEXT));
        } else {
            let resp = ui.add_enabled(
                editable,
                egui::TextEdit::singleline(&mut self.info_ui.title)
                    .desired_width(f32::INFINITY)
                    .char_limit(200)
                    .hint_text(tr("Title")),
            );
            let current = info.title.clone().unwrap_or_default();
            if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.info_ui.title = current.clone();
                resp.surrender_focus();
            } else if resp.lost_focus() && self.info_ui.title.trim() != current {
                let t = self.info_ui.title.trim().to_owned();
                self.push("media.info.set", json!({ "id": id, "title": t }));
            }
        }
        if let Some(d) = &info.released {
            crate::widgets::kv(ui, tr("Released"), if discreet { "—" } else { d });
        }

        ui.add_space(S);
        ui.label(theme::hud(tr("Studio"), FAINT));
        if discreet {
            ui.label(egui::RichText::new("—").size(12.0).color(TEXT));
        } else {
            let studios = self.studios_cached();
            let shown = info
                .studio
                .as_ref()
                .map_or_else(|| "—".to_owned(), |s| s.name.clone());
            let mut set: Option<Option<i64>> = None;
            ui.add_enabled_ui(editable, |ui| {
                egui::ComboBox::from_id_salt("info-studio")
                    .selected_text(shown)
                    .width(ui.available_width() - 8.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(info.studio.is_none(), tr("No studio"))
                            .clicked()
                        {
                            set = Some(None);
                        }
                        for s in studios.iter().take(500) {
                            let on = info.studio.as_ref().is_some_and(|c| c.id == s.id);
                            if ui.selectable_label(on, &s.name).clicked() {
                                set = Some(Some(s.id));
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(false, tr("New studio…")).clicked() {
                            self.info_ui.new_studio = Some(String::new());
                        }
                    });
            });
            if let Some(s) = set {
                self.push("studio.assign", json!({ "media_ids": [id], "studio": s }));
            }
            if let Some(text) = &mut self.info_ui.new_studio {
                let mut done = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(ui.available_width() - 64.0)
                            .char_limit(60)
                            .hint_text(tr("New studio name")),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        done = true;
                    }
                    if ui.button(tr("Add")).clicked() {
                        done = true;
                    }
                });
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                if done {
                    let t = text.clone();
                    self.info_ui.new_studio = None;
                    self.info_new_studio(id, &t);
                } else if cancel {
                    self.info_ui.new_studio = None;
                }
            }
        }

        ui.add_space(S);
        ui.label(theme::hud(tr("Performers"), FAINT));
        if discreet {
            if !info.people.is_empty() {
                ui.label(egui::RichText::new("—").size(12.0).color(TEXT));
            }
            return;
        }
        if info.people.is_empty() {
            ui.label(egui::RichText::new(tr("None yet")).size(11.0).color(FAINT));
        }
        let mut remove: Option<i64> = None;
        let mut filter: Option<i64> = None;
        ui.horizontal_wrapped(|ui| {
            for p in &info.people {
                let b = ui
                    .add(egui::Button::new(
                        egui::RichText::new(&p.name).size(11.0).color(TEXT),
                    ))
                    .on_hover_text(tr("Show items with this performer"));
                if b.clicked() {
                    filter = Some(p.id);
                }
                if editable
                    && ui
                        .small_button("×")
                        .on_hover_text(tr("Remove performer"))
                        .clicked()
                {
                    remove = Some(p.id);
                }
            }
        });
        if let Some(p) = remove {
            self.push(
                "person.assign",
                json!({ "ids": [p], "media_ids": [id], "on": false }),
            );
        }
        if let Some(p) = filter {
            self.push(
                "view.apply",
                json!({ "spec": { "people": [p] }, "label": tr("Performer") }),
            );
        }
        if editable {
            let mut add = false;
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.info_ui.person)
                        .desired_width(ui.available_width() - 64.0)
                        .char_limit(60)
                        .hint_text(tr("Add performer")),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    add = true;
                }
                if ui.button(tr("Add")).clicked() {
                    add = true;
                }
            });
            // Suggestions from what exists, by name or alias.
            let q = self.info_ui.person.trim().to_owned();
            let mut pick: Option<String> = None;
            if !q.is_empty() {
                let have: Vec<i64> = info.people.iter().map(|p| p.id).collect();
                let sugg = self.db.people(Some(&q), 5).unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    for p in sugg.iter().filter(|p| !have.contains(&p.id)) {
                        if ui
                            .small_button(egui::RichText::new(&p.name).color(MUTED))
                            .clicked()
                        {
                            pick = Some(p.name.clone());
                        }
                    }
                });
            }
            if let Some(name) = pick {
                self.info_ui.person.clear();
                self.info_add_person(id, &name);
            } else if add {
                let t = std::mem::take(&mut self.info_ui.person);
                self.info_add_person(id, &t);
            }
        }
    }

    /// Aliases of one tag: a small window with the list, and a field to add.
    pub(super) fn tag_alias_window(&mut self, ctx: &egui::Context) {
        let Some(edit) = &self.alias_edit else {
            return;
        };
        let tag = edit.tag;
        if self.cfg.discreet || self.opts.disguised {
            self.alias_edit = None;
            return;
        }
        let aliases = self.db.tag_aliases(tag).unwrap_or_default();
        let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut remove: Option<String> = None;
        let mut add: Option<String> = None;
        let mut input = self
            .alias_edit
            .as_ref()
            .map(|e| e.input.clone())
            .unwrap_or_default();
        egui::Window::new(tr("Aliases"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if aliases.is_empty() {
                    ui.label(egui::RichText::new(tr("None yet")).size(11.0).color(FAINT));
                }
                for a in &aliases {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(a).size(12.0).color(TEXT));
                        if ui
                            .small_button("×")
                            .on_hover_text(tr("Remove alias"))
                            .clicked()
                        {
                            remove = Some(a.clone());
                        }
                    });
                }
                ui.add_space(S);
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut input)
                            .desired_width(180.0)
                            .char_limit(60)
                            .hint_text(tr("Add alias")),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        add = Some(input.clone());
                    }
                    if ui.button(tr("Add")).clicked() {
                        add = Some(input.clone());
                    }
                });
                ui.add_space(S);
                if crate::widgets::button_outline(ui, tr("Close")).clicked() {
                    close = true;
                }
            });
        if let Some(e) = &mut self.alias_edit {
            e.input = input;
        }
        if let Some(a) = remove {
            self.push("tag.alias.remove", json!({ "id": tag, "alias": a }));
        }
        if let Some(a) = add.filter(|a| !a.trim().is_empty()) {
            self.push("tag.alias.add", json!({ "id": tag, "alias": a }));
            if let Some(e) = &mut self.alias_edit {
                e.input.clear();
            }
        }
        if close {
            self.alias_edit = None;
        }
    }
}

/// Commands that change what the inspector, the grid captions or the search
/// show; the app refreshes after each.
pub(super) fn changes_metadata(name: &str) -> bool {
    matches!(
        name,
        "media.info.set"
            | "studio.create"
            | "studio.rename"
            | "studio.set"
            | "studio.delete"
            | "studio.merge"
            | "studio.assign"
            | "person.create"
            | "person.rename"
            | "person.set"
            | "person.delete"
            | "person.merge"
            | "person.alias.add"
            | "person.alias.remove"
            | "person.assign"
            | "tag.alias.add"
            | "tag.alias.remove"
    )
}
