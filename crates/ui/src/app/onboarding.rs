//! First-run wizard, shown once after the age gate and reopenable from the
//! menu. Every step is skippable. It shows no file names or paths, and it
//! never opens a network session: the only things it can do are the existing
//! local commands (import a folder, privacy toggles), a background `doctor`
//! look, and, in the online edition, the web-pages step which itself only
//! looks for programs by file name.
//!
//! The state machine ([`Step`], [`next`], [`back`], [`visible_steps`]) is pure
//! and tested without a window. The wizard's state lives in egui memory, so
//! the app needs no new field.

use super::JizzApp;
use crate::edition::{self, Edition};
use crate::i18n::{tr, trf};
use crate::keys;
use crate::theme::{self, BG, FAINT, MUTED, S};
use crate::widgets;
use egui::{Color32, FontId, Key, Rect, Ui, UiBuilder, vec2};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// The web-pages step, supplied by `web_pages::wizard_step` (network build).
#[allow(dead_code)] // used by the network build's step
pub(super) type WebStep = fn(&mut JizzApp, &mut Ui);

// ---- pure state machine -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Step {
    Welcome,
    Folders,
    Privacy,
    Health,
    /// Online edition, browser helper installed.
    WebPages,
    /// Online edition: proxy, VPN and the kill switch.
    Connection,
    Done,
}

/// What a step can cause. There is deliberately no network variant: the
/// wizard cannot start a session (tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // test-only audit of what a step may do
pub(super) enum Effect {
    /// Reading only.
    Nothing,
    /// An existing local command (import, privacy toggles).
    LocalCommand,
    /// A background `doctor` run.
    Doctor,
    /// Hands the area to the web-pages panel (it only looks by file name).
    DelegateWebPages,
}

impl Step {
    #[allow(dead_code)]
    pub(super) fn effects(self) -> &'static [Effect] {
        match self {
            Step::Welcome | Step::Connection | Step::Done => &[Effect::Nothing],
            Step::Folders | Step::Privacy => &[Effect::LocalCommand],
            Step::Health => &[Effect::Doctor],
            Step::WebPages => &[Effect::DelegateWebPages],
        }
    }
}

/// Steps shown, in order. Offline never shows the two online steps; the
/// web-pages step needs the browser helper.
pub(super) fn visible_steps(edition: Edition, browser_installed: bool) -> Vec<Step> {
    let mut v = vec![Step::Welcome, Step::Folders, Step::Privacy, Step::Health];
    if edition == Edition::Online {
        if browser_installed {
            v.push(Step::WebPages);
        }
        v.push(Step::Connection);
    }
    v.push(Step::Done);
    v
}

/// The step after `cur` (stays on the last one).
pub(super) fn next(steps: &[Step], cur: Step) -> Step {
    match steps.iter().position(|s| *s == cur) {
        Some(i) if i + 1 < steps.len() => steps[i + 1],
        _ => *steps.last().unwrap_or(&Step::Done),
    }
}

/// The step before `cur` (stays on the first one).
pub(super) fn back(steps: &[Step], cur: Step) -> Step {
    match steps.iter().position(|s| *s == cur) {
        Some(i) if i > 0 => steps[i - 1],
        _ => *steps.first().unwrap_or(&Step::Welcome),
    }
}

/// Skipping a step is moving on without acting.
pub(super) fn skip(steps: &[Step], cur: Step) -> Step {
    next(steps, cur)
}

/// One-based position and total, for the "Step 2 of 5" line.
pub(super) fn position(steps: &[Step], cur: Step) -> (usize, usize) {
    (
        steps.iter().position(|s| *s == cur).map_or(1, |i| i + 1),
        steps.len(),
    )
}

// ---- state in egui memory -------------------------------------------------------

#[derive(Default)]
struct Health {
    started: bool,
    /// `(label, ok)` rows and the fix command; set once by the job.
    rows: Vec<(String, bool)>,
    fix: Option<String>,
    done: bool,
}

