//! Launcher for the optional browser helper (online edition only).
//!
//! The helper is a separate program (`jizzcraft-browser`). It is started only
//! when the user asks (a click or `Ctrl+Shift+B`), with an argument list, a
//! cleared environment and its own process group. The proxy goes to it on
//! stdin (`Hello`), never on a command line. It is killed, as a group, on the
//! boss key, the panic switch, share-safe, a proxy or kill-switch change and
//! on quit. Offline edition: this module does not exist.
//!
//! The command construction and the message handling are pure and sit behind
//! the small [`Spawn`] trait, so they are tested without a window or a helper.
#![cfg(feature = "network")]

use super::JizzApp;
use super::ToastKind;
use jizzcraft_bridge::{
    FromHelper, ReadError, ToHelper, check_play_url, read_message, write_message,
};
use jizzcraft_fetch::config::BrowserPrefs;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Protocol version the helper checks in `Hello`.
pub(super) const HELLO_VERSION: u32 = 1;

/// The only variables the helper inherits.
pub(super) const ENV_ALLOWED: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XAUTHORITY",
    "DBUS_SESSION_BUS_ADDRESS",
    "HOME",
    // Where "remember" keeps its folder; the helper reads it too.
    "XDG_DATA_HOME",
];

// ---- pure: what to start --------------------------------------------------

/// Everything the launcher knows when deciding to start.
pub(super) struct Inputs<'a> {
    pub exe: Option<PathBuf>,
    pub prefs: &'a BrowserPrefs,
    pub proxy: Option<String>,
    pub require_proxy: bool,
    pub discreet: bool,
    pub disguised: bool,
    pub share_safe: bool,
    pub age_ok: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    AgeGate,
    Disabled,
    NotInstalled,
    ProxyRequired,
    ShareSafe,
}

impl Refusal {
    pub(super) fn sentence(self) -> &'static str {
        match self {
            Refusal::AgeGate => "Confirm the 18+ gate first.",
            Refusal::Disabled => "The browser is turned off in Settings.",
            Refusal::NotInstalled => "The browser is not installed.",
            Refusal::ProxyRequired => "The browser needs a proxy: set one in Downloads settings.",
            Refusal::ShareSafe => "Turn Share-safe off to open the browser.",
        }
    }
}

/// What gets started: program, arguments (never a shell, never the proxy),
/// the only environment it will see, and the first line it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Launch {
    pub exe: PathBuf,
    pub argv: Vec<String>,
    pub env: Vec<(String, OsString)>,
    pub hello: ToHelper,
}

pub(super) fn plan(
    i: &Inputs,
    env: impl Iterator<Item = (OsString, OsString)>,
) -> Result<Launch, Refusal> {
    if !i.age_ok {
        return Err(Refusal::AgeGate);
    }
    if !i.prefs.enabled {
        return Err(Refusal::Disabled);
    }
    let exe = i.exe.clone().ok_or(Refusal::NotInstalled)?;
    let proxy = i.proxy.clone().filter(|p| !p.is_empty());
    if proxy.is_none() && i.require_proxy {
        return Err(Refusal::ProxyRequired);
    }
    if i.share_safe {
        return Err(Refusal::ShareSafe);
    }
    let nonempty = |o: &Option<String>| o.clone().filter(|s| !s.trim().is_empty());
    let env = env
        .filter_map(|(k, v)| {
            let k = k.into_string().ok()?;
            ENV_ALLOWED.contains(&k.as_str()).then_some((k, v))
        })
        .collect();
    Ok(Launch {
        exe,
        argv: Vec::new(),
        env,
        hello: ToHelper::Hello {
            v: HELLO_VERSION,
            proxy,
            require_proxy: i.require_proxy,
            js: i.prefs.js,
            media: i.prefs.media,
            remember: i.prefs.remember,
            discreet: i.discreet,
            disguised: i.disguised,
            rules_path: nonempty(&i.prefs.rules_path),
            search_template: nonempty(&i.prefs.search_template),
            software: i.prefs.software,
        },
    })
}

