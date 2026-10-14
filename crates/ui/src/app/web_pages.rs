//! "Play videos from web pages" (feature `network`).
//!
//! JizzCraft includes no sites. This panel only finds a program the user
//! already installed (by file name, asking `--version`), registers it as a
//! resolver when asked, and lets the user try it on one address. Nothing here
//! runs the tool except `--version` while looking and the explicit Test.
//!
//! The logic is behind the small [`WebHost`] trait so the command layer is
//! tested without a window; `JizzApp` implements it at the bottom.
#![cfg(feature = "network")]

use super::JizzApp;
use crate::theme::{FAINT, MUTED};
use crate::widgets;
use egui::{RichText, TextEdit};
use jizzcraft_core::Registry;
use jizzcraft_fetch::commands::FetchHost;
use jizzcraft_fetch::config::FetchConfig;
use jizzcraft_fetch::detect_tools::Found;
use jizzcraft_fetch::errors;
use jizzcraft_fetch::resolver::ResolverDef;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

// ---- the swap points for `jizzcraft_fetch::resolver_setup` ---------------
// Everything that touches the detection helpers goes through these four
// functions, so moving to `resolver_setup::*` later is a change here only.

fn find_tools(extra: &[PathBuf]) -> Vec<Found> {
    jizzcraft_fetch::detect_tools::find_resolver_tools(extra)
}

fn suggest(f: &Found) -> ResolverDef {
    jizzcraft_fetch::detect_tools::suggest(f)
}

fn already_added(cfg: &FetchConfig, path: &Path) -> bool {
    jizzcraft_fetch::detect_tools::already_added(cfg, path)
}

fn age_sentence(days: Option<u32>) -> Option<&'static str> {
    jizzcraft_fetch::detect_tools::age_sentence(days)
}

// ---- state ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Phase {
    #[default]
    Never,
    Looking,
    Done,
}

type TestOutcome = Result<usize, &'static str>;

#[derive(Default)]
pub(super) struct WebPages {
    /// The panel is open inside the Downloads window.
    pub(super) shown: bool,
    phase: Phase,
    found: Vec<Found>,
    job: Option<Receiver<Vec<Found>>>,
    /// The address being typed. Cleared when a test starts; never stored.
    test_input: String,
    test_job: Option<(u32, Receiver<TestOutcome>)>,
    test_result: Option<(u32, TestOutcome)>,
    install_cmd: Option<String>,
}

/// What the "look for a tool" block shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Detection {
    Never,
    Looking,
    /// Indices into the found list of tools not yet added.
    Found(Vec<usize>),
    /// Tools exist but every one is already added.
    NothingNew,
    None,
}

pub(super) fn detection(phase: Phase, found: &[Found], cfg: &FetchConfig) -> Detection {
    match phase {
        Phase::Never => Detection::Never,
        Phase::Looking => Detection::Looking,
        Phase::Done if found.is_empty() => Detection::None,
        Phase::Done => {
            let fresh: Vec<usize> = found
                .iter()
                .enumerate()
                .filter(|(_, f)| !already_added(cfg, &f.path))
                .map(|(i, _)| i)
                .collect();
            if fresh.is_empty() {
                Detection::NothingNew
            } else {
                Detection::Found(fresh)
            }
        }
    }
}

