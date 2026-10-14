//! JizzCraft desktop app (eframe + wgpu).

use jizzcraft_ui::{JizzApp, Options, config::Config};
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
  --lang <CODE>             interface language: en fr es de it pt (default: system)
  --disguise                launch as a neutral \"Viewer\" (window name, app ID, discreet mode)
  --portable                keep database, config and cache in ./jizzcraft-data next to the executable
  --home <DIR>              keep database, config and cache under DIR
  -h, --help                show this help

ENV:
  JIZZCRAFT_DEBUG=1         log full paths (off by default)
  JIZZCRAFT_HOME=<DIR>      same as --home
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
    portable: bool,
    home: Option<PathBuf>,
    help: bool,
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

fn main() -> eframe::Result {
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
    let Cli {
        mut opts,
        assume_adult,
        bench_file,
        bench_secs,
        bench_gst,
        disguise: disguise_flag,
        magnets,
        portable,
        home,
        help: _,
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
        // Screenshots never touch the user's config.
        opts.config = Some(Config {
            age_confirmed: assume_adult,
            inspector_open: true,
            ..Config::default()
        });
    }

    // Magnet links: hand them to the running app, or play them from this one.
    #[cfg(feature = "network")]
    let _instance_lock = {
        let lock = jizzcraft_fetch::inbox::try_primary_instance();
        if lock.is_none() && !magnets.is_empty() {
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
    let saved = opts
        .config
        .as_ref()
        .map(|c| c.disguise)
        .unwrap_or_else(|| Config::load().disguise);
    let identity = jizzcraft_ui::identity::launch_identity(saved, disguise_flag);
    opts.disguised = identity.disguised;
    let native = eframe::NativeOptions {
        viewport: egui_viewport(&identity),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        identity.app_name,
        native,
        Box::new(move |cc| match JizzApp::new(cc, opts) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => Err(e.into()),
        }),
    )
}

fn egui_viewport(id: &jizzcraft_ui::identity::Identity) -> eframe::egui::ViewportBuilder {
    eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([760.0, 480.0])
        .with_title(id.title)
        .with_app_id(id.app_id)
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
        assert_eq!(c.bench_secs, 30);
        assert!(p(&["--nope"]).err().is_some_and(|e| e.contains("--nope")));
        assert!(p(&["-h"]).is_ok_and(|c| c.help));
    }
}