/// The one sentence for a helper error code. Nothing the helper says is shown.
pub(super) fn error_sentence(code: &str) -> &'static str {
    match code {
        "engine_missing" => "The browser was installed without its engine.",
        "proxy_required" => "The browser needs a proxy: set one in Downloads settings.",
        "no_display" => "The browser could not open a window.",
        "profile_unwritable" => "The browser cannot write its profile folder.",
        "bad_hello" | "bad_version" | "bad_proxy" | "bad_rules_path" | "bad_search_template" => {
            "The browser could not use your settings."
        }
        _ => "The browser stopped.",
    }
}

/// What the app does for one helper message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Reaction {
    Nothing,
    /// A registered command and its arguments.
    Command(&'static str, Value),
    /// One fixed sentence for the user.
    Say(&'static str),
    /// The helper is going away.
    Ended,
}

pub(super) fn react(m: FromHelper) -> Reaction {
    match m {
        FromHelper::Ready { .. } => Reaction::Nothing,
        FromHelper::Play { url, .. } => match check_play_url(&url) {
            Ok(u) => Reaction::Command("stream.open", json!({ "url": u })),
            Err(_) => Reaction::Say("That address cannot be played."),
        },
        FromHelper::Boss => Reaction::Command("app.boss_key", json!({})),
        FromHelper::Closed => Reaction::Ended,
        FromHelper::Error { code } => Reaction::Say(error_sentence(&code)),
    }
}

/// Where "remember" keeps cookies, from the environment as the helper sees it.
pub(super) fn data_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(x) => x,
        None => PathBuf::from(home?).join(".local/share"),
    };
    base.is_absolute()
        .then(|| base.join("jizzcraft").join("browser"))
}

// ---- preferences and saved data ---------------------------------------------

/// Apply a `browser.prefs.set` argument object. Absent keys are untouched.
/// An empty `rules_path` or `search_template` clears it.
pub(super) fn apply_prefs(p: &mut BrowserPrefs, a: &Value) -> Result<(), &'static str> {
    let flag = |k: &str| a.get(k).and_then(Value::as_bool);
    let text = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_owned())
    };
    let rules = text("rules_path");
    if let Some(r) = &rules
        && !r.is_empty()
        && !Path::new(r).is_absolute()
    {
        return Err("The rules file must be a full path.");
    }
    let tpl = text("search_template");
    if let Some(t) = &tpl
        && !t.is_empty()
    {
        let ok = t.matches("{}").count() == 1
            && (t.starts_with("https://") || t.starts_with("http://"))
            && !t.chars().any(|c| c.is_control() || c.is_whitespace());
        if !ok {
            return Err("The search address needs http(s):// and exactly one {}.");
        }
    }
    if let Some(v) = flag("enabled") {
        p.enabled = v;
    }
    if let Some(v) = flag("remember") {
        p.remember = v;
    }
    if let Some(v) = flag("js") {
        p.js = v;
    }
    if let Some(v) = flag("media") {
        p.media = v;
    }
    if let Some(v) = flag("software") {
        p.software = v;
    }
    if let Some(r) = rules {
        p.rules_path = (!r.is_empty()).then_some(r);
    }
    if let Some(t) = tpl {
        p.search_template = (!t.is_empty()).then_some(t);
    }
    Ok(())
}