impl WebPages {
    /// Take the finished job's result, if any. Cheap; call every frame.
    pub(super) fn poll(&mut self) {
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(list) => {
                    self.found = list;
                    self.phase = Phase::Done;
                    self.job = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.phase = Phase::Done;
                    self.job = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((id, rx)) = &self.test_job {
            let id = *id;
            match rx.try_recv() {
                Ok(r) => {
                    self.test_result = Some((id, r));
                    self.test_job = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.test_result = Some((id, Err(errors::GENERIC)));
                    self.test_job = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }
}

// ---- pure text -------------------------------------------------------------

/// Name shown for a resolver or a found tool. Discreet mode: `Resolver N`.
pub(super) fn display_name(discreet: bool, n: u32, name: &str) -> String {
    if discreet || name.trim().is_empty() {
        format!("Resolver {n}")
    } else {
        name.to_owned()
    }
}

/// File name of a tool, reduced to harmless characters (never a directory).
pub(super) fn tool_name(path: &Path) -> String {
    let n: String = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
        .take(40)
        .collect();
    if n.is_empty() { "Resolver".into() } else { n }
}

/// Command that installs the usual tool on this distribution family.
pub(super) fn install_command(f: jizzcraft_core::distro::Family) -> Option<String> {
    use jizzcraft_core::distro::Family::*;
    Some(
        match f {
            Arch => "sudo pacman -S --needed yt-dlp",
            Debian => "sudo apt install yt-dlp",
            Fedora => "sudo dnf install yt-dlp",
            Suse => "sudo zypper install yt-dlp",
            Unknown => return None,
        }
        .to_owned(),
    )
}

/// One fixed sentence and a count; never a title, host or path.
pub(super) fn test_sentence(r: &TestOutcome) -> String {
    match r {
        Ok(0) => crate::i18n::tr(errors::NO_VIDEO_HERE).to_owned(),
        Ok(n) => crate::i18n::trf(
            "The tool understood this page: {} formats.",
            &[&n.to_string()],
        ),
        Err(e) => crate::i18n::tr(e).to_owned(),
    }
}

/// Lines describing a found tool: title, version line, optional age advice.
pub(super) fn found_lines(
    f: &Found,
    index: usize,
    discreet: bool,
) -> (String, String, Option<&'static str>) {
    (
        display_name(discreet, index as u32 + 1, &tool_name(&f.path)),
        crate::i18n::trf("Version {}", &[&f.version]),
        age_sentence(f.age_days),
    )
}

pub(super) struct Row {
    pub id: u32,
    pub label: String,
    pub enabled: bool,
}

pub(super) fn resolver_rows(list: &[ResolverDef], discreet: bool) -> Vec<Row> {
    list.iter()
        .map(|r| Row {
            id: r.id,
            label: display_name(discreet, r.id, &r.name),
            enabled: r.enabled,
        })
        .collect()
}

// ---- commands --------------------------------------------------------------

/// What the commands and the panel need from the application.
pub(super) trait WebHost {
    fn fetch_host(&mut self) -> &mut FetchHost;
    fn web(&mut self) -> &mut WebPages;
    /// Ask for a repaint from a worker thread.
    fn wake(&self) -> Box<dyn Fn() + Send + 'static>;
}

type Res = jizzcraft_core::Result<Value>;

fn invalid(msg: &str) -> jizzcraft_core::Error {
    jizzcraft_core::Error::Invalid(msg.into())
}

/// Extra folders to search, set by tests only (the real search uses PATH).
#[cfg(test)]
static EXTRA_DIRS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

fn extra_dirs() -> Vec<PathBuf> {
    #[cfg(test)]
    {
        EXTRA_DIRS.lock().map(|g| g.clone()).unwrap_or_default()
    }
    #[cfg(not(test))]
    {
        Vec::new()
    }
}

fn detect<C: WebHost>(app: &mut C) -> Res {
    if app.web().phase == Phase::Looking {
        return Ok(json!({ "started": false }));
    }
    let wake = app.wake();
    let extra = extra_dirs();
    let (tx, rx) = channel();
    let w = app.web();
    w.phase = Phase::Looking;
    w.job = Some(rx);
    w.found.clear();
    w.install_cmd = install_command(jizzcraft_core::distro::detect());
    std::thread::spawn(move || {
        let _ = tx.send(find_tools(&extra));
        wake();
    });
    Ok(json!({ "started": true }))
}

fn add_found<C: WebHost>(app: &mut C, a: Value) -> Res {
    let index = a
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing `index`"))? as usize;
    let f = app
        .web()
        .found
        .get(index)
        .cloned()
        .ok_or_else(|| invalid("no such tool"))?;
    if already_added(app.fetch_host().config(), &f.path) {
        return Ok(json!({ "already": true }));
    }
    let mut def = suggest(&f);
    def.name = tool_name(&f.path);
    let id = app.fetch_host().add_resolver(def)?;
    Ok(json!({ "id": id }))
}

fn enable<C: WebHost>(app: &mut C, a: Value) -> Res {
    let id = a
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing `id`"))? as u32;
    let on = a.get("on").and_then(Value::as_bool).unwrap_or(true);
    if !app
        .fetch_host()
        .config()
        .resolvers
        .iter()
        .any(|r| r.id == id)
    {
        return Err(invalid("no such resolver"));
    }
    app.fetch_host().update_settings(|c| {
        if let Some(r) = c.resolvers.iter_mut().find(|r| r.id == id) {
            r.enabled = on;
        }
    })?;
    Ok(json!({}))
}

/// The only place the tool runs besides `--version`: the user typed an
/// address and pressed Test. Honours the proxy kill switch like streaming.
fn test<C: WebHost>(app: &mut C, a: Value) -> Res {
    let id = a
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing `id`"))? as u32;
    let url = a
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .ok_or_else(|| invalid("missing `url`"))?
        .to_owned();
    let (def, blocked) = {
        let cfg = app.fetch_host().config();
        let def = cfg
            .resolvers
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| invalid("no such resolver"))?;
        let blocked = cfg.require_proxy && (cfg.proxy.is_none() || !def.proxy_aware());
        (def, blocked)
    };
    let (tx, rx) = channel();
    if blocked {
        let _ = tx.send(Err(errors::PROXY_REQUIRED));
    } else {
        // Starts the service, which publishes the proxy the tool is given.
        app.fetch_host().service()?;
        let wake = app.wake();
        std::thread::spawn(move || {
            let r = match jizzcraft_fetch::resolver::run(&def, &url) {
                Ok(formats) => Ok(formats.len()),
                Err(raw) => Err(errors::classify(&raw)),
            };
            let _ = tx.send(r);
            wake();
        });
    }
    let w = app.web();
    w.test_result = None;
    w.test_input.clear();
    w.test_job = Some((id, rx));
    Ok(json!({ "started": true }))
}

/// Registers `resolvers.detect`, `resolvers.add_found`, `resolvers.enable`
/// and `resolvers.test`. (`resolvers.remove` and `stream.resolve` exist.)
pub(super) fn register<C: WebHost + 'static>(reg: &mut Registry<C>) {
    reg.register(
        "resolvers.detect",
        "Look for a tool you installed that turns web pages into video addresses",
        |app, _| detect(app),
    );
    reg.register(
        "resolvers.add_found",
        "Add a tool found by resolvers.detect: {index}",
        add_found,
    );
    reg.register(
        "resolvers.enable",
        "Turn a resolver on or off: {id, on}",
        enable,
    );
    reg.register(
        "resolvers.test",
        "Try a resolver on one address: {id, url}",
        test,
    );
}

// ---- UI --------------------------------------------------------------------

/// The panel in the Downloads settings.
pub(super) fn panel(app: &mut JizzApp, ui: &mut egui::Ui) {
    render(app, ui, false);
}

/// The same, compact, as a step of the first-run wizard.
pub(super) fn wizard_step(app: &mut JizzApp, ui: &mut egui::Ui) {
    render(app, ui, true);
}

fn render(app: &mut JizzApp, ui: &mut egui::Ui, compact: bool) {
    app.web_pages.poll();
    let discreet = app.discreet() || app.opts.disguised;
    let cfg = app.downloads.host.config().clone();
    let rows = resolver_rows(&cfg.resolvers, discreet);
    let det = detection(app.web_pages.phase, &app.web_pages.found, &cfg);
    let mut cmds: Vec<(&'static str, Value)> = Vec::new();

    if !compact {
        widgets::eyebrow(ui, "", crate::i18n::tr("Play videos from web pages"), "");
    }
    ui.label(
        RichText::new(crate::i18n::tr(
            "JizzCraft includes no sites. If you installed a tool that turns a web page into a video address, it can play pages that tool supports.",
        ))
        .color(FAINT),
    );

    // Added resolvers: switch, remove, test.
    for r in &rows {
        ui.horizontal(|ui| {
            if widgets::toggle_chip(ui, &r.label, r.enabled).clicked() {
                cmds.push(("resolvers.enable", json!({ "id": r.id, "on": !r.enabled })));
            }
            if ui.small_button(crate::i18n::tr("Remove")).clicked() {
                cmds.push(("resolvers.remove", json!({ "id": r.id })));
            }
        });
        if compact {
            continue;
        }
        let w = &mut app.web_pages;
        let busy = w.test_job.as_ref().is_some_and(|(i, _)| *i == r.id);
        if busy {
            ui.label(RichText::new(crate::i18n::tr("Testing…")).color(MUTED));
        } else if let Some((i, res)) = &w.test_result
            && *i == r.id
        {
            ui.label(RichText::new(test_sentence(res)).color(MUTED));
        }
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut w.test_input)
                    .hint_text(crate::i18n::tr("Test with an address"))
                    .desired_width(260.0),
            );
            let ok = !busy && !w.test_input.trim().is_empty();
            if ui
                .add_enabled(ok, egui::Button::new(crate::i18n::tr("Test")))
                .clicked()
            {
                cmds.push((
                    "resolvers.test",
                    json!({ "id": r.id, "url": w.test_input.trim() }),
                ));
            }
        });
    }

    // Looking for a tool.
    match &det {
        Detection::Never => {}
        Detection::Looking => {
            ui.label(RichText::new(crate::i18n::tr("Looking…")).color(MUTED));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        Detection::Found(idx) => {
            for &i in idx {
                let (title, version, age) = found_lines(&app.web_pages.found[i], i, discreet);
                ui.horizontal(|ui| {
                    ui.label(title);
                    ui.label(RichText::new(version).color(MUTED));
                    if widgets::button_outline(ui, crate::i18n::tr("Add")).clicked() {
                        cmds.push(("resolvers.add_found", json!({ "index": i })));
                    }
                });
                if let Some(a) = age {
                    ui.label(RichText::new(crate::i18n::tr(a)).color(FAINT));
                }
            }
        }
        Detection::NothingNew => {}
        Detection::None => {
            ui.label(crate::i18n::tr(
                "No tool found. Install one with your package manager, then look again.",
            ));
            if let Some(c) = app.web_pages.install_cmd.clone() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&c).monospace().color(MUTED));
                    if ui.small_button(crate::i18n::tr("Copy")).clicked() {
                        ui.ctx().copy_text(c);
                    }
                });
            }
        }
    }
    let looking = matches!(det, Detection::Looking);
    let label = if matches!(det, Detection::Never) {
        crate::i18n::tr("Look for a tool")
    } else {
        crate::i18n::tr("Look again")
    };
    if ui.add_enabled(!looking, egui::Button::new(label)).clicked() {
        cmds.push(("resolvers.detect", json!({})));
    }

    for (n, a) in cmds {
        app.push(n, a);
    }
}

