//! `jizzcraft-cli`: same command registry as the desktop app, scriptable.

use clap::{Parser, Subcommand, ValueEnum};
use jizzcraft_core::{Db, HasLibrary, Registry, paths, register_core};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;

/// Command context: the library, plus the network host in the opt-in build.
struct CliCtx {
    db: Db,
    #[cfg(feature = "network")]
    fetch: jizzcraft_fetch::commands::FetchHost,
}

impl HasLibrary for CliCtx {
    fn db(&mut self) -> &mut Db {
        &mut self.db
    }
}

#[cfg(feature = "network")]
impl jizzcraft_fetch::commands::HasFetch for CliCtx {
    fn fetch(&mut self) -> &mut jizzcraft_fetch::commands::FetchHost {
        &mut self.fetch
    }
}

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
    /// Keep database, config and cache in ./jizzcraft-data next to the executable.
    #[arg(long, global = true)]
    portable: bool,
    /// Keep database, config and cache under this directory (or $JIZZCRAFT_HOME).
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
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
        /// Seed for `--sort random` (stable shuffle).
        #[arg(long)]
        seed: Option<u32>,
        /// Minimum star rating (1-5).
        #[arg(long)]
        min_rating: Option<u8>,
        /// Only unrated items.
        #[arg(long)]
        unrated: bool,
        /// Only items in this directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Include sub-directories with --dir.
        #[arg(long)]
        recursive: bool,
        /// Hidden items: only | include (default: excluded).
        #[arg(long)]
        hidden: Option<String>,
        /// Only items still in the inbox.
        #[arg(long)]
        inbox: bool,
        /// Only items flagged damaged by the integrity check.
        #[arg(long)]
        damaged: bool,
        /// Start from a built-in or saved view (e.g. inbox, top-rated).
        #[arg(long)]
        view: Option<String>,
    },
    /// Set the star rating of an item.
    Rate { id: i64, rating: u8 },
    /// Hide an item in the library (database only; --show to undo).
    Hide {
        id: i64,
        #[arg(long)]
        show: bool,
    },
    /// Watch history: list, clear, pause or resume recording.
    History {
        #[arg(long)]
        clear: bool,
        #[arg(long)]
        pause: bool,
        #[arg(long)]
        resume: bool,
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// List, add or remove video markers.
    Markers {
        id: i64,
        /// Add a marker at this position (milliseconds).
        #[arg(long)]
        add: Option<u64>,
        #[arg(long)]
        label: Option<String>,
        /// Remove the marker with this id.
        #[arg(long)]
        remove: Option<i64>,
    },
    /// List views, or save/delete one.
    Views {
        #[arg(long)]
        save: Option<String>,
        /// JSON filter for --save.
        #[arg(long)]
        spec: Option<String>,
        #[arg(long)]
        delete: Option<String>,
    },
    /// Folder tree with counts.
    Tree,
    /// Pick a random item.
    Random,
    /// Fingerprint files so moved files are recognised.
    Fingerprint {
        #[arg(long)]
        all: bool,
    },
    /// Merge a vanished entry into its new location.
    Relink { from: i64, to: i64 },
    /// Decode-test files and flag damaged ones.
    Check {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        checksum: bool,
        /// Re-check everything, not only unchecked items.
        #[arg(long)]
        all: bool,
    },
    /// Back up the library database to a new file.
    Backup { dest: PathBuf },
    /// Export favorites, ratings and markers keyed by fingerprint.
    Export {
        dest: PathBuf,
        #[arg(long)]
        history: bool,
        #[arg(long)]
        names: bool,
    },
    /// Import an export onto matching files.
    Import {
        src: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Run any registered command: `run media.rate '{"id":1,"rating":4}'`.
    Run { name: String, args: Option<String> },
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
    let mut reg: Registry<CliCtx> = Registry::new();
    register_core(&mut reg);
    jizzcraft_media::integrity::register_media(&mut reg);
    #[cfg(feature = "network")]
    jizzcraft_fetch::commands::register_fetch(&mut reg);
    if cli.portable
        && let Some(h) = paths::portable_home()
    {
        paths::set_home(h);
    }
    if let Some(h) = cli.home.clone().or_else(paths::home_from_env) {
        paths::set_home(h);
    }

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
    let mut db = CliCtx {
        db: Db::open(&db_path).map_err(|e| e.to_string())?,
        #[cfg(feature = "network")]
        fetch: jizzcraft_fetch::commands::FetchHost::new(
            jizzcraft_fetch::FetchConfig::default_path(),
        ),
    };

    let (name, args): (String, Value) = match &cli.cmd {
        Cmd::Scan { dir } => (
            "library.import".to_owned(),
            json!({ "path": dir.to_string_lossy() }),
        ),
        Cmd::Rescan => ("library.rescan".to_owned(), json!({})),
        Cmd::Query {
            fav,
            kind,
            name,
            include_missing,
            limit,
            sort,
            desc,
            seed,
            min_rating,
            unrated,
            dir,
            recursive,
            hidden,
            inbox,
            damaged,
            view,
        } => {
            let mut a = json!({});
            if let Some(v) = view {
                let spec = reg
                    .run(&mut db, "views.get", json!({ "name": v }))
                    .map_err(|e| e.to_string())?;
                a = spec["spec"].clone();
            }
            let set = |a: &mut Value, k: &str, v: Value| a[k] = v;
            if *fav {
                set(&mut a, "fav", json!(true));
            }
            if *include_missing {
                set(&mut a, "include_missing", json!(true));
            }
            if sort != "name" || a.get("sort").is_none() {
                set(&mut a, "sort", json!(sort));
            }
            if *desc {
                set(&mut a, "desc", json!(true));
            }
            if let Some(s) = seed {
                set(&mut a, "seed", json!(s));
            }
            if let Some(r) = min_rating {
                set(&mut a, "min_rating", json!(r));
            }
            if *unrated {
                set(&mut a, "unrated", json!(true));
            }
            if let Some(d) = dir {
                set(&mut a, "dir", json!(d.to_string_lossy()));
            }
            if *recursive {
                set(&mut a, "recursive", json!(true));
            }
            if let Some(h) = hidden {
                set(&mut a, "hidden", json!(h));
            }
            if *inbox {
                set(&mut a, "inbox", json!(true));
            }
            if *damaged {
                set(&mut a, "damaged", json!(true));
            }
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
            ("media.query".to_owned(), a)
        }
        Cmd::Fav { id } => ("media.favorite.toggle".to_owned(), json!({ "id": id })),
        Cmd::Stats => ("library.stats".to_owned(), json!({})),
        Cmd::Rate { id, rating } => (
            "media.rate".to_owned(),
            json!({ "id": id, "rating": rating }),
        ),
        Cmd::Hide { id, show } => (
            "media.hide".to_owned(),
            json!({ "id": id, "hidden": !show }),
        ),
        Cmd::History {
            clear,
            pause,
            resume,
            limit,
        } => {
            if *clear {
                ("history.clear".to_owned(), json!({}))
            } else if *pause {
                ("history.pause".to_owned(), json!({ "paused": true }))
            } else if *resume {
                ("history.pause".to_owned(), json!({ "paused": false }))
            } else {
                ("history.list".to_owned(), json!({ "limit": limit }))
            }
        }
        Cmd::Markers {
            id,
            add,
            label,
            remove,
        } => {
            if let Some(m) = remove {
                ("markers.remove".to_owned(), json!({ "marker_id": m }))
            } else if let Some(pos) = add {
                (
                    "markers.add".to_owned(),
                    json!({ "id": id, "pos_ms": pos, "label": label.clone().unwrap_or_default() }),
                )
            } else {
                ("markers.list".to_owned(), json!({ "id": id }))
            }
        }
        Cmd::Views { save, spec, delete } => {
            if let Some(name) = save {
                let spec: Value = match spec {
                    Some(s) => serde_json::from_str(s).map_err(|e| format!("--spec: {e}"))?,
                    None => json!({}),
                };
                (
                    "views.save".to_owned(),
                    json!({ "name": name, "spec": spec }),
                )
            } else if let Some(name) = delete {
                ("views.delete".to_owned(), json!({ "name": name }))
            } else {
                ("views.list".to_owned(), json!({}))
            }
        }
        Cmd::Tree => ("library.tree".to_owned(), json!({})),
        Cmd::Random => ("media.random".to_owned(), json!({})),
        Cmd::Fingerprint { all } => ("library.fingerprint".to_owned(), json!({ "all": all })),
        Cmd::Relink { from, to } => ("media.relink".to_owned(), json!({ "from": from, "to": to })),
        Cmd::Check {
            full,
            checksum,
            all,
        } => (
            "library.integrity".to_owned(),
            json!({
                "mode": if *full { "full" } else { "quick" },
                "checksum": checksum,
                "scope": if *all { "all" } else { "unchecked" },
            }),
        ),
        Cmd::Backup { dest } => (
            "library.backup".to_owned(),
            json!({ "dest": dest.to_string_lossy() }),
        ),
        Cmd::Export {
            dest,
            history,
            names,
        } => (
            "meta.export".to_owned(),
            json!({
                "dest": dest.to_string_lossy(),
                "include_history": history,
                "include_names": names,
            }),
        ),
        Cmd::Import { src, dry_run } => (
            "meta.import".to_owned(),
            json!({ "src": src.to_string_lossy(), "dry_run": dry_run }),
        ),
        Cmd::Run { name, args } => {
            let a: Value = match args {
                Some(s) => serde_json::from_str(s).map_err(|e| format!("args: {e}"))?,
                None => json!({}),
            };
            (name.clone(), a)
        }
        Cmd::Commands => unreachable!(),
    };

    let out = reg.run(&mut db, &name, args).map_err(|e| e.to_string())?;
    if cli.json {
        println!("{out}");
    } else {
        print_human(&name, &out);
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
