//! WebKitGTK 6.0 engine. Built only with `--features engine`.
//!
//! NOT compiled in CI by default: written against gtk4 0.11 / webkit6 0.6.

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use jizzcraft_bridge::{FromHelper, ReadError, ToHelper, check_play_url};
use webkit6::prelude::*;
use webkit6::{
    ContextMenuAction, NavigationPolicyDecision, NetworkProxyMode, NetworkProxySettings,
    NetworkSession, PolicyDecisionType, Settings, UserContentFilterStore, UserContentManager,
    WebView,
};

use crate::policy::*;
use crate::selftest::{self, Obs};

type Sender = fn(&FromHelper);

const START_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Viewer</title>\
<body style=\"font:16px sans-serif;margin:3em;color:#666;background:#fafafa\">";

fn private_dir(p: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(p)?;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700))
}

/// Remove folders left by dead helpers. Never follows symlinks.
fn wipe_stale(runtime: &Path, own: u32) {
    let Ok(rd) = fs::read_dir(runtime) else {
        return;
    };
    let names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    for n in stale_names(&names, own, |pid| {
        Path::new(&format!("/proc/{pid}")).exists()
    }) {
        let p = runtime.join(&n);
        if fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
            let _ = fs::remove_dir_all(p);
        }
    }
}

struct Cleanup(Option<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_dir_all(p);
        }
    }
}

/// Hidden self-test modes (see `selftest.rs`).
#[derive(Clone)]
pub enum SelfTest {
    /// Run the policy suite against loopback servers.
    Suite,
    /// Load one URL (through the Hello proxy, if any) and report.
    Load(String),
}

