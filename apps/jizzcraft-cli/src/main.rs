//! `jizzcraft-cli`: same command registry as the desktop app, scriptable.
//!
//! Doc comments on the clap types below are `--help` text: keep jokes out of
//! them, the terminal may be on a shared screen.

use clap::{Parser, Subcommand, ValueEnum};
use jizzcraft_core::{Db, HasLibrary, Registry, paths, register_core};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;

/// Command context: the library, plus the network host in the opt-in build.
// No mouse, no thumbnails, no judgement: the CLI only ever sees ids.
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

/// `--version` text: the edition is stated next to the number.
const VERSION: &str = if cfg!(feature = "network") {
    concat!(env!("CARGO_PKG_VERSION"), " (online edition)")
} else {
    concat!(env!("CARGO_PKG_VERSION"), " (offline edition)")
};

#[derive(Parser)]
#[command(
    name = "jizzcraft-cli",
    version = VERSION,
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

#[derive(Subcommand)]
enum GenWhat {
    /// Print completions for a shell.
    Completions { shell: clap_complete::Shell },
    /// Print the man page.
    Man,
}

/// `doctor --report`: the redacted support report, from an allow-list.
fn support_report(db_path: &std::path::Path) -> String {
    use jizzcraft_core::report::Input;
    let r = jizzcraft_media::doctor::run();
    let stats = Db::open(db_path)
        .ok()
        .map(|db| (db.schema_version().ok(), db.stats().ok()));
    let input = Input {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        family: Some(r.family),
        kind: Some(r.kind),
        gst_version: jizzcraft_media::doctor::gst_version(),
        plugins: r
            .checks
            .iter()
            .map(|c| (format!("{}.{}", c.group, c.element), c.ok))
            .collect(),
        schema_version: stats.as_ref().and_then(|s| s.0),
        items: stats.as_ref().and_then(|s| s.1.as_ref()).map(|s| s.total),
        folders: stats.as_ref().and_then(|s| s.1.as_ref()).map(|s| s.folders),
        config: std::fs::read(paths::config_dir().join("config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok()),
    };
    format!(
        "edition: {}\n{}",
        jizzcraft_ui::edition::label(),
        jizzcraft_core::report::render(&input)
    )
}

/// First doctor lines: `Edition: ...` and, online only, `Browser: ...`.
fn edition_header() -> String {
    use jizzcraft_ui::edition::{self, Edition};
    let mut h = format!("Edition: {}\n", edition::label());
    if edition::EDITION == Edition::Online {
        h.push_str(&format!("Browser: {}\n", edition::browser_status().word()));
    }
    h
}

/// `doctor`: human report or JSON, exit 0 fine, 1 required missing, 2 codecs.
fn doctor(cli: &Cli, report: bool) -> ExitCode {
    let json = cli.json;
    if cli.portable
        && let Some(h) = paths::portable_home()
    {
        paths::set_home(h);
    }
    if let Some(h) = cli.home.clone().or_else(paths::home_from_env) {
        paths::set_home(h);
    }
    jizzcraft_media::prepare_bundled_env();
    if report {
        let db = cli.db.clone().unwrap_or_else(paths::db_path);
        print!("{}", support_report(&db));
        return ExitCode::SUCCESS;
    }
    let r = jizzcraft_media::doctor::run();
    let fix = r.fix();
    if json {
        let checks: Vec<Value> = r
            .checks
            .iter()
            .map(|c| json!({ "id": c.id, "group": c.group, "label": c.label, "ok": c.ok, "element": c.element }))
            .collect();
        let dirs: Vec<Value> = r
            .dirs_ok
            .iter()
            .map(|(n, ok)| json!({ "dir": n, "writable": ok }))
            .collect();
        let notes: Vec<Value> = r
            .notes
            .iter()
            .map(|n| json!({ "id": n.id, "warn": n.warn, "text": n.text }))
            .collect();
        let st = jizzcraft_ui::edition::browser_status();
        let mut v = json!({ "edition": jizzcraft_ui::edition::label() });
        if jizzcraft_ui::edition::EDITION == jizzcraft_ui::edition::Edition::Online {
            v["browser"] = json!(st.word());
        }
        v["checks"] = json!(checks);
        v["dirs"] = json!(dirs);
        v["notes"] = json!(notes);
        v["fix"] = json!(fix);
        v["exit"] = json!(r.exit_code());
        println!("{v}");
    } else {
        print!("{}{}", edition_header(), r.render_text());
    }
    ExitCode::from(r.exit_code())
}

#[derive(Clone, Copy, ValueEnum)]
enum TypeArg {
    Image,
    Video,
}

#[derive(Clone, Copy, ValueEnum)]
enum PlaylistSortArg {
    Name,
    Added,
    Duration,
    Random,
}

/// `playlist ...`: ordered, hand-made lists. Database only.
#[derive(Subcommand)]
enum PlaylistCmd {
    /// List playlists with ids and item counts.
    List,
    /// Create a playlist.
    Create { name: String },
    /// Rename a playlist.
    Rename { id: i64, name: String },
    /// Delete a playlist (media files and library entries are untouched).
    Delete { id: i64 },
    /// Append items by media id.
    Add {
        id: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
    },
    /// Remove items by media id.
    Remove {
        id: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
    },
    /// Move items; before the given media id, or to the end.
    Move {
        id: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
        /// Media id (already in the playlist) to insert before.
        #[arg(long)]
        before: Option<i64>,
    },
    /// List the items in order.
    Items {
        id: i64,
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        offset: Option<u64>,
    },
    /// Reorder the playlist by a key.
    Sort {
        id: i64,
        #[arg(long, value_enum)]
        by: PlaylistSortArg,
        #[arg(long)]
        desc: bool,
    },
    /// Write an .m3u8 file. Refuses to overwrite an existing destination.
    ExportM3u {
        id: i64,
        dest: PathBuf,
        /// Paths relative to the destination folder.
        #[arg(long)]
        relative: bool,
        /// Write the item names as titles (off by default).
        #[arg(long)]
        include_names: bool,
    },
}

/// `tag ...`: flat labels.
#[derive(Subcommand)]
enum TagCmd {
    /// List tags with ids and counts.
    List,
    /// Create a tag.
    Create { name: String },
    /// Rename a tag.
    Rename { id: i64, name: String },
    /// Delete a tag (media untouched).
    Delete { id: i64 },
    /// Merge a tag into another one.
    Merge { from: i64, into: i64 },
    /// Add a tag to items (--off removes it).
    Assign {
        tag: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
        #[arg(long)]
        off: bool,
    },
    /// Toggle a tag on items.
    Toggle {
        tag: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
    },
}

/// `category ...`: a tree (depth 4); a filter includes the whole subtree.
#[derive(Subcommand)]
enum CategoryCmd {
    /// List categories in tree order with ids and counts.
    List,
    /// Create a category.
    Create {
        name: String,
        #[arg(long)]
        parent: Option<i64>,
    },
    /// Rename a category.
    Rename { id: i64, name: String },
    /// Re-parent a category (no --parent = top level).
    Move {
        id: i64,
        #[arg(long)]
        parent: Option<i64>,
    },
    /// Delete a category (children move up, media untouched).
    Delete { id: i64 },
    /// Add a category to items (--off removes it).
    Assign {
        category: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Playlists: ordered lists of library items.
    Playlist {
        #[command(subcommand)]
        action: PlaylistCmd,
    },
    /// Tags: flat labels.
    Tag {
        #[command(subcommand)]
        action: TagCmd,
    },
    /// Categories: a tree of labels.
    Category {
        #[command(subcommand)]
        action: CategoryCmd,
    },
    /// Check that this machine can play things, and say how to fix it.
    Doctor {
        /// Print a redacted support report (versions, counts, flags; no paths,
        /// names, URLs or keys) instead of the check list.
        #[arg(long)]
        report: bool,
    },
    /// Generate shell completions or the man page (for packagers).
    #[command(hide = true)]
    Gen {
        #[command(subcommand)]
        what: GenWhat,
    },
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
        /// Sort key: name, added, size, duration, random, or position (needs --playlist).
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
        /// Only items in this playlist id.
        #[arg(long)]
        playlist: Option<i64>,
        /// Must have this tag id (repeat: all of them).
        #[arg(long = "tag", value_name = "ID")]
        tags: Vec<i64>,
        /// Has at least one of these tag ids (repeat for more).
        #[arg(long = "any-tag", value_name = "ID")]
        any_tags: Vec<i64>,
        /// Must not have this tag id (repeat for more).
        #[arg(long = "not-tag", value_name = "ID")]
        not_tags: Vec<i64>,
        /// In this category id or any category below it (repeat: all of them).
        #[arg(long = "category", value_name = "ID")]
        categories: Vec<i64>,
        /// Only items without any tag.
        #[arg(long)]
        untagged: bool,
        /// Only items without any category.
        #[arg(long)]
        uncategorized: bool,
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
        /// Also export playlists, tags and categories flagged private.
        #[arg(long)]
        private: bool,
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

fn main() -> ExitCode {
    // Crash note without the panic message; no core dumps with library data.
    jizzcraft_core::report::install_crash_hook("jizzcraft-cli", env!("CARGO_PKG_VERSION"));
    no_core_dumps();
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Doctor { report } => return doctor(&cli, *report),
        Cmd::Gen { what } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            match what {
                GenWhat::Completions { shell } => {
                    clap_complete::generate(
                        *shell,
                        &mut cmd,
                        "jizzcraft-cli",
                        &mut std::io::stdout(),
                    );
                }
                GenWhat::Man => {
                    let _ = clap_mangen::Man::new(cmd).render(&mut std::io::stdout());
                }
            }
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
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
    jizzcraft_media::prepare_bundled_env();

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
        // Handled in `main` before the registry exists.
        Cmd::Doctor { .. } | Cmd::Gen { .. } => return Ok(()),
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
            playlist,
            tags,
            any_tags,
            not_tags,
            categories,
            untagged,
            uncategorized,
        } => {
            if sort == "position" && playlist.is_none() {
                return Err("--sort position needs --playlist".to_owned());
            }
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
            if let Some(p) = playlist {
                a["playlist"] = json!(p);
            }
            // Categories filter as labels: the engine includes each subtree.
            let all: Vec<i64> = tags.iter().chain(categories).copied().collect();
            for (k, v) in [
                ("labels_all", &all),
                ("labels_any", any_tags),
                ("labels_none", not_tags),
            ] {
                if !v.is_empty() {
                    a[k] = json!(v);
                }
            }
            if *untagged {
                a["untagged"] = json!(true);
            }
            if *uncategorized {
                a["uncategorized"] = json!(true);
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
            private,
        } => (
            "meta.export".to_owned(),
            json!({
                "dest": dest.to_string_lossy(),
                "include_history": history,
                "include_names": names,
                "include_private": private,
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
        Cmd::Playlist { action } => playlist_call(action),
        Cmd::Tag { action } => tag_call(action),
        Cmd::Category { action } => category_call(action),
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

fn playlist_call(a: &PlaylistCmd) -> (String, Value) {
    let (n, v) = match a {
        PlaylistCmd::List => ("playlist.list", json!({})),
        PlaylistCmd::Create { name } => ("playlist.create", json!({ "name": name })),
        PlaylistCmd::Rename { id, name } => ("playlist.rename", json!({ "id": id, "name": name })),
        PlaylistCmd::Delete { id } => ("playlist.delete", json!({ "id": id })),
        PlaylistCmd::Add { id, media_ids } => {
            ("playlist.add", json!({ "id": id, "media_ids": media_ids }))
        }
        PlaylistCmd::Remove { id, media_ids } => (
            "playlist.remove",
            json!({ "id": id, "media_ids": media_ids }),
        ),
        PlaylistCmd::Move {
            id,
            media_ids,
            before,
        } => (
            "playlist.move",
            json!({ "id": id, "media_ids": media_ids, "before": before }),
        ),
        PlaylistCmd::Items { id, limit, offset } => {
            let mut a = json!({ "id": id });
            if let Some(l) = limit {
                a["limit"] = json!(l);
            }
            if let Some(o) = offset {
                a["offset"] = json!(o);
            }
            ("playlist.items", a)
        }
        PlaylistCmd::Sort { id, by, desc } => (
            "playlist.sort",
            json!({
                "id": id,
                "by": match by {
                    PlaylistSortArg::Name => "name",
                    PlaylistSortArg::Added => "added",
                    PlaylistSortArg::Duration => "duration",
                    PlaylistSortArg::Random => "random",
                },
                "desc": desc,
            }),
        ),
        PlaylistCmd::ExportM3u {
            id,
            dest,
            relative,
            include_names,
        } => (
            "playlist.export_m3u",
            json!({
                "id": id,
                "dest": dest.to_string_lossy(),
                "relative": relative,
                "include_names": include_names,
            }),
        ),
    };
    (n.to_owned(), v)
}

fn tag_call(a: &TagCmd) -> (String, Value) {
    let (n, v) = match a {
        TagCmd::List => ("tag.list", json!({})),
        TagCmd::Create { name } => ("tag.create", json!({ "name": name })),
        TagCmd::Rename { id, name } => ("tag.rename", json!({ "id": id, "name": name })),
        TagCmd::Delete { id } => ("tag.delete", json!({ "id": id })),
        TagCmd::Merge { from, into } => ("tag.merge", json!({ "from": from, "into": into })),
        TagCmd::Assign {
            tag,
            media_ids,
            off,
        } => (
            "tag.assign",
            json!({ "ids": [tag], "media_ids": media_ids, "on": !off }),
        ),
        TagCmd::Toggle { tag, media_ids } => {
            ("tag.toggle", json!({ "id": tag, "media_ids": media_ids }))
        }
    };
    (n.to_owned(), v)
}

fn category_call(a: &CategoryCmd) -> (String, Value) {
    let (n, v) = match a {
        CategoryCmd::List => ("category.list", json!({})),
        CategoryCmd::Create { name, parent } => {
            let mut a = json!({ "name": name });
            if let Some(p) = parent {
                a["parent"] = json!(p);
            }
            ("category.create", a)
        }
        CategoryCmd::Rename { id, name } => ("category.rename", json!({ "id": id, "name": name })),
        CategoryCmd::Move { id, parent } => {
            ("category.move", json!({ "id": id, "parent": parent }))
        }
        CategoryCmd::Delete { id } => ("category.delete", json!({ "id": id })),
        CategoryCmd::Assign {
            category,
            media_ids,
            off,
        } => (
            "category.assign",
            json!({ "ids": [category], "media_ids": media_ids, "on": !off }),
        ),
    };
    (n.to_owned(), v)
}

/// `#1  3 item(s)  name` style table for playlists, tags and categories.
fn print_labels(items: &Value, nested: bool) {
    let rows: Vec<&Value> = items.as_array().into_iter().flatten().collect();
    println!("{:>6}  {:>6}  name", "id", "items");
    for r in &rows {
        let count = if nested { &r["total"] } else { &r["count"] };
        let indent = "  ".repeat(r["depth"].as_u64().unwrap_or(0) as usize);
        println!(
            "{:>6}  {:>6}  {indent}{}",
            r["id"],
            count,
            r["name"].as_str().unwrap_or("?")
        );
    }
    println!("{} total", rows.len());
}

fn print_human(cmd: &str, v: &Value) {
    match cmd {
        "playlist.list" | "tag.list" => print_labels(&v["items"], false),
        "category.list" => print_labels(&v["items"], true),
        "playlist.items" => {
            for (i, m) in v["items"].as_array().into_iter().flatten().enumerate() {
                println!(
                    "{:>4} {:>6} {:<5} {}",
                    i as u64 + 1,
                    m["id"],
                    m["kind"].as_str().unwrap_or("?"),
                    m["path"].as_str().unwrap_or("?")
                );
            }
            println!("{} of {} item(s)", v["count"], v["total"]);
        }
        "playlist.create" | "tag.create" | "category.create" => println!("created #{}", v["id"]),
        "playlist.add" => println!("added {} · skipped {}", v["added"], v["skipped"]),
        "playlist.remove" => println!("removed {}", v["removed"]),
        "playlist.move" => println!("moved {}", v["moved"]),
        "tag.assign" | "category.assign" => println!("changed {}", v["changed"]),
        "tag.toggle" | "category.toggle" => {
            println!(
                "{} on {} item(s)",
                if v["on"] == true { "on" } else { "off" },
                v["changed"]
            );
        }
        "playlist.delete" | "tag.delete" | "category.delete" => {
            println!("deleted ({} item link(s) removed)", v["items"]);
        }
        "tag.merge" | "category.merge" => println!("moved {}", v["moved"]),
        "playlist.export_m3u" => println!(
            "written {} · missing skipped {} · unsafe skipped {}",
            v["written"], v["skipped_missing"], v["skipped_unsafe"]
        ),
        "labels.of" => {
            for (k, t) in [("tags", "tag"), ("categories", "category")] {
                for l in v[k].as_array().into_iter().flatten() {
                    println!(
                        "{t:<9} {:>6}  {}",
                        l["id"],
                        l["name"].as_str().unwrap_or("?")
                    );
                }
            }
        }
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
