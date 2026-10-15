//! JizzCraft desktop app (eframe + wgpu).

// No console window in a Windows release build. `--help`, `--doctor` and
// `--bench` re-attach to the parent console in `attach_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use jizzcraft_ui::{Options, Root, config::Config};
use std::path::PathBuf;

const HELP: &str = "\
JizzCraft — private, offline-first media viewer

USAGE: jizzcraft [OPTIONS]

OPTIONS:
  --screenshot <FILE.png>   render the UI once to a PNG and exit
  --import <DIR>            import a folder at startup
  --db <FILE>               use this library database
  --tab <NAME>              library | images | videos | favorites
  --open <N>                open the N-th item of the list at startup
  --cmd <NAME[=JSON]>       run a registry command after startup (repeatable)
  --bench <FILE>            play FILE and report dropped frames, p95 UI time, CPU, RSS
  --seconds <N>             bench duration (default 30)
  --bench-gst-play          also measure gst-play-1.0 on the same file
  --assume-adult            (screenshots only) skip the age gate without saving
  magnet:…                  play a magnet link (network build)
  --send <URL>              hand an http(s) address to the running app, which asks before playing (online edition)
  https://…                 same as --send URL
  --doctor                  check codecs and folders, print how to fix what is missing
  --lang <CODE>             interface language: en fr es de it pt (default: system)
  --renderer <NAME>         graphics backend: auto (default), vulkan or gl
  --disguise                launch as a neutral \"Viewer\" (window name, app ID, discreet mode)
  --portable                keep database, config and cache in ./jizzcraft-data next to the executable
  --home <DIR>              keep database, config and cache under DIR
  -h, --help                show this help

ENV:
  JIZZCRAFT_DEBUG=1         log full paths (off by default)
  JIZZCRAFT_HOME=<DIR>      same as --home
  JIZZCRAFT_RENDERER=<NAME> same as --renderer
";

/// Everything the command line can ask for. Parsing has no side effects so it
/// can be tested; `main` applies the result.
#[derive(Default)]
struct Cli {
    opts: Options,
    assume_adult: bool,
    bench_file: Option<PathBuf>,
    bench_secs: u32,
    bench_gst: bool,
    disguise: bool,
    magnets: Vec<String>,
    /// Addresses for "Send to JizzCraft" (`--send URL` or a plain http(s) argument).
    sends: Vec<String>,
    portable: bool,
    home: Option<PathBuf>,
    help: bool,
    doctor: bool,
    renderer: Option<String>,
}

/// `Err` carries the message for an option the parser refuses.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli {
        bench_secs: 30,
        ..Cli::default()
    };
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            // The desktop's magnet handler passes the link as a plain argument.
            s if s.starts_with("magnet:") => cli.magnets.push(s.to_owned()),
            s if has_web_scheme(s) => cli.sends.push(s.to_owned()),
            "--send" => match args.next() {
                Some(u) => cli.sends.push(u),
                None => return Err("--send: expected an address".to_owned()),
            },
            "--screenshot" => cli.opts.screenshot = args.next().map(PathBuf::from),
            "--import" => cli.opts.import = args.next().map(PathBuf::from),
            "--db" => cli.opts.db_path = args.next().map(PathBuf::from),
            "--tab" => cli.opts.tab = args.next(),
            "--open" => cli.opts.open_index = args.next().and_then(|v| v.parse().ok()),
            "--cmd" => {
                // name or name={"json":"args"}
                let raw = args.next().unwrap_or_default();
                let (name, json) = raw.split_once('=').unwrap_or((raw.as_str(), "{}"));
                match serde_json::from_str(json) {
                    Ok(v) => cli.opts.commands.push((name.to_owned(), v)),
                    Err(e) => return Err(format!("--cmd: bad JSON arguments: {e}")),
                }
            }
            "--bench" => cli.bench_file = args.next().map(PathBuf::from),
            "--seconds" => cli.bench_secs = args.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--bench-gst-play" => cli.bench_gst = true,
            "--assume-adult" => cli.assume_adult = true,
            "--disguise" => cli.disguise = true,
            "--lang" => cli.opts.lang = args.next(),
            "--portable" => cli.portable = true,
            "--doctor" => cli.doctor = true,
            "--renderer" => {
                let v = args.next().unwrap_or_default();
                if renderer_backends(&v).is_none() {
                    return Err(format!(
                        "--renderer: expected auto, vulkan or gl, got `{v}`"
                    ));
                }
                cli.renderer = Some(v);
            }
            "--home" => cli.home = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                cli.help = true;
                return Ok(cli);
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(cli)
}

