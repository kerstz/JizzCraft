//! The Settings window: General, Web pages, Browser, Connection.
//!
//! The online pages (Web pages, Browser, Connection) exist only in the online
//! edition; the offline build shows General and nothing that could connect.
//! Browser preferences live in `fetch.json` (`FetchConfig::browser`) and are
//! changed through the `browser.prefs.set` command.

use super::JizzApp;
use crate::edition::{self, Edition};
use crate::i18n::{tr, trf};
use crate::theme::{FAINT, MUTED};
use crate::widgets;
use egui::RichText;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Tab {
    #[default]
    General,
    WebPages,
    Browser,
    Connection,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::WebPages => "Web pages",
            Tab::Browser => "Browser",
            Tab::Connection => "Connection",
        }
    }

    pub(super) fn parse(s: &str) -> Option<Tab> {
        match s {
            "general" => Some(Tab::General),
            "web_pages" | "web" => Some(Tab::WebPages),
            "browser" => Some(Tab::Browser),
            "connection" => Some(Tab::Connection),
            _ => None,
        }
    }
}

/// Pages shown for an edition, in order. Offline: General only.
pub(super) fn tabs(edition: Edition) -> Vec<Tab> {
    match edition {
        Edition::Offline => vec![Tab::General],
        Edition::Online => vec![Tab::General, Tab::WebPages, Tab::Browser, Tab::Connection],
    }
}

#[derive(Default)]
struct Doctor {
    started: bool,
    done: bool,
    ok: usize,
    total: usize,
    fix: Option<String>,
}

#[derive(Default)]
pub(super) struct Settings {
    pub(super) open: bool,
    tab: Tab,
    doctor: Arc<Mutex<Doctor>>,
    #[cfg(feature = "network")]
    form: Form,
}

#[cfg(feature = "network")]
#[derive(Default)]
struct Form {
    /// Typed values; synced from the saved ones when the window opens.
    rules: String,
    search: String,
    synced: bool,
    confirm_clear: bool,
    pick: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
}

impl Settings {
    pub(super) fn show_tab(&mut self, t: Tab) {
        self.open = true;
        self.tab = t;
        #[cfg(feature = "network")]
        {
            self.form.synced = false;
            self.form.confirm_clear = false;
        }
    }
}

fn start_doctor(ctx: &egui::Context, d: &Arc<Mutex<Doctor>>) {
    {
        let Ok(mut g) = d.lock() else { return };
        if g.started {
            return;
        }
        g.started = true;
        g.done = false;
    }
    let (d, ctx) = (Arc::clone(d), ctx.clone());
    std::thread::spawn(move || {
        let r = jizzcraft_media::doctor::run();
        let rows: Vec<bool> = r
            .checks
            .iter()
            .filter(|c| c.group != "hardware")
            .map(|c| c.ok)
            .collect();
        let fix = if r.exit_code() == 0 { None } else { r.fix() };
        if let Ok(mut g) = d.lock() {
            g.ok = rows.iter().filter(|o| **o).count();
            g.total = rows.len();
            g.fix = fix;
            g.done = true;
        }
        ctx.request_repaint();
    });
}