/// Remove the helper's saved folder and nothing else. Refuses any path that
/// is not exactly `<something>/jizzcraft/browser`, or that is a link.
pub(super) fn clear_data(dir: &Path) -> io::Result<bool> {
    let ok_name = dir.is_absolute()
        && dir.file_name().is_some_and(|n| n == "browser")
        && dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "jizzcraft");
    if !ok_name {
        return Err(io::Error::other("not the browser folder"));
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => {
            std::fs::remove_dir_all(dir)?;
            Ok(true)
        }
        Ok(_) => Err(io::Error::other("not a folder")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

// ---- the seam: starting a process ------------------------------------------

/// A started helper: where to write, where to read, and how to end it.
pub(super) struct Started {
    pub to: Box<dyn Write + Send>,
    pub from: Box<dyn BufRead + Send>,
    /// Kills the whole group and reaps. Called once.
    pub kill: Box<dyn FnOnce() + Send>,
}

pub(super) trait Spawn {
    fn start(&self, l: &Launch) -> io::Result<Started>;
}

/// A helper file the user may start: a regular file, executable, not
/// writable by others.
fn file_ok(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| {
            let mode = m.permissions().mode();
            m.is_file() && mode & 0o111 != 0 && mode & 0o002 == 0
        })
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

pub(super) struct RealSpawn;

impl Spawn for RealSpawn {
    fn start(&self, l: &Launch) -> io::Result<Started> {
        use std::process::{Command, Stdio};
        if !file_ok(&l.exe) {
            return Err(io::Error::other("helper not usable"));
        }
        let mut c = Command::new(&l.exe);
        c.args(&l.argv)
            .env_clear()
            .envs(l.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        let mut child = c.spawn()?;
        let to = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let from = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        #[cfg(unix)]
        let pid = child.id();
        Ok(Started {
            to: Box::new(to),
            from: Box::new(io::BufReader::new(from)),
            kill: Box::new(move || {
                #[cfg(unix)]
                if let Ok(p) = i32::try_from(pid) {
                    // SAFETY: plain signal to the group this process leads.
                    unsafe {
                        libc::killpg(p, libc::SIGKILL);
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
            }),
        })
    }
}

// ---- running session -------------------------------------------------------

/// A message from a session's reader thread, tagged with the session id.
pub(super) enum Event {
    Msg(u64, FromHelper),
    End(u64),
}

/// Read helper lines until the stream ends. Oversized and malformed lines are
/// dropped (the stream stays in step); only end of stream or an I/O error end
/// the session.
pub(super) fn pump(mut r: impl BufRead, id: u64, tx: &Sender<Event>, wake: &dyn Fn()) {
    loop {
        match read_message::<_, FromHelper>(&mut r) {
            Ok(m) => {
                if tx.send(Event::Msg(id, m)).is_err() {
                    return;
                }
                wake();
            }
            Err(ReadError::TooLong | ReadError::Malformed) => {}
            Err(ReadError::Eof | ReadError::Io(_)) => {
                let _ = tx.send(Event::End(id));
                wake();
                return;
            }
        }
    }
}

struct Active {
    id: u64,
    to: Box<dyn Write + Send>,
    kill: Option<Box<dyn FnOnce() + Send>>,
    /// Proxy and kill switch the session started with.
    net: (Option<String>, bool),
}

pub(super) struct Browser {
    active: Option<Active>,
    next_id: u64,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    spawner: Box<dyn Spawn>,
    /// Discreet flag last sent to the helper.
    last_discreet: bool,
}

impl Drop for Browser {
    /// App quit (or any exit path): no helper outlives the app.
    fn drop(&mut self) {
        self.stop();
    }
}

impl Default for Browser {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            active: None,
            next_id: 1,
            tx,
            rx,
            spawner: Box::new(RealSpawn),
            last_discreet: false,
        }
    }
}

impl Browser {
    #[cfg(test)]
    pub(super) fn with_spawner(s: Box<dyn Spawn>) -> Self {
        let mut b = Self::default();
        b.spawner = s;
        b
    }

    pub(super) fn running(&self) -> bool {
        self.active.is_some()
    }

    /// Start a session. Returns a fixed sentence on failure.
    pub(super) fn start(
        &mut self,
        l: &Launch,
        wake: Box<dyn Fn() + Send>,
    ) -> Result<(), &'static str> {
        if self.active.is_some() {
            return Ok(());
        }
        let mut s = self
            .spawner
            .start(l)
            .map_err(|_| "The browser could not be started.")?;
        if write_message(&mut s.to, &l.hello).is_err() {
            (s.kill)();
            return Err("The browser could not be started.");
        }
        let id = self.next_id;
        self.next_id += 1;
        let tx = self.tx.clone();
        let from = s.from;
        std::thread::spawn(move || pump(from, id, &tx, &*wake));
        let net = match &l.hello {
            ToHelper::Hello {
                proxy,
                require_proxy,
                ..
            } => (proxy.clone(), *require_proxy),
            _ => (None, false),
        };
        self.last_discreet = matches!(&l.hello, ToHelper::Hello { discreet: true, .. });
        self.active = Some(Active {
            id,
            to: s.to,
            kill: Some(s.kill),
            net,
        });
        Ok(())
    }

    /// Kill the whole group and forget the session. Safe to call any time.
    pub(super) fn stop(&mut self) {
        if let Some(mut a) = self.active.take() {
            let _ = write_message(&mut a.to, &ToHelper::Close);
            if let Some(k) = a.kill.take() {
                k();
            }
        }
    }

    /// Tell a running helper the discreet flag changed.
    pub(super) fn discreet(&mut self, on: bool) {
        if let Some(a) = &mut self.active
            && write_message(&mut a.to, &ToHelper::Discreet { on }).is_err()
        {
            self.stop();
        }
    }

    /// True when the proxy or kill switch differ from what the session
    /// started with: it must end.
    pub(super) fn net_changed(&self, proxy: &Option<String>, require: bool) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| a.net != (proxy.clone(), require))
    }

    /// Reactions to what the helper sent since the last call.
    pub(super) fn drain(&mut self) -> Vec<Reaction> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            let cur = self.active.as_ref().map(|a| a.id);
            match ev {
                Event::Msg(id, m) if Some(id) == cur => {
                    let r = react(m);
                    if r == Reaction::Ended {
                        self.stop();
                    }
                    out.push(r);
                }
                Event::End(id) if Some(id) == cur => {
                    self.stop();
                    out.push(Reaction::Ended);
                }
                _ => {}
            }
        }
        out
    }
}