#[derive(Clone)]
struct State {
    open: bool,
    step: Step,
    health: Arc<Mutex<Health>>,
}

fn id() -> egui::Id {
    egui::Id::new("jizzcraft-onboarding")
}

fn load(ctx: &egui::Context) -> Option<State> {
    ctx.data(|d| d.get_temp::<State>(id()))
}

fn store(ctx: &egui::Context, s: State) {
    ctx.data_mut(|d| d.insert_temp(id(), s));
}

fn fresh() -> State {
    State {
        open: true,
        step: Step::Welcome,
        health: Arc::new(Mutex::new(Health::default())),
    }
}

/// Menu entry "Welcome tour": reopens the wizard from the first step.
#[allow(dead_code)] // wired to the menu entry
pub(super) fn reopen(ctx: &egui::Context) {
    store(ctx, fresh());
    ctx.request_repaint();
}

// ---- show -------------------------------------------------------------------------

/// Draws the wizard when it is due and returns true if it took the window
/// (the caller then skips the normal UI for this frame). Call it after the
/// age gate. Due: first run (`!cfg.onboarding_done`) or reopened.
pub(super) fn show(app: &mut JizzApp, ui: &mut Ui) -> bool {
    let ctx = &ui.ctx().clone();
    let mut st = match load(ctx) {
        Some(s) => s,
        None if !app.cfg.onboarding_done => fresh(),
        None => return false,
    };
    if !st.open {
        return false;
    }
    let browser = !matches!(
        edition::browser_status(),
        edition::BrowserStatus::NotInstalled | edition::BrowserStatus::Off
    );
    let steps = visible_steps(edition::EDITION, browser);
    if !steps.contains(&st.step) {
        st.step = steps[0];
    }

    // Keyboard, only when no widget has the focus (text boxes keep their keys).
    let mut go_next = false;
    let mut go_back = false;
    let mut leave = false;
    if ctx.memory(|m| m.focused().is_none()) {
        ctx.input(|i| {
            go_next = i.key_pressed(Key::Enter) || i.key_pressed(Key::ArrowRight);
            go_back = i.key_pressed(Key::ArrowLeft);
        });
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        leave = true;
    }

    if st.step == Step::Health {
        start_health(ctx, &st.health);
    }

    let mut open_downloads = false;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let w = 680.0_f32.min(rect.width() - 96.0).max(240.0);
            let col = Rect::from_center_size(rect.center(), vec2(w, rect.height().min(520.0)));
            ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
                ui.spacing_mut().item_spacing = vec2(S * 2.0, S * 2.0);
                let (n, total) = position(&steps, st.step);
                widgets::mono_label(
                    ui,
                    &trf(
                        "Welcome tour, step {} of {}",
                        &[&n.to_string(), &total.to_string()],
                    ),
                    MUTED,
                );
                ui.label(
                    egui::RichText::new(title(st.step))
                        .font(FontId::new(34.0, theme::title_family()))
                        .color(Color32::WHITE),
                );
                body(app, ui, st.step, &st.health, &mut open_downloads);
                ui.add_space(S);
                ui.horizontal(|ui| {
                    if st.step != steps[0] && widgets::button_outline(ui, tr("Back")).clicked() {
                        go_back = true;
                    }
                    if st.step == Step::Done {
                        if widgets::button_solid(ui, tr("Finish")).clicked() {
                            leave = true;
                        }
                    } else {
                        if widgets::button_solid(ui, tr("Next")).clicked() {
                            go_next = true;
                        }
                        if widgets::button_outline(ui, tr("Skip")).clicked() {
                            st.step = skip(&steps, st.step);
                        }
                        if widgets::button_outline(ui, tr("Skip all")).clicked() {
                            leave = true;
                        }
                    }
                });
                ui.label(
                    egui::RichText::new(tr("Enter: next. Left: back. Esc: skip all."))
                        .size(12.0)
                        .color(FAINT),
                );
            });
        });

    if go_next {
        st.step = if st.step == Step::Done {
            st.step
        } else {
            next(&steps, st.step)
        };
    }
    if go_back {
        st.step = back(&steps, st.step);
    }
    if leave || open_downloads {
        st.open = false;
        if !app.cfg.onboarding_done {
            app.cfg.onboarding_done = true;
            app.save_cfg();
        }
    }
    store(ctx, st);
    if open_downloads {
        app.push("view.downloads", json!({}));
    }
    true
}