impl WebHost for JizzApp {
    fn fetch_host(&mut self) -> &mut FetchHost {
        &mut self.downloads.host
    }
    fn web(&mut self) -> &mut WebPages {
        &mut self.web_pages
    }
    fn wake(&self) -> Box<dyn Fn() + Send + 'static> {
        let ctx = self.ctx.clone();
        Box::new(move || ctx.request_repaint())
    }
}

#[cfg(test)]
mod tests {
    #![cfg_attr(not(unix), allow(dead_code, unused_imports))]
    use super::*;

    fn found(path: &str, v: &str, age: Option<u32>) -> Found {
        Found {
            path: PathBuf::from(path),
            version: v.into(),
            age_days: age,
        }
    }

    fn cfg_with(paths: &[&str]) -> FetchConfig {
        let mut c = FetchConfig::default();
        for (i, p) in paths.iter().enumerate() {
            c.resolvers.push(ResolverDef {
                id: i as u32 + 1,
                name: "x".into(),
                argv: vec![(*p).into(), "{url}".into()],
                timeout_secs: 30,
                enabled: true,
                honours_proxy: false,
            });
        }
        c
    }

    #[test]
    fn detection_transitions() {
        let none = FetchConfig::default();
        let f = vec![found("/nonexistent-dir/yt-dlp", "2026.01.01", None)];
        assert_eq!(detection(Phase::Never, &[], &none), Detection::Never);
        assert_eq!(detection(Phase::Looking, &[], &none), Detection::Looking);
        assert_eq!(detection(Phase::Done, &[], &none), Detection::None);
        assert_eq!(detection(Phase::Done, &f, &none), Detection::Found(vec![0]));
        let added = cfg_with(&["/nonexistent-dir/yt-dlp"]);
        assert_eq!(detection(Phase::Done, &f, &added), Detection::NothingNew);
    }