fn has_web_scheme(s: &str) -> bool {
    let l = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// Why `--send` cannot work in this build, if it cannot.
fn send_refusal(sends: &[String]) -> Option<&'static str> {
    (!cfg!(feature = "network") && !sends.is_empty())
        .then_some("This edition has no network features.")
}

/// Backend set for a `--renderer` name. `None` (inside `Some`) means wgpu's
/// own default; the outer `None` is an unknown name.
fn renderer_backends(name: &str) -> Option<Option<eframe::wgpu::Backends>> {
    use eframe::wgpu::Backends;
    match name.to_ascii_lowercase().as_str() {
        "" | "auto" => Some(None),
        "vulkan" | "vk" => Some(Some(Backends::VULKAN)),
        "gl" | "opengl" | "gles" => Some(Some(Backends::GL)),
        _ => None,
    }
}

/// Minimal executor: wgpu's adapter listing is async, nothing else here is.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct Noop(std::thread::Thread);
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Noop(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
}

/// `Backend: adapter name` per adapter wgpu can see (device names, no paths).
fn adapter_names(backends: eframe::wgpu::Backends) -> Vec<String> {
    let inst = eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor {
        backends,
        ..eframe::wgpu::InstanceDescriptor::new_without_display_handle()
    });
    block_on(inst.enumerate_adapters(backends))
        .iter()
        .map(|a| {
            let i = a.get_info();
            format!("{:?} {}", i.backend, jizzcraft_core::report::clean(&i.name))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn no_core_dumps() {
    let lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscall with a valid pointer; failure is harmless.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &lim);
    }
}

#[cfg(not(target_os = "linux"))]
fn no_core_dumps() {}

/// A GUI-subsystem exe has no stdout. When started from a terminal, borrow its
/// console so the text options still print.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: plain Win32 call; failure (no parent console) is harmless.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}