// ---- wiring into the app ---------------------------------------------------

impl JizzApp {
    /// Once per frame: apply what the helper said, and end a session whose
    /// network settings or privacy state no longer match.
    pub(super) fn browser_tick(&mut self) {
        if !self.browser.running() {
            // Drop stale events of ended sessions.
            self.browser.drain();
            return;
        }
        let cfg = self.downloads.host.config();
        let (proxy, require) = (
            cfg.proxy
                .as_ref()
                .filter(|p| !p.is_empty())
                .map(|p| p.expose().to_owned()),
            cfg.require_proxy,
        );
        if self.cfg.screen_share_safe || self.browser.net_changed(&proxy, require) {
            self.browser.stop();
            return;
        }
        if self.browser.last_discreet != self.cfg.discreet {
            self.browser.last_discreet = self.cfg.discreet;
            self.browser.discreet(self.cfg.discreet);
        }
        for r in self.browser.drain() {
            match r {
                Reaction::Nothing | Reaction::Ended => {}
                Reaction::Command(n, a) => self.push(n, a),
                Reaction::Say(s) => self.toast(ToastKind::Error, s),
            }
        }
    }

    pub(super) fn browser_open(&mut self) -> Result<(), &'static str> {
        let cfg = self.downloads.host.config();
        let prefs = cfg.browser.clone();
        let inputs = Inputs {
            exe: match crate::edition::browser_status() {
                crate::edition::BrowserStatus::Installed(p) => Some(p),
                _ => None,
            },
            prefs: &prefs,
            proxy: cfg
                .proxy
                .as_ref()
                .filter(|p| !p.is_empty())
                .map(|p| p.expose().to_owned()),
            require_proxy: cfg.require_proxy,
            discreet: self.cfg.discreet,
            disguised: self.opts.disguised || self.cfg.disguise,
            share_safe: self.cfg.screen_share_safe,
            age_ok: self.cfg.age_confirmed || self.cfg.skip_age_gate,
        };
        let launch = plan(&inputs, std::env::vars_os()).map_err(Refusal::sentence)?;
        let ctx = self.ctx.clone();
        self.browser
            .start(&launch, Box::new(move || ctx.request_repaint()))
    }
}

