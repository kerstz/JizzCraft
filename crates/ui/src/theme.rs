//! Craft "Pro" theme: hairline borders, 0–2 px radius, no shadows, mono labels.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextFormat, text::LayoutJob};

pub const BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
pub const PANEL: Color32 = Color32::from_rgb(0x15, 0x18, 0x1b);
pub const RAISED: Color32 = Color32::from_rgb(0x1e, 0x23, 0x27);
pub const HAIRLINE: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x35);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe8);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x8f, 0x98);
pub const ACCENT: Color32 = Color32::from_rgb(0x7a, 0xa2, 0xf7);
pub const DANGER: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);

/// Consistent 8 px rhythm.
pub const S: f32 = 8.0;

pub fn hairline() -> Stroke {
    Stroke::new(1.0, HAIRLINE)
}

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
        "grotesk-bold",
        include_bytes!("../../../assets/fonts/SpaceGrotesk-Bold.ttf"),
    );

    // Mono first everywhere (labels, tabs, buttons, values); egui's bundled
    // fonts stay behind it as fallback for Unicode filenames and symbols.
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(fam)
            .or_default()
            .insert(0, "jbmono".to_owned());
    }
    let mut title = vec!["grotesk-bold".to_owned()];
    title.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts.families.insert(title_family(), title);
    let mut bold = vec!["jbmono-bold".to_owned()];
    bold.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name("mono-bold".into()), bold);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.extreme_bg_color = BG;
        v.faint_bg_color = PANEL;
        v.code_bg_color = RAISED;
        v.hyperlink_color = ACCENT;
        v.window_stroke = hairline();
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.window_corner_radius = CornerRadius::same(2);
        v.menu_corner_radius = CornerRadius::same(2);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);

        let r = CornerRadius::same(2);
        for (w, fill) in [
            (&mut v.widgets.noninteractive, PANEL),
            (&mut v.widgets.inactive, RAISED),
            (&mut v.widgets.hovered, RAISED),
            (&mut v.widgets.active, RAISED),
            (&mut v.widgets.open, RAISED),
        ] {
            w.corner_radius = r;
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = hairline();
            w.fg_stroke = Stroke::new(1.0, TEXT);
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_stroke = hairline();
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, TEXT_DIM);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);

        style.spacing.item_spacing = egui::vec2(S, S);
        style.spacing.button_padding = egui::vec2(S * 1.5, S * 0.75);
        style.spacing.window_margin = egui::Margin::same(S as i8);
        style.interaction.selectable_labels = false;
        // Keyboard focus ring in accent.
        style.visuals.selection.stroke = Stroke::new(2.0, ACCENT);

        use egui::TextStyle::*;
        let mono = |s: f32| FontId::new(s, FontFamily::Monospace);
        style.text_styles.insert(Small, mono(10.0));
        style.text_styles.insert(Body, mono(12.0));
        style.text_styles.insert(Button, mono(12.0));
        style.text_styles.insert(Monospace, mono(12.0));
        style
            .text_styles
            .insert(Heading, FontId::new(20.0, title_family()));
    });
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
