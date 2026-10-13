//! Craft visual language: flat surfaces, 0 px radius, hairlines drawn as
//! translucent warm white, Archivo display, Inter body, mono labels.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextFormat, text::LayoutJob};

const fn warm(a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        ((0xf2 * a as u16) / 255) as u8,
        ((0xf1 * a as u16) / 255) as u8,
        ((0xee * a as u16) / 255) as u8,
        a,
    )
}

pub const BG: Color32 = Color32::from_rgb(0x12, 0x13, 0x16);
pub const RAISED: Color32 = Color32::from_rgb(0x10, 0x10, 0x14);
pub const SUNKEN: Color32 = Color32::from_rgb(0x06, 0x06, 0x07);
pub const TEXT: Color32 = Color32::from_rgb(0xf2, 0xf1, 0xee);
pub const TEXT_STRONG: Color32 = Color32::WHITE;
/// 60 % warm white.
pub const MUTED: Color32 = warm(0x99);
/// 38 % warm white.
pub const FAINT: Color32 = warm(0x61);
/// 15 % warm white.
pub const LINE: Color32 = warm(0x26);
/// 40 % warm white.
pub const LINE_STRONG: Color32 = warm(0x66);
pub const ACCENT: Color32 = Color32::from_rgb(0x2d, 0x81, 0xff);
pub const ACCENT_INK: Color32 = Color32::from_rgb(0x74, 0xaa, 0xff);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x62, 0x59);
pub const INVERT_BG: Color32 = Color32::WHITE;
pub const INVERT_FG: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0c);
/// JizzCraft's own app colour (from the peach icon), as every Craft app has one.
pub const APP: Color32 = Color32::from_rgb(0xff, 0x8a, 0x6b);
pub const APP_INK: Color32 = Color32::from_rgb(0xff, 0xb3, 0x9e);

/// Consistent 8 px rhythm.
pub const S: f32 = 8.0;

pub fn hairline() -> Stroke {
    Stroke::new(1.0, LINE)
}

/// Archivo Medium, used for the wordmark, headings and hero headline.
pub fn title_family() -> FontFamily {
    FontFamily::Name("title".into())
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let add = |fonts: &mut egui::FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
    };
    add(
        &mut fonts,
        "jbmono",
        include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"),
    );
    add(
        &mut fonts,
        "jbmono-bold",
        include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf"),
    );
    add(
        &mut fonts,
        "archivo",
        include_bytes!("../../../assets/fonts/Archivo-Medium.ttf"),
    );
    add(
        &mut fonts,
        "inter",
        include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
    );
    add(
        &mut fonts,
        "inter-medium",
        include_bytes!("../../../assets/fonts/Inter-Medium.ttf"),
    );

    // Body text is Inter; labels, buttons and numbers use mono. egui's bundled
    // fonts stay behind as fallback for Unicode filenames and symbols.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "inter".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "jbmono".to_owned());
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    for (name, head) in [
        (title_family(), "archivo"),
        (FontFamily::Name("ui-medium".into()), "inter-medium"),
        (FontFamily::Name("mono-bold".into()), "jbmono-bold"),
    ] {
        let mut list = vec![head.to_owned()];
        list.extend(fallback.iter().filter(|f| *f != "inter").cloned());
        fonts.families.insert(name, list);
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = BG;
        v.window_fill = RAISED;
        v.extreme_bg_color = SUNKEN;
        v.faint_bg_color = RAISED;
        v.code_bg_color = RAISED;
        v.hyperlink_color = ACCENT_INK;
        v.window_stroke = Stroke::new(1.0, LINE_STRONG);
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);

        for (w, fill) in [
            (&mut v.widgets.noninteractive, BG),
            (&mut v.widgets.inactive, RAISED),
            (&mut v.widgets.hovered, RAISED),
            (&mut v.widgets.active, RAISED),
            (&mut v.widgets.open, RAISED),
        ] {
            w.corner_radius = CornerRadius::ZERO;
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = hairline();
            w.fg_stroke = Stroke::new(1.0, TEXT);
            w.expansion = 0.0;
        }
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, LINE_STRONG);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_INK);
        // Keyboard focus ring.
        v.selection.stroke = Stroke::new(2.0, ACCENT_INK);

        style.spacing.item_spacing = egui::vec2(S, S * 0.75);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.window_margin = egui::Margin::same(16);
        style.spacing.menu_margin = egui::Margin::same(6);
        style.interaction.selectable_labels = false;

        use egui::TextStyle::*;
        let mono = |s: f32| FontId::new(s, FontFamily::Monospace);
        let sans = |s: f32| FontId::new(s, FontFamily::Proportional);
        style.text_styles.insert(Small, sans(10.5));
        style.text_styles.insert(Body, sans(12.0));
        style.text_styles.insert(Button, mono(11.0));
        style.text_styles.insert(Monospace, mono(12.0));
        style
            .text_styles
            .insert(Heading, FontId::new(20.0, title_family()));
    });
}

/// 11 px tracked uppercase mono, the Craft `.hud-label`.
pub fn hud(text: &str, color: Color32) -> LayoutJob {
    spaced(text, 11.0, 1.65, color)
}

/// Display text with CSS-style em tracking (negative tightens), e.g. -0.035.
pub fn display_job(text: &str, size: f32, tracking_em: f32, color: Color32) -> LayoutJob {
    LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: FontId::new(size, title_family()),
            color,
            extra_letter_spacing: size * tracking_em,
            ..Default::default()
        },
    )
}

/// Letter-spaced uppercase mono text. egui has no CSS letter-spacing, so this
/// builds a `LayoutJob` whose single section carries `extra_letter_spacing`.
pub fn spaced(text: &str, size: f32, tracking: f32, color: Color32) -> LayoutJob {
    spaced_in(text, FontFamily::Monospace, size, tracking, color)
}

pub fn spaced_in(
    text: &str,
    family: FontFamily,
    size: f32,
    tracking: f32,
    color: Color32,
) -> LayoutJob {
    LayoutJob::single_section(
        text.to_uppercase(),
        TextFormat {
            font_id: FontId::new(size, family),
            color,
            extra_letter_spacing: tracking,
            ..Default::default()
        },
    )
}