pub(super) fn register(reg: &mut jizzcraft_core::Registry<JizzApp>) {
    reg.register(
        "browser.open",
        "Open the built-in browser (online edition; not from the command line)",
        |app, _| {
            // Fixed sentences go straight to a toast (and are translated there).
            match app.browser_open() {
                Ok(()) => Ok(json!({ "opened": true })),
                Err(s) => {
                    app.toast(ToastKind::Error, s);
                    Ok(json!({ "opened": false }))
                }
            }
        },
    );
    reg.register(
        "browser.prefs.set",
        "Browser preferences: {enabled?, remember?, js?, media?, rules_path?, search_template?, software?}",
        |app, a| {
            let mut p = app.downloads.host.config().browser.clone();
            if let Err(s) = apply_prefs(&mut p, &a) {
                app.toast(ToastKind::Error, s);
                return Ok(json!({ "saved": false }));
            }
            app.downloads.host.update_settings(|c| c.browser = p)?;
            Ok(json!({ "saved": true }))
        },
    );
    reg.register(
        "browser.data.clear",
        "Remove the browser's saved sign-ins and site data (nothing else)",
        |app, _| {
            app.browser.stop();
            let dir = data_dir(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"));
            match dir.map(|d| clear_data(&d)) {
                Some(Ok(removed)) => Ok(json!({ "removed": removed })),
                _ => {
                    app.toast(ToastKind::Error, "The browser data could not be removed.");
                    Ok(json!({ "removed": false }))
                }
            }
        },
    );
    reg.register("browser.close", "Close the built-in browser", |app, _| {
        app.browser.stop();
        Ok(json!({}))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn inputs<'a>(p: &'a BrowserPrefs) -> Inputs<'a> {
        Inputs {
            exe: Some(PathBuf::from("/usr/libexec/jizzcraft/jizzcraft-browser")),
            prefs: p,
            proxy: Some("socks5://user:secret@127.0.0.1:9050".into()),
            require_proxy: true,
            discreet: true,
            disguised: false,
            share_safe: false,
            age_ok: true,
        }
    }

    fn env() -> Vec<(OsString, OsString)> {
        [
            "DISPLAY",
            "HOME",
            "LD_PRELOAD",
            "PATH",
            "http_proxy",
            "XDG_RUNTIME_DIR",
            "SECRET_TOKEN",
        ]
        .into_iter()
        .map(|k| (OsString::from(k), OsString::from("v")))
        .collect()
    }

    #[test]
    fn argv_has_no_proxy_and_env_is_cleared() {
        let p = BrowserPrefs::default();
        let l = plan(&inputs(&p), env().into_iter()).unwrap();
        assert!(l.argv.is_empty());
        for a in &l.argv {
            assert!(!a.contains("socks5") && !a.contains("secret"));
        }
        let keys: Vec<&str> = l.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["DISPLAY", "HOME", "XDG_RUNTIME_DIR"]);
        // The proxy travels in Hello only.
        match l.hello {
            ToHelper::Hello {
                proxy,
                require_proxy,
                discreet,
                disguised,
                remember,
                ..
            } => {
                assert_eq!(
                    proxy.as_deref(),
                    Some("socks5://user:secret@127.0.0.1:9050")
                );
                assert!(require_proxy && discreet && !disguised && !remember);
            }
            _ => panic!("not a hello"),
        }
    }

    #[test]
    fn refusals() {
        let mut p = BrowserPrefs::default();
        let mut i = inputs(&p);
        i.proxy = None;
        assert_eq!(plan(&i, env().into_iter()), Err(Refusal::ProxyRequired));
        i.require_proxy = false;
        assert!(plan(&i, env().into_iter()).is_ok());
        i.share_safe = true;
        assert_eq!(plan(&i, env().into_iter()), Err(Refusal::ShareSafe));
        i.share_safe = false;
        i.exe = None;
        assert_eq!(plan(&i, env().into_iter()), Err(Refusal::NotInstalled));
        i.exe = Some("/x".into());
        i.age_ok = false;
        assert_eq!(plan(&i, env().into_iter()), Err(Refusal::AgeGate));
        p.enabled = false;
        let mut j = inputs(&p);
        j.age_ok = true;
        assert_eq!(plan(&j, env().into_iter()), Err(Refusal::Disabled));
    }

    #[test]
    fn prefs_reach_the_hello() {
        let p = BrowserPrefs {
            remember: true,
            js: false,
            media: false,
            rules_path: Some("/home/u/rules.json".into()),
            search_template: Some(" ".into()),
            software: true,
            ..BrowserPrefs::default()
        };
        let mut i = inputs(&p);
        i.disguised = true;
        match plan(&i, std::iter::empty()).unwrap().hello {
            ToHelper::Hello {
                js,
                media,
                remember,
                rules_path,
                search_template,
                software,
                disguised,
                ..
            } => {
                assert!(!js && !media && remember && software && disguised);
                assert_eq!(rules_path.as_deref(), Some("/home/u/rules.json"));
                assert_eq!(search_template, None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn data_dir_follows_xdg_then_home() {
        assert_eq!(
            data_dir(Some("/x/data".into()), Some("/h".into())),
            Some(PathBuf::from("/x/data/jizzcraft/browser"))
        );
        assert_eq!(
            data_dir(None, Some("/h".into())),
            Some(PathBuf::from("/h/.local/share/jizzcraft/browser"))
        );
        assert_eq!(data_dir(Some("rel".into()), None), None);
    }

    #[test]
    fn prefs_round_trip_and_validation() {
        let mut p = BrowserPrefs::default();
        assert!(p.enabled && p.js && p.media && !p.remember && !p.software);
        assert!(p.rules_path.is_none() && p.search_template.is_none());
        apply_prefs(
            &mut p,
            &json!({"remember": true, "js": false, "software": true,
                    "rules_path": "/home/u/r.json",
                    "search_template": "https://s.example/?q={}"}),
        )
        .unwrap();
        assert!(p.remember && !p.js && p.software && p.media);
        let back: BrowserPrefs = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        let before = p.clone();
        for bad in [
            json!({"rules_path": "relative.json"}),
            json!({"search_template": "https://s.example/"}),
            json!({"search_template": "ftp://s.example/{}"}),
            json!({"search_template": "https://s.example/{} {}"}),
        ] {
            assert!(apply_prefs(&mut p, &bad).is_err());
            assert_eq!(p, before);
        }
        apply_prefs(&mut p, &json!({"rules_path": "", "search_template": ""})).unwrap();
        assert!(p.rules_path.is_none() && p.search_template.is_none());
        // An old config without the block gets the defaults.
        let c: jizzcraft_fetch::FetchConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.browser, BrowserPrefs::default());
    }

    #[test]
    fn clear_data_touches_only_the_browser_folder() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let ours = root.join("jizzcraft/browser");
        std::fs::create_dir_all(ours.join("cookies")).unwrap();
        std::fs::write(ours.join("cookies/a"), b"x").unwrap();
        let other = root.join("jizzcraft/library.db");
        std::fs::write(&other, b"keep").unwrap();
        assert!(clear_data(root).is_err());
        assert!(clear_data(&root.join("jizzcraft")).is_err());
        assert!(other.exists());
        assert!(clear_data(&ours).unwrap());
        assert!(!ours.exists());
        assert!(other.exists());
        assert!(!clear_data(&ours).unwrap());
    }

    #[test]
    fn error_codes_become_fixed_sentences() {
        for c in [
            "engine_missing",
            "proxy_required",
            "bad_hello",
            "no_display",
            "profile_unwritable",
            "<script>",
        ] {
            let s = error_sentence(c);
            assert!(!s.contains('<') && !s.contains('_'), "{s}");
        }
        assert_eq!(error_sentence("x/y z"), "The browser stopped.");
    }

    #[test]
    fn every_fixed_sentence_is_translated() {
        let mut all: Vec<&str> = [
            Refusal::AgeGate,
            Refusal::Disabled,
            Refusal::NotInstalled,
            Refusal::ProxyRequired,
            Refusal::ShareSafe,
        ]
        .iter()
        .map(|r| r.sentence())
        .collect();
        for c in [
            "engine_missing",
            "proxy_required",
            "no_display",
            "profile_unwritable",
            "bad_hello",
            "x",
        ] {
            all.push(error_sentence(c));
        }
        for t in [
            crate::lang::fr::TABLE,
            crate::lang::es::TABLE,
            crate::lang::de::TABLE,
            crate::lang::it::TABLE,
            crate::lang::pt::TABLE,
        ] {
            for s in &all {
                assert!(t.iter().any(|(en, _)| en == s), "untranslated: {s}");
            }
        }
    }

    #[test]
    fn reactions() {
        assert_eq!(
            react(FromHelper::Play {
                url: "HTTPS://Example.com/v".into(),
                title: Some("t".into())
            }),
            Reaction::Command("stream.open", json!({ "url": "https://example.com/v" }))
        );
        assert!(matches!(
            react(FromHelper::Play {
                url: "file:///etc/passwd".into(),
                title: None
            }),
            Reaction::Say(_)
        ));
        assert_eq!(
            react(FromHelper::Boss),
            Reaction::Command("app.boss_key", json!({}))
        );
        assert_eq!(react(FromHelper::Closed), Reaction::Ended);
    }

    #[test]
    fn pump_handles_play_boss_and_ignores_oversized_lines() {
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"{\"t\":\"play\",\"url\":\"https://example.com/a\"}\n");
        data.extend(std::iter::repeat_n(b'x', jizzcraft_bridge::MAX_LINE * 3));
        data.push(b'\n');
        data.extend_from_slice(b"garbage\n{\"t\":\"boss\"}\n");
        let (tx, rx) = channel();
        pump(
            io::BufReader::with_capacity(64, io::Cursor::new(data)),
            7,
            &tx,
            &|| {},
        );
        let evs: Vec<Reaction> = rx
            .try_iter()
            .filter_map(|e| match e {
                Event::Msg(7, m) => Some(react(m)),
                _ => None,
            })
            .collect();
        assert_eq!(
            evs,
            vec![
                Reaction::Command("stream.open", json!({ "url": "https://example.com/a" })),
                Reaction::Command("app.boss_key", json!({})),
            ]
        );
    }

    // ---- a fake spawner: no process at all -------------------------------

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Fake {
        written: Shared,
        killed: Arc<Mutex<u32>>,
        script: Vec<u8>,
        started: Arc<Mutex<Vec<Launch>>>,
    }
    impl Spawn for Fake {
        fn start(&self, l: &Launch) -> io::Result<Started> {
            self.started.lock().unwrap().push(l.clone());
            let k = self.killed.clone();
            Ok(Started {
                to: Box::new(self.written.clone()),
                from: Box::new(io::Cursor::new(self.script.clone())),
                kill: Box::new(move || *k.lock().unwrap() += 1),
            })
        }
    }

    #[test]
    fn session_hello_then_events_then_kill() {
        let written = Shared::default();
        let killed = Arc::new(Mutex::new(0));
        let started = Arc::new(Mutex::new(Vec::new()));
        let script = b"{\"t\":\"ready\",\"engine\":\"x\"}\n{\"t\":\"boss\"}\n".to_vec();
        let mut b = Browser::with_spawner(Box::new(Fake {
            written: written.clone(),
            killed: killed.clone(),
            script,
            started: started.clone(),
        }));
        let p = BrowserPrefs::default();
        let l = plan(&inputs(&p), env().into_iter()).unwrap();
        b.start(&l, Box::new(|| {})).unwrap();
        // Hello was the first line written.
        let first = {
            let w = written.0.lock().unwrap();
            let line = w.split(|c| *c == b'\n').next().unwrap().to_vec();
            String::from_utf8(line).unwrap()
        };
        assert!(first.contains("\"t\":\"hello\""));
        // The reader thread delivers boss, then end of stream.
        let mut got = Vec::new();
        for _ in 0..200 {
            got.extend(b.drain());
            if got.contains(&Reaction::Ended) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(got.contains(&Reaction::Command("app.boss_key", json!({}))));
        assert!(got.contains(&Reaction::Ended));
        assert!(!b.running());
        assert_eq!(*killed.lock().unwrap(), 1);
        assert_eq!(started.lock().unwrap().len(), 1);
    }

    #[test]
    fn net_change_is_detected_and_stop_is_idempotent() {
        let killed = Arc::new(Mutex::new(0));
        let mut b = Browser::with_spawner(Box::new(Fake {
            written: Shared::default(),
            killed: killed.clone(),
            script: Vec::new(),
            started: Arc::default(),
        }));
        let p = BrowserPrefs::default();
        let i = inputs(&p);
        let l = plan(&i, std::iter::empty()).unwrap();
        b.start(&l, Box::new(|| {})).unwrap();
        assert!(b.running());
        assert!(!b.net_changed(&i.proxy, true));
        assert!(b.net_changed(&None, true));
        assert!(b.net_changed(&i.proxy, false));
        b.stop();
        b.stop();
        assert_eq!(*killed.lock().unwrap(), 1);
    }
}
