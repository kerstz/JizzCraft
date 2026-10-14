//! Accessibility invariants of the Craft theme: WCAG contrast and zoom range.
use jizzcraft_ui::theme::{self, *};

#[test]
fn text_roles_meet_wcag_aa() {
    for (bg_name, bg) in [("BG", BG), ("RAISED", RAISED), ("SUNKEN", SUNKEN)] {
        for (name, fg) in [
            ("TEXT", TEXT),
            ("MUTED", theme::over(MUTED, bg)),
            ("FAINT", theme::over(FAINT, bg)),
            ("ACCENT_INK", ACCENT_INK),
            ("APP_INK", APP_INK),
            ("DANGER", DANGER),
        ] {
            let c = theme::contrast(fg, bg);
            assert!(c >= 4.5, "{name} on {bg_name}: {c:.2}:1");
        }
    }
    let c = theme::contrast(INVERT_FG, INVERT_BG);
    assert!(c >= 4.5, "inverted button: {c:.2}:1");
}

#[test]
fn ui_strokes_meet_3_to_1() {
    for (name, fg) in [("LINE_STRONG", LINE_STRONG), ("ACCENT", ACCENT)] {
        let c = theme::contrast(theme::over(fg, BG), BG);
        assert!(c >= 3.0, "{name} on BG: {c:.2}:1");
    }
    // The focus ring colour.
    assert!(theme::contrast(ACCENT_INK, BG) >= 3.0);
}

#[test]
fn zoom_is_clamped() {
    assert_eq!(theme::clamp_zoom(0.2), 0.75);
    assert_eq!(theme::clamp_zoom(9.0), 2.0);
    assert_eq!(theme::clamp_zoom(f32::NAN), 1.0);
}