impl JizzApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings.open {
            return;
        }
        let mut st = std::mem::take(&mut self.settings);
        let mut open = true;
        let mut cmds: Vec<(&'static str, Value)> = Vec::new();
        egui::Window::new(tr("SETTINGS"))
            .open(&mut open)
            .id(egui::Id::new("settings-window"))
            .default_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for t in tabs(edition::EDITION) {
                        if widgets::toggle_chip(ui, tr(t.label()), st.tab == t).clicked() {
                            st.tab = t;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(460.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match st.tab {
                        Tab::General => self.settings_general(ui, &mut st, &mut cmds),
                        #[cfg(feature = "network")]
                        Tab::WebPages => super::web_pages::panel(self, ui),
                        #[cfg(feature = "network")]
                        Tab::Browser => self.settings_browser(ui, &mut st, &mut cmds),
                        #[cfg(feature = "network")]
                        Tab::Connection => self.settings_connection(ui, &mut cmds),
                        #[cfg(not(feature = "network"))]
                        _ => {}
                    });
            });
        st.open = st.open && open;
        self.settings = st;
        for (n, a) in cmds {
            self.push(n, a);
        }
    }

    fn settings_general(
        &mut self,
        ui: &mut egui::Ui,
        st: &mut Settings,
        cmds: &mut Vec<(&'static str, Value)>,
    ) {
        widgets::kv(
            ui,
            tr("Edition"),
            tr(match edition::EDITION {
                Edition::Offline => "offline",
                Edition::Online => "online",
            }),
        );
        widgets::kv(ui, tr("Version"), env!("CARGO_PKG_VERSION"));
        ui.add_space(6.0);
        widgets::eyebrow(ui, "", tr("This machine"), "");
        let (started, done, ok, total, fix) = match st.doctor.lock() {
            Ok(d) => (d.started, d.done, d.ok, d.total, d.fix.clone()),
            Err(_) => (true, true, 0, 0, None),
        };
        if started && !done {
            ui.label(RichText::new(tr("Looking at the codecs...")).color(MUTED));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        } else if done {
            ui.label(trf(
                "{} of {} checks pass.",
                &[&ok.to_string(), &total.to_string()],
            ));
            if let Some(f) = fix {
                ui.label(
                    RichText::new(tr(
                        "To fix what is missing, run this in a terminal (JizzCraft never runs it for you):",
                    ))
                    .color(FAINT),
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&f).monospace().size(12.0));
                    if widgets::button_outline(ui, tr("Copy")).clicked() {
                        ui.ctx().copy_text(f.clone());
                    }
                });
            }
        }
        if !(started && !done) && widgets::button_outline(ui, tr("Check this machine")).clicked() {
            if let Ok(mut d) = st.doctor.lock() {
                d.started = false;
            }
            start_doctor(ui.ctx(), &st.doctor);
        }
        ui.add_space(6.0);
        if widgets::button_outline(ui, tr("Welcome tour")).clicked() {
            super::onboarding::reopen(ui.ctx());
        }
        ui.add_space(6.0);
        widgets::eyebrow(ui, "", tr("Appearance"), "");
        let mut z = ui.ctx().zoom_factor();
        ui.horizontal(|ui| {
            ui.label(tr("Interface zoom"));
            if ui
                .add(
                    egui::Slider::new(&mut z, crate::theme::ZOOM_MIN..=crate::theme::ZOOM_MAX)
                        .step_by(0.05)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                )
                .changed()
            {
                ui.ctx().set_zoom_factor(crate::theme::clamp_zoom(z));
            }
            if widgets::button_outline(ui, tr("Reset")).clicked() {
                ui.ctx().set_zoom_factor(1.0);
            }
        });
        ui.label(RichText::new(tr("Ctrl+= / Ctrl+- / Ctrl+0 also change the zoom.")).color(FAINT));
        let mut rm = self.cfg.reduce_motion;
        if ui.checkbox(&mut rm, tr("Reduce motion")).changed() {
            self.cfg.reduce_motion = rm;
            crate::theme::set_reduce_motion(ui.ctx(), rm);
            self.save_cfg();
        }
        let _ = cmds;
        if edition::EDITION == Edition::Offline {
            ui.add_space(6.0);
            ui.label(
                RichText::new(tr(
                    "This edition manages local media only. It has no network code at all.",
                ))
                .color(FAINT),
            );
        }
    }
}

