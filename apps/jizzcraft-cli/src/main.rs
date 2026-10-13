//! `jizzcraft-cli`: same command registry as the desktop app, scriptable.

use clap::{Parser, Subcommand, ValueEnum};
use jizzcraft_core::{Db, Registry, paths, register_core};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "jizzcraft-cli",
    version,
    about = "JizzCraft library from the command line"
)]
struct Cli {
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Library database (default: XDG data dir, or $JIZZCRAFT_DB).
    #[arg(long, global = true, value_name = "FILE")]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum TypeArg {
    Image,
    Video,
}

#[derive(Subcommand)]
enum Cmd {
    /// Import a folder and index it recursively.
    Scan { dir: PathBuf },
    /// Rescan every imported folder.
    Rescan,
    /// List media.
    Query {
        /// Favorites only.
        #[arg(long)]
        fav: bool,
        /// Restrict to a media type.
        #[arg(long = "type", value_enum)]
        kind: Option<TypeArg>,
        /// Filename substring (case-insensitive).
        #[arg(long)]
        name: Option<String>,
        /// Include files flagged missing.
        #[arg(long)]
        include_missing: bool,
        #[arg(long)]
        limit: Option<u64>,
        /// Sort key: name, added, size or duration.
        #[arg(long, default_value = "name")]
        sort: String,
        /// Descending order.
        #[arg(long)]
        desc: bool,
    },
    /// Toggle the favorite flag of a media id.
    Fav { id: i64 },
    /// Library counters.
    Stats,
    /// List registered commands.
    Commands,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if cli.json {
                println!("{}", json!({ "error": e }));
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);

    if matches!(cli.cmd, Cmd::Commands) {
        let list: Vec<_> = reg.list().collect();
        if cli.json {
            let v: Vec<Value> = list
                .iter()
                .map(|(n, h)| json!({ "name": n, "help": h }))
                .collect();
            println!("{}", Value::Array(v));
        } else {
            for (n, h) in list {
                println!("{n:<24} {h}");
            }
        }
        return Ok(());
    }

    let db_path = cli.db.clone().unwrap_or_else(paths::db_path);
    let mut db = Db::open(&db_path).map_err(|e| e.to_string())?;

    let (name, args) = match &cli.cmd {
        Cmd::Scan { dir } => ("library.import", json!({ "path": dir.to_string_lossy() })),
        Cmd::Rescan => ("library.rescan", json!({})),
        Cmd::Query {
            fav,
            kind,
            name,
            include_missing,
            limit,
            sort,
            desc,
        } => {
            let mut a = json!({
                "fav": fav,
                "include_missing": include_missing,
                "sort": sort,
                "desc": desc,
            });
            if let Some(k) = kind {
                a["kind"] = json!(match k {
                    TypeArg::Image => "image",
                    TypeArg::Video => "video",
                });
            }
            if let Some(n) = name {
                a["name"] = json!(n);
            }
            if let Some(l) = limit {
                a["limit"] = json!(l);
            }
            ("media.query", a)
        }
        Cmd::Fav { id } => ("media.favorite.toggle", json!({ "id": id })),
        Cmd::Stats => ("library.stats", json!({})),
        Cmd::Commands => unreachable!(),
    };

    let out = reg.run(&mut db, name, args).map_err(|e| e.to_string())?;
    if cli.json {
        println!("{out}");
    } else {
        print_human(name, &out);
    }
    Ok(())
}

fn print_human(cmd: &str, v: &Value) {
    match cmd {
        "media.query" => {
            for m in v["items"].as_array().into_iter().flatten() {
                let flags = format!(
                    "{}{}",
                    if m["favorite"].as_bool() == Some(true) {
                        "*"
                    } else {
                        " "
                    },
                    if m["missing"].as_bool() == Some(true) {
                        "!"
                    } else {
                        " "
                    }
                );
                println!(
                    "{:>6} {:<5} {} {}",
                    m["id"],
                    m["kind"].as_str().unwrap_or("?"),
                    flags,
                    m["path"].as_str().unwrap_or("?")
                );
            }
            println!("{} item(s)", v["count"]);
        }
        "library.import" | "library.rescan" => {
            let r = &v["report"];
            println!(
                "added {} · updated {} · unchanged {} · missing {} · symlinks skipped {} · errors {}",
                r["added"],
                r["updated"],
                r["unchanged"],
                r["marked_missing"],
                r["skipped_symlinks"],
                r["errors"]
            );
        }
        "media.favorite.toggle" => {
            println!("#{} favorite: {}", v["id"], v["favorite"]);
        }
        "library.stats" => {
            for k in [
                "total",
                "images",
                "videos",
                "favorites",
                "missing",
                "folders",
                "total_bytes",
            ] {
                println!("{k:<12} {}", v[k]);
            }
        }
        _ => println!("{v}"),
    }
}