fn title(step: Step) -> &'static str {
    match step {
        Step::Welcome => tr("Welcome"),
        Step::Folders => tr("Your folders"),
        Step::Privacy => tr("Privacy"),
        Step::Health => tr("Can this machine play things?"),
        Step::WebPages => tr("Play videos from web pages"),
        Step::Connection => tr("Your connection"),
        Step::Done => tr("All set"),
    }
}

fn para(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).size(13.0).color(MUTED)).wrap());
}

fn body(
    app: &mut JizzApp,
    ui: &mut Ui,
    step: Step,
    health: &Arc<Mutex<Health>>,
    open_downloads: &mut bool,
) {
    match step {
        Step::Welcome => {
            let ed = edition::label();
            para(ui, &trf("Edition: {}", &[ed]));
            if edition::EDITION == Edition::Online {
                para(
                    ui,
                    tr(
                        "This edition can fetch and stream, but only when you ask. Nothing connects before you act.",
                    ),
                );
            } else {
                para(
                    ui,
                    tr("This edition manages local media only. It has no network code at all."),
                );
            }
            para(
                ui,
                tr("Everything stays on this machine. Your media files are never changed."),
            );
        }
        Step::Folders => {
            para(
                ui,
                tr("Add a folder to build your library. You can add more later."),
            );
            if widgets::button_outline(ui, tr("Add a folder")).clicked() {
                app.push("library.import", json!({}));
            }
        }
        Step::Privacy => {
            para(
                ui,
                tr("Pick what suits your screen and your room. All of it can be changed later."),
            );
            ui.horizontal_wrapped(|ui| {
                let c = &app.cfg;
                let (d, s, j, g) = (c.discreet, c.screen_share_safe, !c.no_jokes, c.disguise);
                if widgets::toggle_chip(ui, tr("Discreet mode"), d).clicked() {
                    app.push("app.discreet.toggle", json!({}));
                }
                if widgets::toggle_chip(ui, tr("Share-safe (blur until hovered)"), s).clicked() {
                    app.push("app.share_safe.toggle", json!({}));
                }
                if widgets::toggle_chip(ui, tr("Playful messages"), j).clicked() {
                    app.push("app.jokes.toggle", json!({}));
                }
                if widgets::toggle_chip(ui, tr("Disguise as \"Viewer\""), g).clicked() {
                    app.push("app.disguise.set", json!({ "on": !g }));
                }
            });
            if let Some(b) = boss_key_label() {
                para(ui, &trf("Boss key: {}", &[b]));
            }
        }
        Step::Health => health_ui(ui, health),
        Step::WebPages => {
            #[cfg(feature = "network")]
            {
                let f: WebStep = super::web_pages::wizard_step;
                f(app, ui);
            }
            #[cfg(not(feature = "network"))]
            para(ui, tr("Not available in this edition."));
        }
        Step::Connection => {
            para(
                ui,
                tr(
                    "If you use a proxy or a VPN, set it in Downloads settings. With \"require proxy\" on (the kill switch), nothing connects when the proxy is not there.",
                ),
            );
            if widgets::button_outline(ui, tr("Open Downloads settings")).clicked() {
                *open_downloads = true;
            }
        }
        Step::Done => {
            para(
                ui,
                tr("You are ready. You can reopen this tour from the menu."),
            );
        }
    }
}

/// The boss-key chord, read from the keymap table.
pub(super) fn boss_key_label() -> Option<&'static str> {
    keys::KEYMAP
        .iter()
        .find(|b| b.cmd == "app.boss_key")
        .map(|b| b.keys)
}

// ---- health ---------------------------------------------------------------------

