//! Craft graphic language: technical grid, crosshairs, ruler, mono buttons.

use crate::theme::{self, ACCENT, BG, HAIRLINE, S, TEXT, TEXT_DIM};
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, vec2,
};

/// Faint technical grid behind a panel.
pub fn grid_background(painter: &egui::Painter, rect: Rect) {
    let step = 32.0;
    let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x2a, 0x2f, 0x35, 90));
    let mut x = (rect.left() / step).ceil() * step;
    while x < rect.right() {
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            stroke,
        );
        x += step;
    }
    let mut y = (rect.top() / step).ceil() * step;
    while y < rect.bottom() {
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            stroke,
        );
        y += step;
    }
}

/// `+` registration marks at the four corners of a rect.
pub fn corner_crosshairs(painter: &egui::Painter, rect: Rect, inset: f32) {
    let r = rect.shrink(inset);
    let arm = 6.0;
    let stroke = Stroke::new(1.0, TEXT_DIM);
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

/// Graduated vertical ruler (0, 5, 10…) on the right edge of `rect`.
pub fn ruler(painter: &egui::Painter, rect: Rect) {
    let x = rect.right() - 8.0;
    let step = 12.0;
    let top = rect.top() + 48.0;
    let bottom = rect.bottom() - 48.0;
    let mut y = top;
    let mut i = 0;
    while y <= bottom {
        let major = i % 5 == 0;
        let len = if major { 12.0 } else { 6.0 };
        painter.line_segment(
            [Pos2::new(x - len, y), Pos2::new(x, y)],
            Stroke::new(1.0, HAIRLINE.gamma_multiply(2.0)),
        );
        if major {
            painter.text(
                Pos2::new(x - 16.0, y),
                Align2::RIGHT_CENTER,
                format!("{i}"),
                FontId::new(9.0, FontFamily::Monospace),
                TEXT_DIM,
            );
        }
        y += step;
        i += 1;
    }
}

fn paint_focus(ui: &Ui, r: &Response) {
    if r.has_focus() {
        ui.painter().rect_stroke(
            r.rect.expand(2.0),
            CornerRadius::same(2),
            Stroke::new(2.0, ACCENT),
            StrokeKind::Outside,
        );
    }
}

fn button_inner(
    ui: &mut Ui,
    label: &str,
    fill: Color32,
    fg: Color32,
    stroke: Stroke,
    tracking: f32,
) -> Response {
    let galley = ui
        .painter()
        .layout_job(theme::spaced(label, 11.0, tracking, fg));
    let pad = vec2(S * 1.5, S);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::click());
    let hovered = resp.hovered();
    let fill = if hovered && fill != Color32::TRANSPARENT {
        fill.gamma_multiply(0.92)
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(1), fill);
    let stroke = if hovered {
        Stroke::new(1.0, TEXT)
    } else {
        stroke
    };
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(1), stroke, StrokeKind::Inside);
    ui.painter().galley(rect.min + pad, galley, fg);
    paint_focus(ui, &resp);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Solid white button with black mono text (primary action).
pub fn button_solid(ui: &mut Ui, label: &str) -> Response {
    button_inner(
        ui,
        label,
        Color32::WHITE,
        Color32::BLACK,
        Stroke::new(1.0, Color32::WHITE),
        1.5,
    )
}

/// Outlined button (secondary action).
pub fn button_outline(ui: &mut Ui, label: &str) -> Response {
    button_inner(
        ui,
        label,
        Color32::TRANSPARENT,
        TEXT,
        Stroke::new(1.0, TEXT),
        1.5,
    )
}

/// Nav tab. The active tab is a white outlined box, as on the site.
pub fn nav_tab(ui: &mut Ui, label: &str, active: bool) -> Response {
    let color = if active { Color32::WHITE } else { TEXT_DIM };
    let galley = ui
        .painter()
        .layout_job(theme::spaced(label, 11.0, 1.5, color));
    let pad = vec2(S * 1.25, S * 0.75);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::click());
    if active {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(1),
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
    let color = if resp.hovered() && !active {
        TEXT
    } else {
        color
    };
    ui.painter().galley(rect.min + pad, galley, color);
    paint_focus(ui, &resp);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Small toggle chip (discreet mode, inspector, …).
pub fn toggle_chip(ui: &mut Ui, label: &str, on: bool) -> Response {
    let (fg, stroke) = if on {
        (ACCENT, Stroke::new(1.0, ACCENT))
    } else {
        (TEXT_DIM, theme::hairline())
    };
    button_inner(ui, label, Color32::TRANSPARENT, fg, stroke, 1.0)
}

/// Square blue badge with a mono number, e.g. the "01" before the wordmark.
pub fn badge(ui: &mut Ui, text: &str, side: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(1), ACCENT);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::new(side * 0.45, FontFamily::Name("mono-bold".into())),
        BG,
    );
    resp
}

/// "Jizz" (white) + "Craft" (accent) in the title face.
pub fn wordmark_job(size: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = FontId::new(size, theme::title_family());
    job.append(
        "Jizz",
        0.0,
        egui::TextFormat::simple(font.clone(), Color32::WHITE),
    );
    job.append("Craft", 0.0, egui::TextFormat::simple(font, ACCENT));
    job
}

pub fn wordmark(ui: &mut Ui, size: f32) {
    ui.label(wordmark_job(size));
}

/// Mono dim label, uppercase and tracked.
pub fn mono_label(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(theme::spaced(text, 10.0, 1.2, color));
}

/// Key / value metadata row (inspector).
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.set_min_width(ui.available_width());
        ui.allocate_ui(vec2(84.0, 14.0), |ui| {
            ui.label(theme::spaced(key, 10.0, 1.2, TEXT_DIM));
        });
        ui.add(egui::Label::new(egui::RichText::new(value).size(11.0).color(TEXT)).wrap());
    });
}

pub fn hairline_h(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, HAIRLINE);
}
