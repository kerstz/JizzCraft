//! Left panel: built-in and saved views, then the folder tree.

use super::{JizzApp, PlayState, View};
use crate::theme::{self, APP, BG, FAINT, LINE, MUTED, S, TEXT};
use crate::widgets;
use egui::{Align2, CornerRadius, FontFamily, FontId, Rect, Sense, Ui, pos2, vec2};
use jizzcraft_core::Filter;
use jizzcraft_core::view::{BUILTINS, DirNode};
use serde_json::json;
use std::path::PathBuf;

pub(super) struct SidebarCache {
    pub generation: u64,
    /// `(key, label, sensitive, count)`
    pub builtin: Vec<(&'static str, &'static str, bool, u64)>,
    pub saved: Vec<(String, u64)>,
    pub tree: Vec<DirNode>,
}

const ROW_H: f32 = 26.0;

impl JizzApp {
    fn sidebar_cache(&mut self) -> &SidebarCache {
        let stale = self
            .sidebar
            .as_ref()
            .is_none_or(|c| c.generation != self.generation);
        if stale {
            let count = |db: &jizzcraft_core::Db, f: &Filter| db.count(f).unwrap_or(0);
            let builtin = BUILTINS
                .iter()
                .map(|b| (b.key, b.label, b.sensitive, count(&self.db, &(b.spec)())))
                .collect();
            let saved = self
                .db
                .views()
                .unwrap_or_default()
                .into_iter()
                .map(|v| {
                    let n = Filter::from_json(&v.spec)
                        .map(|f| count(&self.db, &f))
                        .unwrap_or(0);
                    (v.name, n)
                })
                .collect();
            let roots: Vec<PathBuf> = self
                .db
                .folders()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            let counts = self.db.dir_counts().unwrap_or_default();
            let tree = jizzcraft_core::view::build_tree(&roots, &counts);
            for r in &roots {
                self.tree_expanded.insert(r.clone());
            }
            self.sidebar = Some(SidebarCache {
                generation: self.generation,
                builtin,
                saved,
                tree,
            });
        }
        self.sidebar.as_ref().expect("cache built above")
    }

    pub(super) fn sidebar(&mut self, ui: &mut Ui) {
        if self.cfg.sidebar_closed || self.stats.total == 0 {
            return;
        }
        let discreet = self.cfg.discreet;
        let hide_history = discreet || self.cfg.hide_history;
        let active_label = self.extra.as_ref().map(|_| self.view_label.clone());
        let active_dir = self.extra.as_ref().and_then(|f| f.dir.clone());
        // Snapshot so rows can push commands while drawing.
        let (builtin, saved, tree) = {
            let c = self.sidebar_cache();
            (c.builtin.clone(), c.saved.clone(), c.tree.clone())
        };
        let expanded = self.tree_expanded.clone();
        let mut toggled: Option<PathBuf> = None;
        let mut cmds: Vec<(&'static str, serde_json::Value)> = Vec::new();
        let mut delete: Option<String> = None;

        egui::Panel::left("sidebar")
            .default_size(224.0)
            .size_range(180.0..=360.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(0, 12))
                    .stroke(theme::hairline()),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        section(ui, "02", crate::i18n::tr("Views"));
                        let all_active = active_label.is_none();
                        if row(ui, 0, crate::i18n::tr("Library"), None, all_active).clicked() {
                            cmds.push(("view.filter", json!({ "tab": "library" })));
                        }
                        for (key, label, sensitive, n) in &builtin {
                            if *sensitive && hide_history {
                                continue;
                            }
                            let on = active_label.as_deref() == Some(*label);
                            if row(ui, 0, crate::i18n::tr(label), Some(*n), on).clicked() {
                                cmds.push(("view.apply", json!({ "name": key })));
                            }
                        }
                        for (i, (name, n)) in saved.iter().enumerate() {
                            let shown = if discreet {
                                crate::i18n::trf("View {}", &[&format!("{:02}", i + 1)])
                            } else {
                                name.clone()
                            };
                            let on = active_label.as_deref() == Some(name.as_str());
                            let r = row(ui, 0, &shown, Some(*n), on);
                            if r.clicked() {
                                cmds.push(("view.apply", json!({ "name": name })));
                            }
                            r.context_menu(|ui| {
                                if ui.button(crate::i18n::tr("Delete view")).clicked() {
                                    delete = Some(name.clone());
                                    ui.close();
                                }
                            });
                        }
                        ui.add_space(S * 2.0);
                        section(ui, "03", crate::i18n::tr("Folders"));
                        if discreet {
                            ui.add_space(S);
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(theme::hud(
                                    crate::i18n::tr("Hidden in discreet mode"),
                                    FAINT,
                                ));
                            });
                        } else {
                            for root in &tree {
                                tree_rows(
                                    ui,
                                    root,
                                    0,
                                    &expanded,
                                    active_dir.as_deref(),
                                    &mut toggled,
                                    &mut cmds,
                                );
                            }
                        }
                    });
            });
        if let Some(p) = toggled
            && !self.tree_expanded.remove(&p)
        {
            self.tree_expanded.insert(p);
        }
        for (name, args) in cmds {
            self.push(name, args);
        }
        if let Some(name) = delete {
            self.push("views.delete", json!({ "name": name }));
        }
    }

    /// Modal-ish dialog for naming the current filter as a saved view.
    pub(super) fn save_view_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.save_view.take() else {
            return;
        };
        let mut keep = true;
        let mut save = false;
        egui::Window::new(crate::i18n::tr("SAVE VIEW"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::tr(
                        "Name this filter to find it in the sidebar.",
                    ))
                    .color(MUTED),
                );
                ui.add_space(S);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .hint_text(crate::i18n::tr("View name"))
                        .desired_width(260.0),
                );
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                ui.add_space(S);
                ui.horizontal(|ui| {
                    if widgets::button_solid(ui, crate::i18n::tr("Save")).clicked() {
                        save = true;
                    }
                    if widgets::button_outline(ui, crate::i18n::tr("Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep = false;
        }
        if save && !name.trim().is_empty() {
            let spec = self.filter().to_json();
            self.push("views.save", json!({ "name": name.trim(), "spec": spec }));
            return;
        }
        if keep {
            self.save_view = Some(name);
        }
    }

    /// Slideshow driver: images advance on a timer, videos when they end.
    pub(super) fn tick_slideshow(&mut self, ctx: &egui::Context) {
        if !self.slideshow.active {
            return;
        }
        let ready = match &self.view {
            View::Image(iv) => iv.tex.is_some() || iv.error.is_some(),
            View::Player(pv) => pv.player.state() == PlayState::Ended,
            View::Library => false,
        };
        if !ready || self.slideshow.paused {
            self.slideshow.next_at = None;
            return;
        }
        let wait = match &self.view {
            View::Player(_) => std::time::Duration::from_secs(1),
            _ => std::time::Duration::from_secs(self.slideshow.interval.max(1)),
        };
        let at = *self
            .slideshow
            .next_at
            .get_or_insert_with(|| std::time::Instant::now() + wait);
        let left = at.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            self.slideshow.next_at = None;
            let cur = match &self.view {
                View::Image(iv) => Some(iv.index),
                View::Player(pv) => pv.index,
                View::Library => None,
            };
            if let Some(cur) = cur {
                let next = if cur + 1 < self.total { cur + 1 } else { 0 };
                self.open_index(next);
            }
        } else {
            ctx.request_repaint_after(left.min(std::time::Duration::from_millis(500)));
        }
    }
}