fn start_health(ctx: &egui::Context, health: &Arc<Mutex<Health>>) {
    {
        let Ok(mut h) = health.lock() else { return };
        if h.started {
            return;
        }
        h.started = true;
    }
    let (h, ctx) = (Arc::clone(health), ctx.clone());
    std::thread::spawn(move || {
        let r = jizzcraft_media::doctor::run();
        let rows = r
            .checks
            .iter()
            .filter(|c| c.group != "hardware")
            .map(|c| (c.label.to_string(), c.ok))
            .collect();
        let fix = if r.exit_code() == 0 { None } else { r.fix() };
        if let Ok(mut g) = h.lock() {
            g.rows = rows;
            g.fix = fix;
            g.done = true;
        }
        ctx.request_repaint();
    });
}

fn health_ui(ui: &mut Ui, health: &Arc<Mutex<Health>>) {
    let (rows, fix, done) = match health.lock() {
        Ok(h) => (h.rows.clone(), h.fix.clone(), h.done),
        Err(_) => (Vec::new(), None, true),
    };
    if !done {
        para(ui, tr("Looking at the codecs..."));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
        return;
    }
    for (label, ok) in &rows {
        let mark = if *ok { tr("ok") } else { tr("missing") };
        para(ui, &format!("{mark}  {label}"));
    }
    match fix {
        Some(cmd) => {
            para(
                ui,
                tr(
                    "To fix what is missing, run this in a terminal (JizzCraft never runs it for you):",
                ),
            );
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&cmd).monospace().size(12.0));
                if widgets::button_outline(ui, tr("Copy")).clicked() {
                    ui.ctx().copy_text(cmd.clone());
                }
            });
        }
        None => para(ui, tr("All good.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_has_no_online_steps() {
        for browser in [false, true] {
            let v = visible_steps(Edition::Offline, browser);
            assert_eq!(
                v,
                [
                    Step::Welcome,
                    Step::Folders,
                    Step::Privacy,
                    Step::Health,
                    Step::Done
                ]
            );
        }
    }

    #[test]
    fn online_adds_web_pages_only_with_browser() {
        let with = visible_steps(Edition::Online, true);
        assert!(with.contains(&Step::WebPages) && with.contains(&Step::Connection));
        let without = visible_steps(Edition::Online, false);
        assert!(!without.contains(&Step::WebPages) && without.contains(&Step::Connection));
        assert_eq!(*with.last().unwrap(), Step::Done);
    }

    #[test]
    fn next_back_skip_walk_and_clamp() {
        let v = visible_steps(Edition::Online, true);
        let mut s = v[0];
        for want in &v[1..] {
            s = next(&v, s);
            assert_eq!(s, *want);
        }
        assert_eq!(next(&v, s), Step::Done);
        for want in v.iter().rev().skip(1) {
            s = back(&v, s);
            assert_eq!(s, *want);
        }
        assert_eq!(back(&v, s), Step::Welcome);
        assert_eq!(skip(&v, Step::Folders), Step::Privacy);
        // A step not in the list (edition changed) falls to the end, never panics.
        let off = visible_steps(Edition::Offline, false);
        assert_eq!(next(&off, Step::WebPages), Step::Done);
        assert_eq!(position(&off, Step::Health), (4, 5));
    }

    #[test]
    fn wizard_never_starts_a_network_session() {
        // No effect variant is a network one; the only step that reaches the
        // online code is the web-pages delegate, which looks by file name.
        for s in visible_steps(Edition::Online, true) {
            for e in s.effects() {
                assert!(matches!(
                    e,
                    Effect::Nothing
                        | Effect::LocalCommand
                        | Effect::Doctor
                        | Effect::DelegateWebPages
                ));
            }
        }
        assert_eq!(Step::Connection.effects(), [Effect::Nothing]);
        assert!(!visible_steps(Edition::Offline, true).contains(&Step::WebPages));
    }

    #[test]
    fn boss_key_comes_from_the_keymap() {
        assert_eq!(boss_key_label(), Some("Ctrl+Shift+H"));
    }
}