pub fn run(
    s: Session,
    rx: Receiver<Result<ToHelper, ReadError>>,
    send: Sender,
    st: Option<SelfTest>,
) -> i32 {
    let obs: Option<Rc<Obs>> = st.as_ref().map(|_| Rc::new(Obs::default()));
    if s.software {
        // SAFETY: single-threaded here; no other thread reads the environment yet
        // except the stdin reader, which does not touch it.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    let own = std::process::id();
    let env_path = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let (rt, data, home) = (
        env_path("XDG_RUNTIME_DIR"),
        env_path("XDG_DATA_HOME"),
        env_path("HOME"),
    );
    if let Some(rt) = &rt {
        wipe_stale(rt, own);
    }
    let profile = profile_dir(
        s.remember,
        rt.as_deref(),
        data.as_deref(),
        home.as_deref(),
        own,
    );
    if let Some(p) = &profile
        && private_dir(&p.root).is_err()
    {
        send(&FromHelper::Error {
            code: "profile_unwritable".into(),
        });
        return 1;
    }
    let _cleanup = Cleanup(
        profile
            .as_ref()
            .filter(|p| p.ephemeral)
            .map(|p| p.root.clone()),
    );

    if gtk4::init().is_err() {
        send(&FromHelper::Error {
            code: "no_display".into(),
        });
        return 1;
    }
    glib::set_application_name(WINDOW_TITLE);
    glib::set_prgname(Some(if s.disguised {
        "local.Viewer"
    } else {
        "jizzcraft-browser"
    }));

    // Session: in memory unless "remember".
    let session = match (&profile, s.remember) {
        (Some(p), true) => {
            let (d, c) = (p.root.join("data"), p.root.join("cache"));
            if private_dir(&d).is_err() || private_dir(&c).is_err() {
                send(&FromHelper::Error {
                    code: "profile_unwritable".into(),
                });
                return 1;
            }
            NetworkSession::new(d.to_str(), c.to_str())
        }
        _ => NetworkSession::new_ephemeral(),
    };
    if s.remember
        && let (Some(p), Some(cm)) = (&profile, session.cookie_manager())
    {
        cm.set_persistent_storage(
            p.root.join("data/cookies.txt").to_str().unwrap_or_default(),
            webkit6::CookiePersistentStorage::Text,
        );
    }
    if let Some(proxy) = &s.proxy {
        let ps = NetworkProxySettings::new(Some(proxy), &[]);
        session.set_proxy_settings(NetworkProxyMode::Custom, Some(&ps));
    } else {
        session.set_proxy_settings(NetworkProxyMode::NoProxy, None);
    }

    // Every download is cancelled; the page gets a visible notice.
    let notice = gtk4::Label::new(None);
    notice.set_visible(false);
    notice.set_wrap(true);
    {
        let (notice, obs) = (notice.clone(), obs.clone());
        session.connect_download_started(move |_, dl| {
            dl.cancel();
            if let Some(o) = &obs {
                o.downloads.set(o.downloads.get() + 1);
            }
            notice.set_text(DOWNLOAD_NOTICE);
            notice.set_visible(true);
        });
    }

    let settings = Settings::new();
    settings.set_enable_javascript(s.js);
    settings.set_javascript_can_open_windows_automatically(false);
    settings.set_javascript_can_access_clipboard(false);
    settings.set_enable_webrtc(false);
    settings.set_enable_media_stream(false);
    settings.set_enable_mediasource(false);
    settings.set_allow_modal_dialogs(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings.set_enable_encrypted_media(false);
    settings.set_enable_media(s.media);
    settings.set_enable_webaudio(s.media);
    settings.set_media_playback_requires_user_gesture(true);
    settings.set_media_playback_allows_inline(true);
    settings.set_enable_page_cache(false);
    settings.set_enable_developer_extras(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    // DNS prefetch and form autofill: WebKitGTK 6.0 has no such features to switch on.

    let ucm = UserContentManager::new();
    if let (Some(rules), Some(p)) = (&s.rules_path, &profile) {
        // Content rules come only from the user's file; nothing is bundled.
        let store_dir = p.root.join("filters");
        if private_dir(&store_dir).is_ok() {
            let store = UserContentFilterStore::new(store_dir.to_str().unwrap_or_default());
            let ucm = ucm.clone();
            let file = gio::File::for_path(rules);
            let keep = store.clone();
            store.save_from_file("user-rules", &file, None::<&gio::Cancellable>, move |res| {
                let _keep = &keep;
                if let Ok(filter) = res {
                    ucm.add_filter(&filter);
                }
            });
        }
    }

    let view: WebView = glib::Object::builder()
        .property("network-session", &session)
        .property("settings", &settings)
        .property("user-content-manager", &ucm)
        .build();
    view.set_hexpand(true);
    view.set_vexpand(true);

    // Popups: nothing is ever created.
    {
        let obs = obs.clone();
        view.connect_create(move |_, _| {
            if let Some(o) = &obs {
                o.windows_requested.set(o.windows_requested.get() + 1);
            }
            None
        });
    }
    // Permissions: always denied.
    {
        let obs = obs.clone();
        view.connect_permission_request(move |_, req| {
            req.deny();
            if let Some(o) = &obs {
                o.permission_requests.set(o.permission_requests.get() + 1);
            }
            true
        });
    }
    // Never show a desktop notification.
    {
        let obs = obs.clone();
        view.connect_show_notification(move |_, _| {
            if let Some(o) = &obs {
                o.notifications_shown.set(o.notifications_shown.get() + 1);
            }
            true
        });
    }
    // Top-level navigation filter.
    {
        let (notice, obs) = (notice.clone(), obs.clone());
        view.connect_decide_policy(move |_, decision, ty| {
            match ty {
                PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                    let uri = decision
                        .downcast_ref::<NavigationPolicyDecision>()
                        .and_then(|d| d.navigation_action())
                        .and_then(|a| a.request())
                        .and_then(|r| r.uri())
                        .map(|u| u.to_string())
                        .unwrap_or_default();
                    if ty == PolicyDecisionType::NewWindowAction || !navigation_allowed(&uri) {
                        if let Some(o) = &obs {
                            o.refused_navigations.borrow_mut().push(uri);
                        }
                        decision.ignore();
                        return true;
                    }
                    false
                }
                PolicyDecisionType::Response => {
                    // A response WebKit would turn into a download.
                    if let Some(r) = decision.downcast_ref::<webkit6::ResponsePolicyDecision>()
                        && !r.is_mime_type_supported()
                    {
                        decision.ignore();
                        if let Some(o) = &obs {
                            o.downloads.set(o.downloads.get() + 1);
                        }
                        notice.set_text(DOWNLOAD_NOTICE);
                        notice.set_visible(true);
                        return true;
                    }
                    false
                }
                _ => false,
            }
        });
    }

    let pid_play = send;
    let play = {
        let view = view.clone();
        move |link: Option<String>| {
            let target = link
                .or_else(|| view.uri().map(|u| u.to_string()))
                .unwrap_or_default();
            match check_play_url(&target) {
                Ok(url) => pid_play(&FromHelper::Play { url, title: None }),
                Err(_) => pid_play(&FromHelper::Error {
                    code: "bad_play_url".into(),
                }),
            }
        }
    };
    let play = Rc::new(play);

    // Context menu: drop everything that opens windows or downloads; add Play.
    {
        let play = play.clone();
        let link: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let action = gio::SimpleAction::new("play-link", None);
        {
            let link = link.clone();
            action.connect_activate(move |_, _| play(link.borrow().clone()));
        }
        view.connect_context_menu(move |_, menu, hit| {
            use ContextMenuAction as A;
            for item in menu.items() {
                if matches!(
                    item.stock_action(),
                    A::OpenLinkInNewWindow
                        | A::DownloadLinkToDisk
                        | A::DownloadImageToDisk
                        | A::DownloadVideoToDisk
                        | A::DownloadAudioToDisk
                        | A::OpenImageInNewWindow
                        | A::OpenFrameInNewWindow
                        | A::OpenVideoInNewWindow
                        | A::OpenAudioInNewWindow
                ) {
                    menu.remove(&item);
                }
            }
            if hit.context_is_link() {
                *link.borrow_mut() = hit.link_uri().map(|u| u.to_string());
                let item = webkit6::ContextMenuItem::from_gaction(
                    action.upcast_ref::<gio::Action>(),
                    "Play link in JizzCraft",
                    None,
                );
                menu.append(&item);
            }
            false
        });
    }

    // ---- Window ----------------------------------------------------------
    let app_id = if s.disguised {
        "local.Viewer"
    } else {
        "local.jizzcraft.Browser"
    };
    let app = gtk4::Application::builder()
        .application_id(app_id)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let exit_code = Rc::new(std::cell::Cell::new(0));
    let servers: Option<Rc<selftest::Servers>> = match &st {
        Some(SelfTest::Suite) => match selftest::start() {
            Ok(sv) => Some(Rc::new(sv)),
            Err(_) => return 1,
        },
        _ => None,
    };
    let session = Rc::new(session);
    let rx = Rc::new(rx);
    let s = Rc::new(s);
    {
        let (view, notice, play, exit_code, s) = (
            view.clone(),
            notice.clone(),
            play.clone(),
            exit_code.clone(),
            s.clone(),
        );
        let (obs, st, servers, session) =
            (obs.clone(), st.clone(), servers.clone(), session.clone());
        app.connect_activate(move |app| {
            let win = gtk4::ApplicationWindow::builder()
                .application(app)
                .title(WINDOW_TITLE)
                .default_width(1100)
                .default_height(740)
                .build();
            let entry = gtk4::Entry::new();
            entry.set_hexpand(true);
            entry.set_placeholder_text(Some("Address"));
            let back = gtk4::Button::from_icon_name("go-previous-symbolic");
            let fwd = gtk4::Button::from_icon_name("go-next-symbolic");
            let reload = gtk4::Button::from_icon_name("view-refresh-symbolic");
            let playb = gtk4::Button::with_label("Play this page");
            let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
            for w in [
                back.upcast_ref::<gtk4::Widget>(),
                fwd.upcast_ref(),
                reload.upcast_ref(),
            ] {
                bar.append(w);
            }
            bar.append(&entry);
            bar.append(&playb);
            let col = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            col.append(&bar);
            col.append(&notice);
            col.append(&view);
            win.set_child(Some(&col));

            let v = view.clone();
            back.connect_clicked(move |_| v.go_back());
            let v = view.clone();
            fwd.connect_clicked(move |_| v.go_forward());
            let v = view.clone();
            reload.connect_clicked(move |_| v.reload());
            let p = play.clone();
            playb.connect_clicked(move |_| p(None));

            let (v, n, s2) = (view.clone(), notice.clone(), s.clone());
            entry.connect_activate(move |e| {
                n.set_visible(false);
                match resolve_address(&e.text(), s2.search_template.as_deref()) {
                    Address::Go(u) => v.load_uri(&u),
                    Address::NoSearch => {
                        v.load_html(&format!("{START_PAGE}<p>{NO_SEARCH_PAGE}</p>"), None)
                    }
                    Address::Refused => v.load_html(
                        &format!("{START_PAGE}<p>That address is not allowed.</p>"),
                        None,
                    ),
                }
            });
            // Keep the address entry in step with the view; never the title.
            let e = entry.clone();
            view.connect_uri_notify(move |v| {
                if let Some(u) = v.uri()
                    && u != "about:blank"
                {
                    e.set_text(&u);
                }
            });

            // Ctrl+Shift+H: Boss and out, at once.
            let obs_key = obs.clone();
            let keys = gtk4::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, mods| {
                let want =
                    gtk4::gdk::ModifierType::CONTROL_MASK | gtk4::gdk::ModifierType::SHIFT_MASK;
                if matches!(key, gtk4::gdk::Key::H | gtk4::gdk::Key::h) && mods.contains(want) {
                    if let Some(o) = &obs_key {
                        o.boss.set(true);
                        return glib::Propagation::Stop;
                    }
                    send(&FromHelper::Boss);
                    std::process::exit(0);
                }
                glib::Propagation::Proceed
            });
            win.add_controller(keys.clone());

            let a = app.clone();
            win.connect_close_request(move |_| {
                send(&FromHelper::Closed);
                a.quit();
                glib::Propagation::Proceed
            });

            win.present();
            if let (Some(st), Some(obs)) = (st.clone(), obs.clone()) {
                let ctx = Drive {
                    view: view.clone(),
                    win: win.clone(),
                    app: app.clone(),
                    obs,
                    servers: servers.clone(),
                    code: exit_code.clone(),
                    ephemeral: session.is_ephemeral(),
                    keys: keys.clone(),
                    notice: notice.clone(),
                };
                drive(st, ctx);
                return;
            }
            view.load_html(START_PAGE, None);

            // Messages from the app, polled on the main loop.
            let (rx, v, a, win2, code) = (
                rx.clone(),
                view.clone(),
                app.clone(),
                win.clone(),
                exit_code.clone(),
            );
            glib::timeout_add_local(Duration::from_millis(100), move || {
                while let Ok(m) = rx.try_recv() {
                    match m {
                        Ok(ToHelper::Close) | Err(ReadError::Eof) | Err(ReadError::Io(_)) => {
                            send(&FromHelper::Closed);
                            code.set(0);
                            a.quit();
                            return glib::ControlFlow::Break;
                        }
                        Ok(ToHelper::Discreet { on }) => {
                            // Hidden and muted; the page keeps its state.
                            v.set_is_muted(on);
                            v.set_visible(!on);
                            if on {
                                win2.minimize();
                            }
                        }
                        _ => {}
                    }
                }
                glib::ControlFlow::Continue
            });
            if s.discreet {
                view.set_is_muted(true);
            }
            send(&FromHelper::Ready {
                engine: "webkitgtk-6.0".into(),
            });
        });
    }
    app.run_with_args::<&str>(&[]);
    exit_code.get()
}

struct Drive {
    view: WebView,
    win: gtk4::ApplicationWindow,
    app: gtk4::Application,
    obs: Rc<Obs>,
    servers: Option<Rc<selftest::Servers>>,
    code: Rc<std::cell::Cell<i32>>,
    ephemeral: bool,
    keys: gtk4::EventControllerKey,
    notice: gtk4::Label,
}

fn line(v: serde_json::Value) {
    println!("{v}");
}

fn drive(st: SelfTest, c: Drive) {
    let c = Rc::new(c);
    match st {
        SelfTest::Load(url) => {
            let done = Rc::new(std::cell::Cell::new(false));
            let report = {
                let (c, done) = (c.clone(), done.clone());
                Rc::new(move |finished: bool, why: &str| {
                    if done.replace(true) {
                        return;
                    }
                    line(serde_json::json!({
                        "check": "load", "finished": finished, "detail": why,
                        "uri": c.view.uri().map(|u| u.to_string()),
                    }));
                    c.code.set(if finished { 0 } else { 1 });
                    c.app.quit();
                })
            };
            {
                let r = report.clone();
                c.view.connect_load_changed(move |_, ev| {
                    if ev == webkit6::LoadEvent::Finished {
                        r(true, "finished");
                    }
                });
                let r = report.clone();
                c.view.connect_load_failed(move |_, _, _, err| {
                    r(false, &format!("load_failed:{}", err.message()));
                    true
                });
                let r = report.clone();
                glib::timeout_add_local_once(Duration::from_secs(10), move || r(false, "timeout"));
            }
            c.view.load_uri(&url);
        }
        SelfTest::Suite => {
            let Some(sv) = c.servers.clone() else {
                c.code.set(1);
                c.app.quit();
                return;
            };
            c.view.load_uri(&format!("http://{}/", sv.main));
            let started = std::time::Instant::now();
            let c2 = c.clone();
            glib::timeout_add_local(Duration::from_millis(500), move || {
                let c = c2.clone();
                let timed_out = started.elapsed() > Duration::from_secs(12);
                c2.view.evaluate_javascript(
                    "(window.R && R.done) ? JSON.stringify(R) : ''",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    move |res| {
                        let got = res
                            .ok()
                            .filter(|v| v.is_string())
                            .map(|v| v.to_str().to_string())
                            .unwrap_or_default();
                        if !got.is_empty() || timed_out {
                            finish(&c, &got);
                        }
                    },
                );
                if timed_out {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        }
    }
}

fn finish(c: &Rc<Drive>, got: &str) {
    thread_local!(static ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if ONCE.with(|o| o.replace(true)) {
        return;
    }
    // Drive the navigation filter itself: WebKit refuses file: from an http
    // page before any policy decision, so ask for it from the helper side.
    let before = c.view.uri().map(|u| u.to_string()).unwrap_or_default();
    c.view.load_uri("file:///etc/hostname");
    let (c, got) = (c.clone(), got.to_owned());
    glib::timeout_add_local_once(Duration::from_millis(1200), move || {
        let mut got = got;
        let after = c.view.uri().map(|u| u.to_string()).unwrap_or_default();
        report(&c, &got, &before, &after);
        got.clear();
    });
}

fn report(c: &Rc<Drive>, got: &str, before: &str, after: &str) {
    let r: serde_json::Value = serde_json::from_str(got).unwrap_or(serde_json::json!({}));
    let s = |k: &str| r.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let not_granted = |v: &Option<String>| !v.as_deref().is_some_and(|x| x.starts_with("granted"));
    let hits = c.servers.as_ref().map(|s| s.hits.clone());
    let n = |p: &str| hits.as_ref().map_or(0, |h| selftest::count(h, p));
    let o = &c.obs;
    let mut all = !got.is_empty();
    let mut check = |name: &str, blocked: bool, extra: serde_json::Value| {
        all &= blocked;
        line(serde_json::json!({"check": name, "blocked": blocked, "detail": extra}));
    };
    check(
        "popup",
        r.get("popup_null") == Some(&serde_json::json!(true))
            && n("main/popup") == 0
            && n("main/popup2") == 0,
        serde_json::json!({"window_open_returned_null": r.get("popup_null"),
            "create_signals": o.windows_requested.get(),
            "popup_requests_at_server": n("main/popup") + n("main/popup2"),
            "refused_new_window_actions": o.refused_navigations.borrow().len()}),
    );
    check(
        "download",
        o.downloads.get() >= 1 && c.notice.is_visible() && c.notice.text() == DOWNLOAD_NOTICE,
        serde_json::json!({"cancelled": o.downloads.get(), "notice": c.notice.text().to_string(),
            "server_hits": n("main/file.bin")}),
    );
    check(
        "geolocation",
        not_granted(&s("geo")),
        serde_json::json!({"page_result": s("geo")}),
    );
    check(
        "notifications",
        not_granted(&s("notif")) && o.notifications_shown.get() == 0,
        serde_json::json!({"page_result": s("notif"), "shown": o.notifications_shown.get()}),
    );
    check(
        "camera_microphone",
        not_granted(&s("cam")),
        serde_json::json!({"page_result": s("cam"),
            "permission_requests_total": o.permission_requests.get()}),
    );
    let final_uri = after.to_owned();
    check(
        "file_navigation",
        s("final_scheme").as_deref() == Some("http:")
            && final_uri == before
            && o.refused_navigations
                .borrow()
                .iter()
                .any(|u| u.starts_with("file://")),
        serde_json::json!({"final_uri": final_uri,
                   "page_script_attempt_scheme_after": s("final_scheme"),
                   "refused_by_filter": o.refused_navigations.borrow().clone(),
        }),
    );
    check(
        "cross_origin_fetch",
        s("cross_fetch").as_deref() == Some("blocked"),
        serde_json::json!({"page_result": s("cross_fetch"),
            "request_reached_server": n("other/x"),
            "note": "same-origin policy blocks the read; the request itself is not stopped"}),
    );
    check(
        "webrtc",
        s("webrtc").as_deref() == Some("absent"),
        serde_json::json!({"page_result": s("webrtc")}),
    );
    check(
        "eme",
        s("eme").as_deref() == Some("absent"),
        serde_json::json!({"page_result": s("eme")}),
    );
    check(
        "autoplay",
        !s("autoplay").as_deref().is_some_and(|x| x == "played"),
        serde_json::json!({"page_result": s("autoplay")}),
    );
    check(
        "window_title",
        c.win.title().as_deref() == Some(WINDOW_TITLE)
            && c.view.title().as_deref() == Some("Secret page title"),
        serde_json::json!({"window": c.win.title().map(|t| t.to_string()),
            "page": c.view.title().map(|t| t.to_string())}),
    );
    let id = c.app.application_id().map(|g| g.to_string());
    check(
        "app_id_disguised",
        id.as_deref() == Some("local.Viewer"),
        serde_json::json!({"app_id": id}),
    );
    check(
        "session_ephemeral",
        c.ephemeral,
        serde_json::json!({"ephemeral": c.ephemeral}),
    );
    // The handler is run by emitting the controller's signal; a physical
    // keystroke is not injected.
    let _ = c.keys.emit_by_name::<bool>(
        "key-pressed",
        &[
            &gtk4::gdk::Key::H,
            &0u32,
            &(gtk4::gdk::ModifierType::CONTROL_MASK | gtk4::gdk::ModifierType::SHIFT_MASK),
        ],
    );
    check(
        "boss_key_handler",
        o.boss.get(),
        serde_json::json!({"via": "signal emission, not a physical keystroke"}),
    );
    c.code.set(if all { 0 } else { 1 });
    c.app.quit();
}
