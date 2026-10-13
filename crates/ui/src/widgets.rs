//! Craft graphic language: hairlines, square corners, tracked mono labels,
//! inverted primary buttons, corner ticks.

use crate::theme::{
    self, ACCENT_INK, APP, APP_INK, FAINT, INVERT_BG, INVERT_FG, LINE, LINE_STRONG, MUTED, S, TEXT,
    TEXT_STRONG,
};
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, vec2,
};

/// `+` / corner registration ticks at the four corners of a rect (11 px).
pub fn corner_crosshairs(painter: &egui::Painter, rect: Rect, inset: f32) {
    let r = rect.shrink(inset);
    let arm = 5.5;
    let stroke = Stroke::new(1.0, LINE_STRONG);
    for c in [
        r.left_top(),
        r.right_top(),
        r.left_bottom(),
        r.right_bottom(),
    ] {
        painter.line_segment([c - vec2(arm, 0.0), c + vec2(arm, 0.0)], stroke);
        painter.line_segment([c - vec2(0.0, arm), c + vec2(0.0, arm)], stroke);
    }
}

fn paint_focus(ui: &Ui, r: &Response) {
    if r.has_focus() {
        ui.painter().rect_stroke(
            r.rect.expand(2.0),
            CornerRadius::ZERO,
            Stroke::new(2.0, ACCENT_INK),
            StrokeKind::Outside,
        );
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Solid,
    Outline,
    Chip { on: bool },
}

fn button_inner(ui: &mut Ui, label: &str, look: Look, height: f32, side_pad: f32) -> Response {
    let tracking = 1.32;
    let probe = ui
        .painter()
        .layout_job(theme::spaced(label, 11.0, tracking, TEXT));
    let size = vec2(probe.size().x + side_pad * 2.0, height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    let (fill, fg, stroke) = match (look, hovered) {
        (Look::Solid, false) => (INVERT_BG, INVERT_FG, Stroke::NONE),
        (Look::Solid, true) => (INVERT_BG.gamma_multiply(0.8), INVERT_FG, Stroke::NONE),
        (Look::Outline, false) => (Color32::TRANSPARENT, TEXT, Stroke::new(1.0, LINE_STRONG)),
        (Look::Outline, true) => (INVERT_BG, INVERT_FG, Stroke::NONE),
        (Look::Chip { on: true }, _) => (Color32::TRANSPARENT, TEXT, Stroke::new(1.0, LINE_STRONG)),
        (Look::Chip { on: false }, false) => (Color32::TRANSPARENT, MUTED, Stroke::new(1.0, LINE)),
        (Look::Chip { on: false }, true) => {
            (Color32::TRANSPARENT, TEXT, Stroke::new(1.0, LINE_STRONG))
        }
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    if stroke != Stroke::NONE {
        ui.painter()
            .rect_stroke(rect, CornerRadius::ZERO, stroke, StrokeKind::Inside);
    }
    let galley = ui
        .painter()
        .layout_job(theme::spaced(label, 11.0, tracking, fg));
    let pos = rect.center() - galley.size() / 2.0;
    ui.painter().galley(pos, galley, fg);
    paint_focus(ui, &resp);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Inverted primary button: white fill, near-black text.
pub fn button_solid(ui: &mut Ui, label: &str) -> Response {
    button_inner(ui, label, Look::Solid, 32.0, 14.0)
}

/// Outlined secondary button; inverts on hover.
pub fn button_outline(ui: &mut Ui, label: &str) -> Response {
    button_inner(ui, label, Look::Outline, 32.0, 14.0)
}

/// 48 px hero-size primary button.
pub fn button_solid_hero(ui: &mut Ui, label: &str) -> Response {
    button_inner(ui, label, Look::Solid, 48.0, 24.0)
}

/// Full-height top-bar primary button.
pub fn button_solid_bar(ui: &mut Ui, label: &str) -> Response {
    button_inner(ui, label, Look::Solid, 48.0, 20.0)
}

/// 48 px hero-size secondary button.
pub fn button_outline_hero(ui: &mut Ui, label: &str) -> Response {
    button_inner(ui, label, Look::Outline, 48.0, 24.0)
}

/// Top-bar nav item: no box, muted when idle, inverted chip on hover.
pub fn nav_tab(ui: &mut Ui, label: &str, active: bool) -> Response {
    let probe = ui.painter().layout_job(theme::hud(label, TEXT));
    let h = 48.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(probe.size().x + 20.0, h), Sense::click());
    let hovered = resp.hovered() && !active;
    let fg = if hovered {
        INVERT_FG
    } else if active {
        TEXT
    } else {
        MUTED
    };
    let galley = ui.painter().layout_job(theme::hud(label, fg));
    if hovered {
        let chip = Rect::from_center_size(rect.center(), galley.size() + vec2(12.0, 4.0));
        ui.painter()
            .rect_filled(chip, CornerRadius::ZERO, INVERT_BG);
    }
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    paint_focus(ui, &resp);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Small toggle chip (discreet mode, inspector, …).
pub fn toggle_chip(ui: &mut Ui, label: &str, on: bool) -> Response {
    button_inner(ui, label, Look::Chip { on }, 24.0, 10.0)
}

/// Rectangular app-colour badge with black bold mono text ("01").
pub fn badge(ui: &mut Ui, text: &str, side: f32) -> Response {
    let font = FontId::new(
        (side * 0.46).max(11.0),
        FontFamily::Name("mono-bold".into()),
    );
    let galley = ui
        .painter()
        .layout_job(egui::text::LayoutJob::single_section(
            text.to_owned(),
            egui::TextFormat {
                font_id: font,
                color: Color32::BLACK,
                extra_letter_spacing: 1.65,
                ..Default::default()
            },
        ));
    let size = galley.size() + vec2(16.0, 8.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, APP);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::BLACK);
    resp
}

/// "Jizz" (strong white) + "Craft" (app colour) in the display face.
pub fn wordmark_job(size: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = FontId::new(size, theme::title_family());
    let fmt = |color| egui::TextFormat {
        font_id: font.clone(),
        color,
        extra_letter_spacing: size * -0.03,
        ..Default::default()
    };
    job.append("Jizz", 0.0, fmt(TEXT_STRONG));
    job.append("Craft", 0.0, fmt(APP_INK));
    job
}

pub fn wordmark(ui: &mut Ui, size: f32) {
    ui.label(wordmark_job(size));
}

/// Tracked uppercase mono label (`hud`).
pub fn mono_label(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(theme::hud(text, color));
}

/// Key / value metadata row (inspector).
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.set_min_width(ui.available_width());
        ui.allocate_ui(vec2(84.0, 14.0), |ui| {
            ui.label(theme::spaced(key, 10.0, 1.2, FAINT));
        });
        ui.add(egui::Label::new(egui::RichText::new(value).size(12.0).color(TEXT)).wrap());
    });
}

pub fn hairline_h(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, LINE);
}

/// Section header: faint number, faint slash, muted name; faint note on the right.
pub fn eyebrow(ui: &mut Ui, num: &str, name: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(theme::hud(num, FAINT));
        ui.label(theme::hud("/", FAINT));
        ui.label(theme::hud(name, MUTED));
        if !right.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(theme::hud(right, FAINT));
            });
        }
    });
}

/// Line-style 5-point star (Lucide look), filled when `filled`.
pub fn star_icon(painter: &egui::Painter, center: Pos2, size: f32, filled: bool, color: Color32) {
    let outer = size / 2.0;
    let inner = outer * 0.42;
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let r = if i % 2 == 0 { outer } else { inner };
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            center + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    let fill = if filled { color } else { Color32::TRANSPARENT };
    painter.add(egui::Shape::convex_polygon(
        pts,
        fill,
        Stroke::new(1.5, color),
    ));
}

#[allow(dead_code)]
const _: f32 = S;
#[allow(dead_code)]
const _A: Align2 = Align2::CENTER_CENTER;