#[cfg(feature = "network")]
impl JizzApp {
    fn settings_browser(
        &mut self,
        ui: &mut egui::Ui,
        st: &mut Settings,
        cmds: &mut Vec<(&'static str, Value)>,
    ) {
        use edition::BrowserStatus;
        let prefs = self.downloads.host.config().browser.clone();
        let f = &mut st.form;
        if !f.synced {
            f.rules = prefs.rules_path.clone().unwrap_or_default();
            f.search = prefs.search_template.clone().unwrap_or_default();
            f.synced = true;
        }
        if let Some(rx) = &f.pick
            && let Ok(p) = rx.try_recv()
        {
            f.pick = None;
            if let Some(p) = p {
                cmds.push((
                    "browser.prefs.set",
                    json!({ "rules_path": p.to_string_lossy() }),
                ));
                f.synced = false;
            }
        }
        let status = edition::browser_status();
        widgets::kv(
            ui,
            tr("Engine"),
            tr(match &status {
                BrowserStatus::Installed(_) => "installed",
                BrowserStatus::NotInstalled => "not installed",
                BrowserStatus::Off => "off",
            }),
        );
        if matches!(status, BrowserStatus::NotInstalled) {
            ui.label(
                RichText::new(tr(
                    "Install the jizzcraft-browser package to use the browser.",
                ))
                .color(FAINT),
            );
        }
        let toggles = [
            ("enabled", "Browser on", prefs.enabled),
            ("remember", "Remember sign-ins", prefs.remember),
            ("js", "JavaScript", prefs.js),
            ("media", "Video inside pages", prefs.media),
            ("software", "Software rendering", prefs.software),
        ];
        ui.horizontal_wrapped(|ui| {
            for (key, label, on) in toggles {
                if widgets::toggle_chip(ui, tr(label), on).clicked() {
                    cmds.push(("browser.prefs.set", json!({ key: !on })));
                }
            }
        });
        ui.label(
            RichText::new(tr(
                "Sign-ins are kept in a private folder on this machine only when Remember is on.",
            ))
            .color(FAINT),
        );
        ui.add_space(6.0);
        widgets::eyebrow(ui, "", tr("Content-block list"), "");
        ui.label(
            RichText::new(tr(
                "Your own rules file (WebKit content-rule JSON). None is included.",
            ))
            .color(FAINT),
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new(if prefs.rules_path.is_some() {
                tr("A file is set.")
            } else {
                tr("No file.")
            }));
            if widgets::button_outline(ui, tr("Choose a file…")).clicked() && f.pick.is_none() {
                let (tx, rx) = std::sync::mpsc::channel();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let p = rfd::FileDialog::new()
                        .add_filter("JSON", &["json"])
                        .pick_file();
                    let _ = tx.send(p);
                    ctx.request_repaint();
                });
                f.pick = Some(rx);
            }
            if prefs.rules_path.is_some() && ui.small_button(tr("Remove")).clicked() {
                cmds.push(("browser.prefs.set", json!({ "rules_path": "" })));
                f.synced = false;
            }
        });
        ui.add_space(6.0);
        widgets::eyebrow(ui, "", tr("Search address"), "");
        ui.label(
            RichText::new(tr(
                "An address with one {} where the words go. Empty: the browser has no search.",
            ))
            .color(FAINT),
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut f.search)
                    .hint_text("https://…/?q={}")
                    .desired_width(300.0),
            );
            if ui.button(tr("Save")).clicked() {
                cmds.push((
                    "browser.prefs.set",
                    json!({ "search_template": f.search.trim() }),
                ));
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can = matches!(status, BrowserStatus::Installed(_)) && prefs.enabled;
            if ui
                .add_enabled(can, egui::Button::new(tr("Open the browser")))
                .clicked()
            {
                cmds.push(("browser.open", json!({})));
            }
            if self.browser.running() && ui.button(tr("Close the browser")).clicked() {
                cmds.push(("browser.close", json!({})));
            }
        });
        ui.add_space(8.0);
        widgets::eyebrow(ui, "", tr("Browser data"), "");
        if f.confirm_clear {
            ui.label(tr("Remove the browser's saved sign-ins and site data?"));
            ui.horizontal(|ui| {
                if widgets::button_solid(ui, tr("Remove")).clicked() {
                    cmds.push(("browser.data.clear", json!({})));
                    f.confirm_clear = false;
                }
                if widgets::button_outline(ui, tr("Cancel")).clicked() {
                    f.confirm_clear = false;
                }
            });
        } else if widgets::button_outline(ui, tr("Clear browser data")).clicked() {
            f.confirm_clear = true;
        }
    }

    fn settings_connection(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<(&'static str, Value)>) {
        use super::status_chip as chip;
        self.proxy_test.poll();
        let cfg = self.downloads.host.config();
        let state = jizzcraft_fetch::service::connection_state(cfg);
        let discreet = self.discreet() || self.opts.disguised;
        let (require, proxied) = (
            cfg.require_proxy,
            cfg.proxy.as_ref().is_some_and(|p| !p.is_empty()),
        );
        widgets::kv(ui, tr("Status"), &chip::label(&state, discreet));
        ui.label(RichText::new(tr(chip::explain(&state))).color(FAINT));
        ui.add_space(6.0);
        if widgets::toggle_chip(ui, tr("Block the network without a proxy"), require).clicked() {
            cmds.push(("fetch.settings", json!({ "require_proxy": !require })));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let can = proxied && !self.proxy_test.busy();
            if ui
                .add_enabled(can, egui::Button::new(tr("Test the proxy")))
                .clicked()
            {
                cmds.push(("net.proxy.test", json!({})));
            }
            if let Some(p) = self.proxy_test.last {
                ui.label(RichText::new(tr(chip::probe_sentence(p))).color(MUTED));
            } else if self.proxy_test.busy() {
                ui.label(RichText::new(tr("Testing…")).color(MUTED));
            }
        });
        ui.label(
            RichText::new(tr(
                "The test only checks that the proxy answers; nothing is sent through it.",
            ))
            .color(FAINT),
        );
        ui.add_space(6.0);
        if widgets::button_outline(ui, tr("Open Downloads settings")).clicked() {
            cmds.push(("view.downloads", json!({})));
        }
    }
}

pub(super) fn register(reg: &mut jizzcraft_core::Registry<JizzApp>) {
    reg.register(
        "app.settings",
        "Show or hide Settings: {tab?: general|web_pages|browser|connection}",
        |app, a| {
            let tab = a
                .get("tab")
                .and_then(Value::as_str)
                .and_then(Tab::parse)
                .filter(|t| tabs(edition::EDITION).contains(t));
            match tab {
                Some(t) => app.settings.show_tab(t),
                None if app.settings.open => app.settings.open = false,
                None => app.settings.show_tab(Tab::General),
            }
            Ok(json!({ "open": app.settings.open }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_shows_general_only() {
        assert_eq!(tabs(Edition::Offline), [Tab::General]);
        let on = tabs(Edition::Online);
        assert_eq!(on[0], Tab::General);
        assert!(on.contains(&Tab::Browser) && on.contains(&Tab::Connection));
    }

    #[test]
    fn tab_names_parse() {
        assert_eq!(Tab::parse("connection"), Some(Tab::Connection));
        assert_eq!(Tab::parse("nope"), None);
    }
}