    #[test]
    fn poll_moves_looking_to_done() {
        let (tx, rx) = channel();
        let mut w = WebPages {
            phase: Phase::Looking,
            job: Some(rx),
            ..Default::default()
        };
        w.poll();
        assert_eq!(w.phase, Phase::Looking);
        tx.send(vec![found("/a/yt-dlp", "1", None)]).unwrap();
        w.poll();
        assert_eq!(w.phase, Phase::Done);
        assert_eq!(w.found.len(), 1);
        assert!(w.job.is_none());
        // A job that died without answering ends in "none found", not a hang.
        let (tx, rx) = channel::<Vec<Found>>();
        drop(tx);
        w.job = Some(rx);
        w.phase = Phase::Looking;
        w.found.clear();
        w.poll();
        assert_eq!(w.phase, Phase::Done);
    }

    #[test]
    fn discreet_names_are_neutral() {
        assert_eq!(display_name(true, 3, "yt-dlp"), "Resolver 3");
        assert_eq!(display_name(false, 3, "yt-dlp"), "yt-dlp");
        let rows = resolver_rows(&cfg_with(&["/a/yt-dlp", "/b/other"]).resolvers, true);
        assert_eq!(rows[0].label, "Resolver 1");
        assert_eq!(rows[1].label, "Resolver 2");
        let f = found("/home/someone/bin/yt-dlp", "2026.01.01", None);
        assert_eq!(found_lines(&f, 0, true).0, "Resolver 1");
        assert_eq!(found_lines(&f, 0, false).0, "yt-dlp");
    }