fn section(ui: &mut Ui, num: &str, name: &str) {
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        widgets::eyebrow(ui, num, name, "");
    });
    ui.add_space(6.0);
}

/// One sidebar row. Returns its click response.
fn row(ui: &mut Ui, depth: usize, label: &str, count: Option<u64>, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, LINE);
    }
    if active {
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
            CornerRadius::ZERO,
            APP,
        );
    }
    let color = if active { TEXT } else { MUTED };
    let x = rect.left() + 16.0 + depth as f32 * 12.0;
    p.with_clip_rect(Rect::from_min_max(
        rect.min,
        pos2(rect.right() - 44.0, rect.bottom()),
    ))
    .text(
        pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(12.0, FontFamily::Proportional),
        color,
    );
    if let Some(n) = count {
        p.text(
            pos2(rect.right() - 12.0, rect.center().y),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::new(10.0, FontFamily::Monospace),
            FAINT,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn tree_rows(
    ui: &mut Ui,
    node: &DirNode,
    depth: usize,
    expanded: &std::collections::HashSet<PathBuf>,
    active: Option<&std::path::Path>,
    toggled: &mut Option<PathBuf>,
    cmds: &mut Vec<(&'static str, serde_json::Value)>,
) {
    let open = expanded.contains(&node.path);
    let has_kids = !node.children.is_empty();
    let on = active == Some(node.path.as_path());
    let r = row(
        ui,
        depth + usize::from(has_kids),
        &node.name,
        Some(node.total),
        on,
    );
    // Chevron, drawn with lines so it needs no font glyph.
    if has_kids {
        let c = pos2(
            r.rect.left() + 12.0 + depth as f32 * 12.0,
            r.rect.center().y,
        );
        let stroke = egui::Stroke::new(1.2, FAINT);
        let (a, b, d) = if open {
            (
                c + vec2(-3.0, -1.5),
                c + vec2(0.0, 1.5),
                c + vec2(3.0, -1.5),
            )
        } else {
            (
                c + vec2(-1.5, -3.0),
                c + vec2(1.5, 0.0),
                c + vec2(-1.5, 3.0),
            )
        };
        ui.painter().line_segment([a, b], stroke);
        ui.painter().line_segment([b, d], stroke);
        let hit = Rect::from_center_size(c, vec2(18.0, ROW_H));
        let clicked_chevron = r.clicked()
            && ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| hit.contains(p));
        if clicked_chevron {
            *toggled = Some(node.path.clone());
        } else if r.clicked() {
            cmds.push((
                "view.apply",
                json!({ "spec": { "dir": node.path, "recursive": true }, "label": node.name }),
            ));
        }
    } else if r.clicked() {
        cmds.push((
            "view.apply",
            json!({ "spec": { "dir": node.path, "recursive": true }, "label": node.name }),
        ));
    }
    if open {
        for c in &node.children {
            tree_rows(ui, c, depth + 1, expanded, active, toggled, cmds);
        }
    }
}
