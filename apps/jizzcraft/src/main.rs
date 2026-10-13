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
  -h, --help                show this help

ENV:
  JIZZCRAFT_KNOW=2          double the word \"know\" in the headline
  JIZZCRAFT_DEBUG=1         log full paths (off by default)
";

fn main() -> eframe::Result {
    let mut opts = Options {
        know: 1,
        ..Default::default()
    };
    if let Some(n) = std::env::var("JIZZCRAFT_KNOW")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        opts.know = n.clamp(1, 4);
    }
    let mut args = std::env::args().skip(1);
    let mut assume_adult = false;
    let mut bench_file: Option<PathBuf> = None;
    let mut bench_secs = 30u32;
    let mut bench_gst = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => opts.screenshot = args.next().map(PathBuf::from),
            "--import" => opts.import = args.next().map(PathBuf::from),
            "--db" => opts.db_path = args.next().map(PathBuf::from),
            "--tab" => opts.tab = args.next(),
            "--open" => opts.open_index = args.next().and_then(|v| v.parse().ok()),
            "--cmd" => {
                // name or name={"json":"args"}
                let raw = args.next().unwrap_or_default();
                let (name, json) = raw.split_once('=').unwrap_or((raw.as_str(), "{}"));
                match serde_json::from_str(json) {
                    Ok(v) => opts.commands.push((name.to_owned(), v)),
                    Err(e) => {
                        eprintln!("--cmd: bad JSON arguments: {e}");
                        std::process::exit(2);
                    }
                }
            }
            "--bench" => bench_file = args.next().map(PathBuf::from),
            "--seconds" => bench_secs = args.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--bench-gst-play" => bench_gst = true,
            "--assume-adult" => assume_adult = true,
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => {
                eprintln!("unknown option `{other}`\n\n{HELP}");
                std::process::exit(2);
            }
        }
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

    let native = eframe::NativeOptions {
        viewport: egui_viewport(),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "JizzCraft",
        native,
        Box::new(move |cc| match JizzApp::new(cc, opts) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => Err(e.into()),
        }),
    )
}

fn egui_viewport() -> eframe::egui::ViewportBuilder {
    eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([760.0, 480.0])
        .with_title("JizzCraft")
        .with_app_id("org.jizzcraft.JizzCraft")
}