    #[test]
    fn produced_strings_leak_no_path_or_host() {
        let f = found("/home/someone/secret-dir/yt-dlp", "2026.01.01", Some(400));
        for discreet in [true, false] {
            let (t, v, a) = found_lines(&f, 0, discreet);
            for s in [t, v, a.unwrap_or("").to_owned()] {
                assert!(!s.contains('/') && !s.contains("someone"), "{s}");
            }
        }
        assert_eq!(tool_name(Path::new("/x/y/../we ird$.sh")), "weird.sh");
        for r in [Ok(0), Ok(4), Err(errors::RESOLVER_FAILED)] {
            let s = test_sentence(&r);
            assert!(!s.contains("http") && !s.contains('/'), "{s}");
        }
        assert!(test_sentence(&Ok(4)).contains('4'));
        // A raw error carrying an address becomes a fixed sentence.
        let raw = "resolver failed for https://host.invalid/x /home/u/f";
        assert!(errors::ALL.contains(&errors::classify(raw)));
    }

    #[test]
    fn install_commands_by_family() {
        use jizzcraft_core::distro::Family;
        assert!(install_command(Family::Arch).unwrap().contains("pacman"));
        assert!(install_command(Family::Debian).unwrap().contains("apt"));
        assert_eq!(install_command(Family::Unknown), None);
    }

    // ---- the command layer, with a stub tool -------------------------------

    struct Ctx {
        host: FetchHost,
        web: WebPages,
    }

    impl WebHost for Ctx {
        fn fetch_host(&mut self) -> &mut FetchHost {
            &mut self.host
        }
        fn web(&mut self) -> &mut WebPages {
            &mut self.web
        }
        fn wake(&self) -> Box<dyn Fn() + Send + 'static> {
            Box::new(|| {})
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_stub_tool_is_found_and_added_once() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let tool = bin.path().join("yt-dlp");
        std::fs::write(&tool, "#!/bin/sh\necho 2020.02.03\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(bin.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        *EXTRA_DIRS.lock().unwrap() = vec![bin.path().to_path_buf()];

        let state = tempfile::tempdir().unwrap();
        let mut ctx = Ctx {
            host: FetchHost::new(state.path().join("fetch.json")),
            web: WebPages::default(),
        };
        let mut reg: Registry<Ctx> = Registry::new();
        register(&mut reg);
        for c in [
            "resolvers.detect",
            "resolvers.add_found",
            "resolvers.enable",
        ] {
            assert!(reg.has(c));
        }

        let r = reg.run(&mut ctx, "resolvers.detect", json!({})).unwrap();
        assert_eq!(r["started"], true);
        assert_eq!(ctx.web.phase, Phase::Looking);
        // A second request while looking does nothing.
        let r = reg.run(&mut ctx, "resolvers.detect", json!({})).unwrap();
        assert_eq!(r["started"], false);
        for _ in 0..500 {
            ctx.web.poll();
            if ctx.web.phase == Phase::Done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ctx.web.phase, Phase::Done);
        let idx = ctx
            .web
            .found
            .iter()
            .position(|f| f.version == "2020.02.03")
            .expect("stub found");
        assert!(ctx.web.found[idx].age_days.is_some());

        let r = reg
            .run(&mut ctx, "resolvers.add_found", json!({ "index": idx }))
            .unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!(ctx.host.config().resolvers.len(), 1);
        assert!(ctx.host.config().resolvers[0].enabled);
        // Idempotent.
        let r = reg
            .run(&mut ctx, "resolvers.add_found", json!({ "index": idx }))
            .unwrap();
        assert_eq!(r["already"], true);
        assert_eq!(ctx.host.config().resolvers.len(), 1);
        assert_eq!(
            detection(Phase::Done, &ctx.web.found[idx..=idx], ctx.host.config()),
            Detection::NothingNew
        );

        reg.run(
            &mut ctx,
            "resolvers.enable",
            json!({ "id": id, "on": false }),
        )
        .unwrap();
        assert!(!ctx.host.config().resolvers[0].enabled);
        assert!(
            reg.run(&mut ctx, "resolvers.enable", json!({ "id": 99 }))
                .is_err()
        );
        assert!(
            reg.run(&mut ctx, "resolvers.add_found", json!({ "index": 999 }))
                .is_err()
        );
        EXTRA_DIRS.lock().unwrap().clear();
    }
}