fn main() -> eframe::Result {
    attach_console();
    // A crash leaves a redacted note; no core dump with library data in it.
    jizzcraft_core::report::install_crash_hook("jizzcraft", env!("CARGO_PKG_VERSION"));
    no_core_dumps();
    let cli = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n\n{HELP}");
            std::process::exit(2);
        }
    };
    if cli.help {
        print!("{HELP}");
        return Ok(());
    }
    if let Some(m) = send_refusal(&cli.sends) {
        eprintln!("{m}");
        std::process::exit(2);
    }
    if cli.doctor {
        jizzcraft_media::prepare_bundled_env();
        jizzcraft_media::doctor::set_renderer_probe(|| {
            adapter_names(eframe::wgpu::Backends::PRIMARY | eframe::wgpu::Backends::GL)
        });
        let r = jizzcraft_media::doctor::run();
        print!("{}", r.render_text());
        std::process::exit(i32::from(r.exit_code()));
    }
    let Cli {
        mut opts,
        assume_adult,
        bench_file,
        bench_secs,
        bench_gst,
        disguise: disguise_flag,
        magnets,
        sends,
        portable,
        home,
        help: _,
        doctor: _,
        renderer,
    } = cli;
    if portable && let Some(h) = jizzcraft_core::paths::portable_home() {
        jizzcraft_core::paths::set_home(h);
    }
    if let Some(d) = home {
        jizzcraft_core::paths::set_home(d);
    }
    // Flags win: the home is set once, so the environment only fills the gap.
    if let Some(h) = jizzcraft_core::paths::home_from_env() {
        jizzcraft_core::paths::set_home(h);
    }
    if let Some(name) = jizzcraft_core::paths::state_dir()
        .exists()
        .then(|| jizzcraft_core::report::take_unseen_crash(&jizzcraft_core::paths::state_dir()))
        .flatten()
    {
        // UI wiring comes later; the note is stderr only and says no names.
        eprintln!("jizzcraft: the last run ended in a crash ({name} in the state folder)");
    }
    // Packaged builds use their own GStreamer only. Still single-threaded here.
    jizzcraft_media::prepare_bundled_env();
    if let Some(file) = bench_file {
        if bench_gst {
            match jizzcraft_ui::bench::gst_play_baseline(&file, bench_secs) {
                Ok((cpu, rss)) => {
                    println!("GST-PLAY-1.0 BASELINE (same file, {bench_secs} s window)");
                    println!("  cpu             {cpu:.1} % of one core");
                    println!("  rss peak        {:.0} MB", rss.1);
                }
                Err(e) => eprintln!("gst-play baseline failed: {e}"),
            }
        }
        opts.bench = Some((file, bench_secs));
        opts.config = Some(Config {
            age_confirmed: true,
            ..Config::default()
        });
    }
    if opts.screenshot.is_some() {
        // Screenshots never touch the user's config. Say cheese, change nothing.
        opts.config = Some(Config {
            age_confirmed: assume_adult,
            inspector_open: true,
            ..Config::default()
        });
    }

    #[cfg(not(feature = "network"))]
    let _ = &sends;
    // Addresses: vetted, queued in the inbox; the running app (or this one,
    // once started) asks before playing. A refused address is never queued.
    #[cfg(feature = "network")]
    for u in &sends {
        if let Err(e) = jizzcraft_fetch::inbox::push_link(u) {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
    // Magnet links: hand them to the running app, or play them from this one.
    #[cfg(feature = "network")]
    let _instance_lock = {
        let lock = jizzcraft_fetch::inbox::try_primary_instance();
        if lock.is_none() && (!magnets.is_empty() || !sends.is_empty()) {
            for m in &magnets {
                let _ = jizzcraft_fetch::inbox::push(m);
            }
            return Ok(());
        }
        lock
    };
    #[cfg(feature = "network")]
    for m in &magnets {
        opts.commands
            .push(("fetch.play".to_owned(), serde_json::json!({ "magnet": m })));
    }
    #[cfg(not(feature = "network"))]
    if !magnets.is_empty() {
        eprintln!("magnet links need the network build: cargo build --features network");
        std::process::exit(2);
    }

    // The identity is fixed for the whole run (the app ID cannot change later).
    // No costume changes mid-show.
    let saved = opts
        .config
        .as_ref()
        .map(|c| c.disguise)
        .unwrap_or_else(|| Config::load().disguise);
    let identity = jizzcraft_ui::identity::launch_identity(saved, disguise_flag);
    opts.disguised = identity.disguised;
    set_taskbar_id(identity.app_id);
    // Flag wins over the environment; `auto` leaves wgpu's defaults alone.
    let renderer_name = renderer
        .or_else(|| std::env::var("JIZZCRAFT_RENDERER").ok())
        .unwrap_or_default();
    let backends = renderer_backends(&renderer_name).unwrap_or_else(|| {
        eprintln!("JIZZCRAFT_RENDERER: expected auto, vulkan or gl; using auto");
        None
    });
    let mut native = eframe::NativeOptions {
        viewport: egui_viewport(&identity),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    if let Some(b) = backends
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut native.wgpu_options.wgpu_setup
    {
        c.instance_descriptor.backends = b;
    }
    let res = eframe::run_native(
        identity.app_name,
        native,
        Box::new(move |cc| match Root::new(cc, opts) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => Err(e.into()),
        }),
    );
    match res {
        // The app itself refused (library cannot open, ...): its own message.
        Err(eframe::Error::AppCreation(e)) => Err(eframe::Error::AppCreation(e)),
        // No adapter or no window system: say it in plain words, exit 1.
        Err(e) => {
            // The error text can carry device names, never user paths.
            let msg = "JizzCraft could not start its graphics.\n\nInstall your GPU's Vulkan driver, or try `jizzcraft --renderer gl`.\nRun `jizzcraft --doctor` for details.";
            eprintln!("{msg}\n({e})");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("JizzCraft")
                .set_description(msg)
                .show();
            std::process::exit(1);
        }
        Ok(()) => Ok(()),
    }
}

/// The taskbar groups windows by this ID: give it the identity's, so a
/// disguised launch does not share a button (or a name) with the brand.
#[cfg(windows)]
fn set_taskbar_id(app_id: &str) {
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    let wide: Vec<u16> = app_id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    unsafe {
        SetCurrentProcessExplicitAppUserModelID(wide.as_ptr());
    }
}

#[cfg(not(windows))]
fn set_taskbar_id(_: &str) {}

fn egui_viewport(id: &jizzcraft_ui::identity::Identity) -> eframe::egui::ViewportBuilder {
    // Window and taskbar icon: the neutral one while disguised.
    let png: &[u8] = if id.disguised {
        include_bytes!("../../../assets/icons/viewer-256.png")
    } else {
        include_bytes!("../../../assets/icons/jizzcraft-512.png")
    };
    let mut vb = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([760.0, 480.0])
        .with_title(id.title)
        .with_app_id(id.app_id);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(png) {
        vb = vb.with_icon(icon);
    }
    vb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Cli, String> {
        parse_args(a.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn flags_map_to_options() {
        let c = p(&[
            "--import",
            "/m",
            "--tab",
            "videos",
            "--open",
            "3",
            "--lang",
            "fr",
            "--screenshot",
            "o.png",
            "--assume-adult",
            "--disguise",
            "--portable",
        ])
        .unwrap();
        assert_eq!(c.opts.import, Some(PathBuf::from("/m")));
        assert_eq!(c.opts.tab.as_deref(), Some("videos"));
        assert_eq!(c.opts.open_index, Some(3));
        assert_eq!(c.opts.lang.as_deref(), Some("fr"));
        assert_eq!(c.opts.screenshot, Some(PathBuf::from("o.png")));
        assert!(c.assume_adult && c.disguise && c.portable && !c.help);
        assert_eq!(c.bench_secs, 30);
    }

    #[test]
    fn cmd_takes_name_with_optional_json() {
        let c = p(&[
            "--cmd",
            "player.toggle",
            "--cmd",
            "view.sort={\"by\":\"name\"}",
        ])
        .unwrap();
        assert_eq!(c.opts.commands.len(), 2);
        assert_eq!(c.opts.commands[0].0, "player.toggle");
        assert_eq!(c.opts.commands[1].1["by"], "name");
        assert!(p(&["--cmd", "x={bad"]).is_err());
    }

    #[test]
    fn send_and_positional_urls() {
        let c = p(&["--send", "https://example.com/a", "HTTP://example.com/b"]).unwrap();
        assert_eq!(c.sends.len(), 2);
        assert!(p(&["--send"]).is_err());
        assert!(p(&["--nope"]).is_err());
        // Offline builds refuse; online builds do not.
        let refused = send_refusal(&c.sends).is_some();
        assert_eq!(refused, !cfg!(feature = "network"));
        assert!(send_refusal(&[]).is_none());
        if refused {
            assert_eq!(
                send_refusal(&c.sends),
                Some("This edition has no network features.")
            );
        }
    }

    #[test]
    fn magnets_and_unknown_flags() {
        let c = p(&[
            "magnet:?xt=urn:btih:abc",
            "--bench",
            "f.mp4",
            "--seconds",
            "x",
        ])
        .unwrap();
        assert_eq!(c.magnets.len(), 1);
        assert_eq!(c.bench_file, Some(PathBuf::from("f.mp4")));
        // A bad number falls back to the default instead of failing.
        // "x seconds" is 30 seconds. We have decided not to argue.
        assert_eq!(c.bench_secs, 30);
        assert!(p(&["--nope"]).err().is_some_and(|e| e.contains("--nope")));
        assert!(p(&["-h"]).is_ok_and(|c| c.help));
    }
}
