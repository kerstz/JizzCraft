//! `jizzcraft-cli`: same command registry as the desktop app, scriptable.
//!
//! Doc comments on the clap types below are `--help` text: keep jokes out of
//! them, the terminal may be on a shared screen.

mod vault_cli;

use clap::{Parser, Subcommand, ValueEnum};
use jizzcraft_core::{Db, HasLibrary, Registry, paths, register_core};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use vault_cli::{Fail, Sources};

/// Command context: the library, plus the network host in the opt-in build.
// No mouse, no thumbnails, no judgement: the CLI only ever sees ids.
struct CliCtx {
    db: Db,
    /// Set when the library is open in vault mode.
    thumb_keys: Option<Arc<jizzcraft_core::vault::ThumbKeys>>,
    #[cfg(feature = "network")]
    fetch: jizzcraft_fetch::commands::FetchHost,
}

impl HasLibrary for CliCtx {
    fn db(&mut self) -> &mut Db {
        &mut self.db
    }

    fn thumb_keys(&self) -> Option<Arc<jizzcraft_core::vault::ThumbKeys>> {
        self.thumb_keys.clone()
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
    /// Read the vault passphrase from this file descriptor (one trailing
    /// newline is removed). Never an argument or an environment variable.
    #[arg(long, global = true, value_name = "N")]
    passphrase_fd: Option<i32>,
    /// Read the vault passphrase from standard input (one trailing newline
    /// is removed).
    #[arg(long, global = true)]
    passphrase_stdin: bool,
    /// New passphrase for `vault enable` and `vault change-passphrase`, from
    /// this file descriptor.
    #[arg(long, global = true, value_name = "N")]
    new_passphrase_fd: Option<i32>,
    #[command(subcommand)]
    cmd: Cmd,
}

impl Cli {
    fn sources(&self) -> Sources {
        Sources {
            passphrase_fd: self.passphrase_fd,
            passphrase_stdin: self.passphrase_stdin,
            new_passphrase_fd: self.new_passphrase_fd,
        }
    }
}

#[derive(Subcommand)]
enum GenWhat {
    /// Print completions for a shell.
    Completions { shell: clap_complete::Shell },
    /// Print the man page.
    Man,
}

/// `doctor --report`: the redacted support report, from an allow-list.
///
/// An encrypted library that is not opened with `--passphrase-fd` or
/// `--passphrase-stdin` is reported as `vault: locked`, without statistics.
fn support_report(db_path: &std::path::Path, src: &Sources) -> String {
    use jizzcraft_core::db::{FileFormat, probe_format};
    use jizzcraft_core::report::{Input, VaultState};
    let r = jizzcraft_media::doctor::run();
    let encrypted = probe_format(db_path).ok() == Some(FileFormat::Encrypted);
    let opened = if encrypted && !src.explicit() {
        None
    } else {
        vault_cli::open_library(db_path, src).ok()
    };
    let vault = match (&opened, encrypted) {
        (None, true) => Some(VaultState::Locked),
        (Some(o), _) => Some(if o.db.is_encrypted() {
            VaultState::On
        } else {
            VaultState::Off
        }),
        (None, false) => None,
    };
    let stats = opened
        .as_ref()
        .map(|o| (o.db.schema_version().ok(), o.db.stats().ok()));
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
        vault,
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
        print!("{}", support_report(&db, &cli.sources()));
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

/// `person ...`: performers (name, disambiguation, aliases, favorite, private).
#[derive(Subcommand)]
enum PersonCmd {
    /// List performers (optionally matching a name or alias).
    List {
        query: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u64,
    },
    /// Create a performer.
    Create {
        name: String,
        #[arg(long)]
        disambiguation: Option<String>,
    },
    /// Rename a performer.
    Rename { id: i64, name: String },
    /// Set flags: favorite, private.
    Set {
        id: i64,
        #[arg(long)]
        favorite: Option<bool>,
        #[arg(long)]
        private: Option<bool>,
        #[arg(long)]
        disambiguation: Option<String>,
    },
    /// Delete a performer (media untouched).
    Delete { id: i64 },
    /// Merge a performer into another one (names become aliases).
    Merge { from: i64, into: i64 },
    /// Add an alias.
    AliasAdd { id: i64, alias: String },
    /// Remove an alias.
    AliasRemove { id: i64, alias: String },
    /// Add a performer to items (--off removes it).
    Assign {
        person: i64,
        #[arg(required = true)]
        media_ids: Vec<i64>,
        #[arg(long)]
        off: bool,
    },
}

/// `studio ...`: one studio per item.
#[derive(Subcommand)]
enum StudioCmd {
    /// List studios with item counts.
    List,
    /// Create a studio.
    Create { name: String },
    /// Rename a studio.
    Rename { id: i64, name: String },
    /// Set the private flag.
    Set {
        id: i64,
        #[arg(long)]
        private: Option<bool>,
    },
    /// Delete a studio (items only lose the studio).
    Delete { id: i64 },
    /// Merge a studio into another one.
    Merge { from: i64, into: i64 },
    /// Set the studio of items (--clear removes it).
    Assign {
        #[arg(long, conflicts_with = "clear", required_unless_present = "clear")]
        studio: Option<i64>,
        #[arg(long)]
        clear: bool,
        #[arg(required = true)]
        media_ids: Vec<i64>,
    },
}

/// `tag ...`: flat labels.
#[derive(Subcommand)]
enum TagCmd {
    /// Add an alias to a tag (resolved on import and found by search).
    AliasAdd { id: i64, alias: String },
    /// Remove a tag alias.
    AliasRemove { id: i64, alias: String },
    /// List the aliases of a tag.
    Aliases { id: i64 },
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

/// Arguments of `query` (boxed in `Cmd`: this variant is far larger than the rest).
#[derive(clap::Args)]
struct QueryArgs {
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
    /// Only items of this studio id.
    #[arg(long)]
    studio: Option<i64>,
    /// Features this performer id (repeat: all of them).
    #[arg(long = "person", value_name = "ID")]
    people: Vec<i64>,
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
    /// Performers.
    Person {
        #[command(subcommand)]
        action: PersonCmd,
    },
    /// Studios.
    Studio {
        #[command(subcommand)]
        action: StudioCmd,
    },
    /// Show or edit the title, release date, studio and performers of an item.
    Info {
        id: i64,
        #[arg(long, conflicts_with = "clear_title")]
        title: Option<String>,
        #[arg(long)]
        clear_title: bool,
        /// YYYY-MM-DD.
        #[arg(long, conflicts_with = "clear_released")]
        released: Option<String>,
        #[arg(long)]
        clear_released: bool,
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
    /// Full-text search over file names, tags, categories and marker notes
    /// (accent and case insensitive, every word matches as a prefix).
    Search {
        /// Words to look for, all required.
        #[arg(required = true)]
        words: Vec<String>,
        /// Most results to print.
        #[arg(long, default_value_t = 100)]
        limit: u64,
        /// Results to skip.
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// List media.
    Query(Box<QueryArgs>),
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
    /// Read Kodi-style .nfo files onto matching items. A dry run unless --apply.
    ImportNfo {
        /// Folder to read (default: every library folder).
        dir: Option<PathBuf>,
        /// Write the changes (without it, only count them).
        #[arg(long)]
        apply: bool,
        /// Replace existing titles, dates, studios and ratings.
        #[arg(long)]
        overwrite: bool,
        /// Import star ratings.
        #[arg(long)]
        ratings: bool,
        /// Import play counts, last played dates and resume points.
        #[arg(long)]
        history: bool,
    },
    /// Find items by content hash (give one of --oshash, --fp).
    Lookup {
        /// 16 hex digits.
        #[arg(long, conflicts_with = "fp")]
        oshash: Option<String>,
        /// fp1:<32 hex>.
        #[arg(long)]
        fp: Option<String>,
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
    /// Encrypted vault: status, enable, disable, change-passphrase, verify.
    Vault {
        #[command(subcommand)]
        action: vault_cli::VaultCmd,
    },
    /// Build the thumbnail cache for every image (encrypted in vault mode).
    Thumbs,
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
                println!("{}", json!({ "error": e.msg, "code": e.code }));
            } else {
                eprintln!("error: {}", e.msg);
            }
            ExitCode::from(e.code)
        }
    }
}

fn run(cli: &Cli) -> Result<(), Fail> {
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
    // Private (0700) from the first creation: the home and cache directories
    // hold the library and the thumbnails.
    let _ = jizzcraft_core::fsutil::create_private_dir_all(&paths::cache_dir());
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
    let sources = cli.sources();
    if let Cmd::Vault { action } = &cli.cmd {
        return vault_cli::run(action, &db_path, &sources, cli.json);
    }
    // Every library open goes through `LibraryOpener` with the vault key when
    // the file is encrypted (see `vault_cli::open_library`).
    let opened = vault_cli::open_library(&db_path, &sources)?;
    let mut db = CliCtx {
        db: opened.db,
        thumb_keys: opened.thumb_keys,
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
        Cmd::Doctor { .. } | Cmd::Gen { .. } | Cmd::Vault { .. } => return Ok(()),
        Cmd::Thumbs => {
            let v = build_thumbs(&mut db)?;
            if cli.json {
                println!("{v}");
            } else {
                println!(
                    "thumbnails: {} images, {} built or cached, {} failed",
                    v["images"], v["ok"], v["failed"]
                );
            }
            return Ok(());
        }
        Cmd::Rescan => ("library.rescan".to_owned(), json!({})),
        Cmd::Search {
            words,
            limit,
            offset,
        } => (
            "media.search".to_owned(),
            json!({ "query": words.join(" "), "limit": limit, "offset": offset }),
        ),
        Cmd::Query(q) => {
            let QueryArgs {
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
                studio,
                people,
            } = &**q;
            if sort == "position" && playlist.is_none() {
                return Err("--sort position needs --playlist".into());
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
            if let Some(st) = studio {
                a["studio"] = json!(st);
            }
            if !people.is_empty() {
                a["people"] = json!(people);
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
        Cmd::ImportNfo {
            dir,
            apply,
            overwrite,
            ratings,
            history,
        } => {
            let mut a = json!({
                "dry_run": !apply, "overwrite": overwrite, "ratings": ratings, "history": history,
            });
            if let Some(d) = dir {
                a["dir"] = json!(d.to_string_lossy());
            }
            ("meta.import_nfo".to_owned(), a)
        }
        Cmd::Lookup { oshash, fp } => {
            let mut a = json!({});
            if let Some(h) = oshash {
                a["oshash"] = json!(h);
            }
            if let Some(f) = fp {
                a["fp"] = json!(f);
            }
            ("media.by_hash".to_owned(), a)
        }
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
        Cmd::Person { action } => person_call(action),
        Cmd::Studio { action } => studio_call(action),
        Cmd::Info {
            id,
            title,
            clear_title,
            released,
            clear_released,
        } => {
            let mut a = json!({ "id": id });
            if let Some(t) = title {
                a["title"] = json!(t);
            } else if *clear_title {
                a["title"] = Value::Null;
            }
            if let Some(d) = released {
                a["released"] = json!(d);
            } else if *clear_released {
                a["released"] = Value::Null;
            }
            let edits = a.as_object().is_some_and(|o| o.len() > 1);
            (
                if edits {
                    "media.info.set"
                } else {
                    "media.info"
                }
                .to_owned(),
                a,
            )
        }
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

/// Build the thumbnails of every present image through `ThumbService`, keyed
/// when the library is open in vault mode. Counts only, never names.
fn build_thumbs(ctx: &mut CliCtx) -> Result<Value, Fail> {
    use jizzcraft_core::{Filter, HiddenMode, MediaKind};
    use jizzcraft_media::service::{Request, ThumbService};
    let rows = ctx
        .db
        .query(&Filter {
            hidden: HiddenMode::Include,
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    let wanted: Vec<Request> = rows
        .into_iter()
        .filter(|m| m.kind == MediaKind::Image && !m.missing)
        .map(|m| Request {
            id: m.id,
            path: m.path,
            kind: m.kind,
            size: m.size,
            mtime: m.mtime,
            need_meta: false,
        })
        .collect();
    let n = wanted.len();
    let svc = ThumbService::with_keys(paths::cache_dir(), ctx.thumb_keys.clone(), Arc::new(|| {}));
    svc.set_wanted(wanted);
    let (mut ok, mut failed) = (0u64, 0u64);
    while (ok + failed) < n as u64 {
        match svc.try_recv() {
            Some(d) if d.thumb.is_ok() => ok += 1,
            Some(_) => failed += 1,
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    Ok(json!({ "images": n, "ok": ok, "failed": failed }))
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

fn person_call(a: &PersonCmd) -> (String, Value) {
    let (n, v) = match a {
        PersonCmd::List { query, limit } => {
            ("person.list", json!({ "query": query, "limit": limit }))
        }
        PersonCmd::Create {
            name,
            disambiguation,
        } => (
            "person.create",
            json!({ "name": name, "disambiguation": disambiguation }),
        ),
        PersonCmd::Rename { id, name } => ("person.rename", json!({ "id": id, "name": name })),
        PersonCmd::Set {
            id,
            favorite,
            private,
            disambiguation,
        } => (
            "person.set",
            json!({ "id": id, "favorite": favorite, "private": private, "disambiguation": disambiguation }),
        ),
        PersonCmd::Delete { id } => ("person.delete", json!({ "id": id })),
        PersonCmd::Merge { from, into } => ("person.merge", json!({ "from": from, "into": into })),
        PersonCmd::AliasAdd { id, alias } => {
            ("person.alias.add", json!({ "id": id, "alias": alias }))
        }
        PersonCmd::AliasRemove { id, alias } => {
            ("person.alias.remove", json!({ "id": id, "alias": alias }))
        }
        PersonCmd::Assign {
            person,
            media_ids,
            off,
        } => (
            "person.assign",
            json!({ "ids": [person], "media_ids": media_ids, "on": !off }),
        ),
    };
    (n.to_owned(), v)
}

fn studio_call(a: &StudioCmd) -> (String, Value) {
    let (n, v) = match a {
        StudioCmd::List => ("studio.list", json!({})),
        StudioCmd::Create { name } => ("studio.create", json!({ "name": name })),
        StudioCmd::Rename { id, name } => ("studio.rename", json!({ "id": id, "name": name })),
        StudioCmd::Set { id, private } => ("studio.set", json!({ "id": id, "private": private })),
        StudioCmd::Delete { id } => ("studio.delete", json!({ "id": id })),
        StudioCmd::Merge { from, into } => ("studio.merge", json!({ "from": from, "into": into })),
        StudioCmd::Assign {
            studio,
            clear: _,
            media_ids,
        } => (
            "studio.assign",
            json!({ "media_ids": media_ids, "studio": studio }),
        ),
    };
    (n.to_owned(), v)
}

fn tag_call(a: &TagCmd) -> (String, Value) {
    let (n, v) = match a {
        TagCmd::AliasAdd { id, alias } => ("tag.alias.add", json!({ "id": id, "alias": alias })),
        TagCmd::AliasRemove { id, alias } => {
            ("tag.alias.remove", json!({ "id": id, "alias": alias }))
        }
        TagCmd::Aliases { id } => ("tag.aliases", json!({ "id": id })),
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
        "media.query" | "media.search" => {
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
        "meta.import_nfo" => {
            println!(
                "{}: {} files · {} read · {} rejected · {} matched · {} changed · {} unmatched · {} ambiguous",
                if v["dry_run"] == true {
                    "dry run"
                } else {
                    "applied"
                },
                v["files_seen"],
                v["parsed"],
                v["rejected"],
                v["matched"],
                v["applied"],
                v["unmatched"],
                v["ambiguous"]
            );
        }
        "media.by_hash" => {
            for id in v["ids"].as_array().into_iter().flatten() {
                println!("{id}");
            }
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
