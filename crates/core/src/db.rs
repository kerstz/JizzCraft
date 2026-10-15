//! SQLite library. Versioned schema through `PRAGMA user_version`.
//! Paths are stored as raw bytes so non-UTF-8 filenames survive a round trip.

use crate::error::{Error, Result};
use crate::model::{Media, MediaKind, MediaMeta, Stats};
use crate::organize::OrgFilter;
use rusqlite::{Connection, OptionalExtension, Row, params, types::Value};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Each entry migrates from version `index` to `index + 1`. SQL is append-only;
/// Rust backfills are keyed by target version in `post_migrate`.
// Like a diary: you may add pages, you may not tear any out.
const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE folders (
        id        INTEGER PRIMARY KEY,
        path      BLOB NOT NULL UNIQUE,
        added_at  INTEGER NOT NULL,
        scan_gen  INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE media (
        id          INTEGER PRIMARY KEY,
        folder_id   INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        path        BLOB NOT NULL UNIQUE,
        name        TEXT NOT NULL,
        name_lc     TEXT NOT NULL,
        kind        INTEGER NOT NULL,
        size        INTEGER NOT NULL,
        mtime       INTEGER NOT NULL,
        width       INTEGER,
        height      INTEGER,
        duration_ms INTEGER,
        codec       TEXT,
        added_at    INTEGER NOT NULL,
        missing     INTEGER NOT NULL DEFAULT 0,
        favorite    INTEGER NOT NULL DEFAULT 0,
        scan_gen    INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX media_kind ON media(kind);
    CREATE INDEX media_fav  ON media(favorite) WHERE favorite = 1;
    CREATE INDEX media_name ON media(name_lc, id);
    CREATE INDEX media_folder ON media(folder_id);",
    // v2: ratings, history, hidden/inbox, fingerprints, integrity, markers,
    // saved views, settings. Rust backfill for `dir` runs in `post_migrate`.
    "ALTER TABLE media ADD COLUMN dir            BLOB;
    ALTER TABLE media ADD COLUMN rating         INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5);
    ALTER TABLE media ADD COLUMN resume_ms      INTEGER CHECK (resume_ms IS NULL OR resume_ms >= 0);
    ALTER TABLE media ADD COLUMN play_count     INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE media ADD COLUMN last_played_at INTEGER;
    ALTER TABLE media ADD COLUMN hidden         INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE media ADD COLUMN inbox          INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE media ADD COLUMN fp             BLOB;
    ALTER TABLE media ADD COLUMN checksum       BLOB;
    ALTER TABLE media ADD COLUMN checked_at     INTEGER;
    ALTER TABLE media ADD COLUMN damaged        INTEGER NOT NULL DEFAULT 0 CHECK (damaged IN (0,1,2));
    ALTER TABLE media ADD COLUMN check_note     TEXT;
    UPDATE media SET inbox = 0;
    CREATE TABLE markers (
        id         INTEGER PRIMARY KEY,
        media_id   INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        pos_ms     INTEGER NOT NULL CHECK (pos_ms >= 0),
        label      TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        UNIQUE (media_id, pos_ms)
    );
    CREATE INDEX markers_media ON markers(media_id, pos_ms);
    CREATE TABLE saved_views (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        spec       TEXT NOT NULL,
        position   INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
    INSERT INTO settings(key, value) VALUES ('history.enabled','1'), ('resume.enabled','1');
    CREATE INDEX media_dir    ON media(dir, name_lc, id);
    CREATE INDEX media_fp     ON media(fp) WHERE fp IS NOT NULL;
    CREATE INDEX media_size   ON media(size, kind);
    CREATE INDEX media_rating ON media(rating) WHERE rating > 0;
    CREATE INDEX media_played ON media(last_played_at) WHERE last_played_at IS NOT NULL;
    CREATE INDEX media_hidden ON media(hidden) WHERE hidden = 1;
    CREATE INDEX media_inbox  ON media(inbox) WHERE inbox = 1;",
    // v3: playlists, categories and tags. Membership rows only; deleting any of
    // them never touches `media` rows or files.
    "CREATE TABLE playlists (id INTEGER PRIMARY KEY, name TEXT NOT NULL, name_lc TEXT NOT NULL UNIQUE, color INTEGER CHECK (color IS NULL OR color BETWEEN 0 AND 16777215), private INTEGER NOT NULL DEFAULT 0 CHECK (private IN (0,1)), position INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
    CREATE TABLE playlist_items (playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE, media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE, pos INTEGER NOT NULL, added_at INTEGER NOT NULL, PRIMARY KEY (playlist_id, media_id)) WITHOUT ROWID;
    CREATE INDEX playlist_items_pos ON playlist_items(playlist_id, pos);
    CREATE INDEX playlist_items_media ON playlist_items(media_id);
    CREATE TABLE labels (id INTEGER PRIMARY KEY, kind INTEGER NOT NULL CHECK (kind IN (1,2)), parent_id INTEGER REFERENCES labels(id) ON DELETE SET NULL, name TEXT NOT NULL, name_lc TEXT NOT NULL, color INTEGER CHECK (color IS NULL OR color BETWEEN 0 AND 16777215), private INTEGER NOT NULL DEFAULT 0 CHECK (private IN (0,1)), position INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, CHECK (kind = 1 OR parent_id IS NULL));
    CREATE UNIQUE INDEX labels_name ON labels(kind, IFNULL(parent_id, 0), name_lc);
    CREATE INDEX labels_parent ON labels(parent_id);
    CREATE TABLE media_labels (label_id INTEGER NOT NULL REFERENCES labels(id) ON DELETE CASCADE, media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE, added_at INTEGER NOT NULL, PRIMARY KEY (label_id, media_id)) WITHOUT ROWID;
    CREATE INDEX media_labels_media ON media_labels(media_id, label_id);",
    // v4: full-text index (SQLite FTS5, built into the bundled SQLCipher) over
    // file names, tag and category names, and marker notes. `media_fts.rowid`
    // is `media.id`. Triggers keep it current on scan, labelling, marker edits
    // and folder removal. Private tags and categories (and anything under a
    // private category) are never indexed, so discreet mode cannot leak them
    // through search.
    "CREATE VIEW media_fts_src AS
        SELECT m.id AS id, m.name AS name,
            COALESCE((SELECT group_concat(l.name, ' ') FROM media_labels ml JOIN labels l ON l.id = ml.label_id
                WHERE ml.media_id = m.id AND l.kind = 2 AND l.private = 0), '') AS tags,
            COALESCE((SELECT group_concat(l.name, ' ') FROM media_labels ml JOIN labels l ON l.id = ml.label_id
                WHERE ml.media_id = m.id AND l.kind = 1
                AND NOT EXISTS (WITH RECURSIVE up(id) AS (SELECT l.id UNION SELECT p.parent_id FROM labels p JOIN up ON p.id = up.id WHERE p.parent_id IS NOT NULL)
                                SELECT 1 FROM labels x JOIN up ON x.id = up.id WHERE x.private = 1)), '') AS cats,
            COALESCE((SELECT group_concat(k.label, ' ') FROM markers k WHERE k.media_id = m.id AND k.label <> ''), '') AS notes
        FROM media m;
    CREATE VIRTUAL TABLE media_fts USING fts5(name, tags, cats, notes, tokenize = 'unicode61 remove_diacritics 2', prefix = '1 2 3');
    INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src;
    CREATE TRIGGER media_fts_ai AFTER INSERT ON media BEGIN
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = NEW.id;
    END;
    CREATE TRIGGER media_fts_au AFTER UPDATE OF name ON media BEGIN
        DELETE FROM media_fts WHERE rowid = NEW.id;
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = NEW.id;
    END;
    CREATE TRIGGER media_fts_ad AFTER DELETE ON media BEGIN
        DELETE FROM media_fts WHERE rowid = OLD.id;
    END;
    CREATE TRIGGER media_fts_ml_ai AFTER INSERT ON media_labels BEGIN
        DELETE FROM media_fts WHERE rowid = NEW.media_id;
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = NEW.media_id;
    END;
    CREATE TRIGGER media_fts_ml_ad AFTER DELETE ON media_labels
    WHEN EXISTS (SELECT 1 FROM media WHERE id = OLD.media_id) BEGIN
        DELETE FROM media_fts WHERE rowid = OLD.media_id;
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = OLD.media_id;
    END;
    CREATE TRIGGER media_fts_lb_au AFTER UPDATE OF name, private, parent_id ON labels BEGIN
        DELETE FROM media_fts WHERE rowid IN (
            WITH RECURSIVE sub(id) AS (SELECT NEW.id UNION SELECT c.id FROM labels c JOIN sub ON c.parent_id = sub.id)
            SELECT ml.media_id FROM media_labels ml JOIN sub ON ml.label_id = sub.id);
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id IN (
            WITH RECURSIVE sub(id) AS (SELECT NEW.id UNION SELECT c.id FROM labels c JOIN sub ON c.parent_id = sub.id)
            SELECT ml.media_id FROM media_labels ml JOIN sub ON ml.label_id = sub.id);
    END;
    CREATE TRIGGER media_fts_mk_ai AFTER INSERT ON markers BEGIN
        DELETE FROM media_fts WHERE rowid = NEW.media_id;
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = NEW.media_id;
    END;
    CREATE TRIGGER media_fts_mk_au AFTER UPDATE OF label ON markers BEGIN
        DELETE FROM media_fts WHERE rowid = NEW.media_id;
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = NEW.media_id;
    END;
    CREATE TRIGGER media_fts_mk_ad AFTER DELETE ON markers
    WHEN EXISTS (SELECT 1 FROM media WHERE id = OLD.media_id) BEGIN
        DELETE FROM media_fts WHERE rowid = OLD.media_id;
        INSERT INTO media_fts(rowid, name, tags, cats, notes) SELECT id, name, tags, cats, notes FROM media_fts_src WHERE id = OLD.media_id;
    END;",
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

const COLS: &str = "id, path, name, kind, size, mtime, width, height, duration_ms, codec, \
                    added_at, missing, favorite, rating, resume_ms, play_count, \
                    last_played_at, hidden, inbox, damaged, check_note, checked_at, fp";

/// `COLS` with each column prefixed (for joins).
pub(crate) fn row_to_media_cols(prefix: &str) -> String {
    COLS.split(", ")
        .map(|c| format!("{prefix}{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Sort key for library queries. Mapped to SQL through a fixed whitelist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Name,
    Added,
    Size,
    Duration,
    Rating,
    LastPlayed,
    PlayCount,
    /// Stable shuffle driven by `Filter::seed`.
    Random,
    /// Manual order of the playlist in `Filter::org.playlist`. Not in `ALL`:
    /// it only makes sense inside a playlist.
    Position,
}

impl SortKey {
    pub const ALL: [SortKey; 8] = [
        SortKey::Name,
        SortKey::Added,
        SortKey::Size,
        SortKey::Duration,
        SortKey::Rating,
        SortKey::LastPlayed,
        SortKey::PlayCount,
        SortKey::Random,
    ];

    pub fn parse(s: &str) -> Option<SortKey> {
        match s.to_ascii_lowercase().as_str() {
            "name" => Some(SortKey::Name),
            "added" => Some(SortKey::Added),
            "size" => Some(SortKey::Size),
            "duration" => Some(SortKey::Duration),
            "rating" => Some(SortKey::Rating),
            "played" | "last_played" => Some(SortKey::LastPlayed),
            "plays" | "play_count" => Some(SortKey::PlayCount),
            "random" | "shuffle" => Some(SortKey::Random),
            "position" => Some(SortKey::Position),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Added => "added",
            SortKey::Size => "size",
            SortKey::Duration => "duration",
            SortKey::Rating => "rating",
            SortKey::LastPlayed => "played",
            SortKey::PlayCount => "plays",
            SortKey::Random => "random",
            SortKey::Position => "position",
        }
    }

    /// History-derived keys are hidden in discreet mode by the UI.
    pub fn is_history(self) -> bool {
        matches!(self, SortKey::LastPlayed | SortKey::PlayCount)
    }

    pub(crate) fn column_name(self) -> &'static str {
        self.column()
    }

    fn column(self) -> &'static str {
        match self {
            SortKey::Name => "name_lc",
            SortKey::Added => "added_at",
            SortKey::Size => "size",
            SortKey::Duration => "duration_ms",
            SortKey::Rating => "rating",
            SortKey::LastPlayed => "last_played_at",
            SortKey::PlayCount => "play_count",
            SortKey::Random | SortKey::Position => "id",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HiddenMode {
    /// Normal library: hidden items are left out.
    #[default]
    Exclude,
    Only,
    Include,
}

impl HiddenMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exclude" => Some(Self::Exclude),
            "only" => Some(Self::Only),
            "include" => Some(Self::Include),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exclude => "exclude",
            Self::Only => "only",
            Self::Include => "include",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl Orientation {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "landscape" => Some(Self::Landscape),
            "portrait" => Some(Self::Portrait),
            "square" => Some(Self::Square),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Landscape => "landscape",
            Self::Portrait => "portrait",
            Self::Square => "square",
        }
    }
}

/// Library query. The grid asks for one page at a time.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub sort: SortKey,
    pub desc: bool,
    pub kind: Option<MediaKind>,
    pub favorites: bool,
    pub name: Option<String>,
    pub include_missing: bool,
    /// `Some(true)` = has been played, `Some(false)` = never played.
    pub played: Option<bool>,
    pub hidden: HiddenMode,
    pub min_rating: Option<u8>,
    pub unrated: bool,
    /// Has a saved resume point.
    pub in_progress: bool,
    pub min_duration_ms: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub orientation: Option<Orientation>,
    pub added_within_days: Option<u32>,
    pub inbox: bool,
    pub damaged: bool,
    /// Only files flagged missing (overrides `include_missing`).
    pub missing_only: bool,
    pub has_markers: bool,
    /// Restrict to a directory (raw path); `recursive` includes sub-directories.
    pub dir: Option<PathBuf>,
    pub recursive: bool,
    /// Seed for `SortKey::Random`.
    pub seed: u32,
    /// Playlist, tag and category conditions.
    pub org: OrgFilter,
    pub limit: Option<u64>,
    pub offset: u64,
}

pub struct Db {
    pub(crate) conn: Connection,
    /// File this handle was opened from (`None` for in-memory databases).
    pub(crate) path: Option<PathBuf>,
    /// Opened with a vault key: copies made from this handle stay encrypted.
    pub(crate) encrypted: bool,
}

/// What the first bytes of a database file say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    /// No file, or an empty one: a new database will be created.
    Absent,
    /// Starts with `SQLite format 3\0`.
    Plain,
    /// Anything else: SQLCipher files start with a random salt.
    Encrypted,
}

/// Look at the first 16 bytes of `path`. Disk state is the truth; no header
/// file or setting is consulted.
pub fn probe_format(path: &Path) -> Result<FileFormat> {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileFormat::Absent),
        Err(e) => return Err(e.into()),
    };
    let mut buf = [0u8; 16];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(match n {
        0 => FileFormat::Absent,
        16 if &buf == b"SQLite format 3\0" => FileFormat::Plain,
        _ => FileFormat::Encrypted,
    })
}

/// A library location plus the key to open it with. Cheap to clone and send
/// to a job thread; every connection a job needs goes through it.
#[derive(Clone)]
pub struct LibraryOpener {
    pub path: PathBuf,
    pub key: Option<std::sync::Arc<crate::vault::DbKey>>,
}

impl std::fmt::Debug for LibraryOpener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryOpener")
            .field("keyed", &self.key.is_some())
            .finish()
    }
}

impl LibraryOpener {
    pub fn new(path: impl Into<PathBuf>, key: Option<std::sync::Arc<crate::vault::DbKey>>) -> Self {
        LibraryOpener {
            path: path.into(),
            key,
        }
    }

    pub fn open(&self) -> Result<Db> {
        Db::open_with(&self.path, self.key.as_deref())
    }

    pub fn open_read_only(&self) -> Result<Db> {
        Db::open_read_only_with(&self.path, self.key.as_deref())
    }
}

/// `PRAGMA key` and a first read, before anything else touches the file.
fn apply_key(conn: &Connection, key: &crate::vault::DbKey) -> Result<()> {
    use crate::vault::VaultError;
    // SQLCipher logs decryption failures to stderr; a wrong key is an
    // expected, handled outcome here and must not print.
    let _ = conn.execute_batch("PRAGMA cipher_log_level = NONE;");
    let pragma = Zeroizing::new(format!("PRAGMA key = {};", key.pragma_value().as_str()));
    conn.execute_batch(&pragma)
        .map_err(|_| Error::Vault(VaultError::DbCorrupt))?;
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::NotADatabase =>
        {
            Err(Error::Vault(VaultError::DbCorrupt))
        }
        Err(e) => Err(e.into()),
    }
}

pub fn path_to_blob(p: &Path) -> Vec<u8> {
    crate::pathbytes::path_to_bytes(p)
}

pub fn blob_to_path(b: Vec<u8>) -> PathBuf {
    crate::pathbytes::bytes_to_path(b)
}

/// Parent directory of `p` as raw bytes (empty when there is none).
pub fn parent_blob(p: &Path) -> Vec<u8> {
    p.parent().map(path_to_blob).unwrap_or_default()
}

/// `fp1:<32 hex>` form of a 16-byte quick fingerprint.
pub fn fp_string(fp: &[u8]) -> String {
    let mut s = String::with_capacity(4 + fp.len() * 2);
    s.push_str("fp1:");
    for b in fp {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pre-migration copies kept next to the database.
const KEEP_BACKUPS: usize = 3;

/// Keep the newest `KEEP_BACKUPS` files named `<prefix><tail>.db` in `dir`
/// whose `tail` satisfies `tail_ok`; delete the others.
fn rotate_backups(dir: &Path, prefix: &str, tail_ok: impl Fn(&str) -> bool) -> Result<()> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(prefix)
                && n.ends_with(".db")
                && n.len() >= prefix.len() + 3
                && tail_ok(&n[prefix.len()..n.len() - 3])
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, old) in found.into_iter().skip(KEEP_BACKUPS) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

/// Outcome of [`Db::remove_folder_safely`]. Counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveReport {
    /// Media rows dropped so far.
    pub removed: u64,
    /// The cancel flag stopped the job: the root is still imported and a
    /// second run finishes the work.
    pub cancelled: bool,
    /// A `pre-remove` backup was written first (false for in-memory databases).
    pub backed_up: bool,
}

/// Rows deleted per transaction, so the write lock is never held for long.
const REMOVE_CHUNK: i64 = 500;

impl Db {
    /// True when this handle was opened with a vault key.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// File this handle was opened from (`None` for in-memory databases).
    pub fn file_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Open (and migrate) the library. A database written by a newer build is
    /// refused with `Error::FutureSchema`; use `open_read_only` for that case.
    pub fn open(path: &Path) -> Result<Db> {
        Self::open_with(path, None)
    }

    /// Like `open`, with the vault key for an encrypted library. Without a key
    /// an encrypted file is `Error::Locked`; with a key a plain file is
    /// `VaultError::NotEncrypted` and a wrong key `VaultError::DbCorrupt`. A
    /// missing file is created encrypted when a key is given.
    pub fn open_with(path: &Path, key: Option<&crate::vault::DbKey>) -> Result<Db> {
        use crate::vault::VaultError;
        match (probe_format(path)?, key) {
            (FileFormat::Encrypted, None) => return Err(Error::Locked),
            (FileFormat::Plain, Some(_)) => return Err(Error::Vault(VaultError::NotEncrypted)),
            // The header is the only envelope of the key: never start a new
            // clear library next to one (a lost or replaced library.db).
            (FileFormat::Absent, None) if crate::vault::valid_header_exists(path) => {
                return Err(Error::Vault(VaultError::HeaderOrphan));
            }
            _ => {}
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        if let Some(k) = key {
            apply_key(&conn, k)?;
        }
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(Error::FutureSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        Self::init(conn, Some(path), key.is_some())
    }

    /// Read-only handle: no migration, no journal change, no writes. For a
    /// library from a newer build, or when the folder is not writable.
    pub fn open_read_only(path: &Path) -> Result<Db> {
        Self::open_read_only_with(path, None)
    }

    /// `open_read_only` for an encrypted library; same errors as `open_with`.
    pub fn open_read_only_with(path: &Path, key: Option<&crate::vault::DbKey>) -> Result<Db> {
        use crate::vault::VaultError;
        match (probe_format(path)?, key) {
            (FileFormat::Encrypted, None) => return Err(Error::Locked),
            (FileFormat::Plain, Some(_)) => return Err(Error::Vault(VaultError::NotEncrypted)),
            _ => {}
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        if let Some(k) = key {
            apply_key(&conn, k)?;
        }
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(Db {
            conn,
            path: Some(path.to_path_buf()),
            encrypted: key.is_some(),
        })
    }

    pub fn open_in_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?, None, false)
    }

    fn init(conn: Connection, path: Option<&Path>, encrypted: bool) -> Result<Db> {
        conn.pragma_update(None, "foreign_keys", true)?;
        // Background jobs open their own connections; wait instead of failing.
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        // Cleared history must not linger in free pages.
        conn.pragma_update(None, "secure_delete", true)?;
        // WAL needs shared memory, which network filesystems do not give.
        let mode = if path.is_some_and(crate::fsutil::is_network_fs) {
            "DELETE"
        } else {
            "WAL"
        };
        // No-op (returns "memory") on in-memory databases; ignore the result.
        let _ = conn.query_row(&format!("PRAGMA journal_mode = {mode}"), [], |_| Ok(()));
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut db = Db {
            conn,
            path: path.map(Path::to_path_buf),
            encrypted,
        };
        db.migrate(path)?;
        Ok(db)
    }

    /// The frozen migration SQL, so tests can rebuild every historical schema.
    #[doc(hidden)]
    pub fn migrations_for_tests() -> &'static [&'static str] {
        MIGRATIONS
    }

    /// Raw connection for tests that need to poke the schema directly.
    #[doc(hidden)]
    pub fn conn_for_tests(&self) -> &Connection {
        &self.conn
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// `PRAGMA quick_check`. Slow on a big library: run it on a thread with a
    /// separate connection (`Db::open_read_only`).
    pub fn quick_check(&self) -> std::result::Result<(), String> {
        let mut st = self
            .conn
            .prepare("PRAGMA quick_check")
            .map_err(|e| e.to_string())?;
        let rows: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| e.to_string())?;
        if rows.len() == 1 && rows[0] == "ok" {
            Ok(())
        } else {
            Err(rows.join("; "))
        }
    }

    fn migrate(&mut self, path: Option<&Path>) -> Result<()> {
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(Error::FutureSchema {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        if let Some(p) = path.filter(|_| current > 0 && current < SCHEMA_VERSION) {
            self.backup_before_migrating(p, current)?;
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            Self::post_migrate(&tx, (i + 1) as i64)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// `VACUUM INTO <stem>.pre-v{N}.db` beside the database; keeps the newest
    /// `KEEP_BACKUPS`. A failed backup stops the migration: better a refusal
    /// than an upgrade with no way back.
    fn backup_before_migrating(&self, db_path: &Path, version: i64) -> Result<()> {
        let dir = db_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let stem = db_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("library");
        let target = dir.join(format!("{stem}.pre-v{version}.db"));
        let _ = std::fs::remove_file(&target);
        let target_str = target
            .to_str()
            .ok_or_else(|| Error::Invalid("database path is not UTF-8".into()))?;
        self.conn.execute("VACUUM INTO ?1", [target_str])?;

        rotate_backups(dir, &format!("{stem}.pre-v"), |tail| {
            tail.chars().all(|c| c.is_ascii_digit())
        })?;
        Ok(())
    }

    /// Rust-side backfills that SQL alone cannot express, keyed by target version.
    fn post_migrate(tx: &rusqlite::Transaction<'_>, version: i64) -> Result<()> {
        if version == 2 {
            let rows: Vec<(i64, Vec<u8>)> = {
                let mut st = tx.prepare("SELECT id, path FROM media WHERE dir IS NULL")?;
                let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                it.collect::<std::result::Result<_, _>>()?
            };
            let mut up = tx.prepare("UPDATE media SET dir = ?2 WHERE id = ?1")?;
            for (id, path) in rows {
                up.execute(params![id, parent_blob(&blob_to_path(path))])?;
            }
        }
        Ok(())
    }

    // ---- folders -------------------------------------------------------

    /// Returns `(folder_id, newly_created)`. Importing twice is a no-op.
    pub fn add_folder(&mut self, path: &Path) -> Result<(i64, bool)> {
        let blob = path_to_blob(path);
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO folders(path, added_at) VALUES (?1, ?2)",
            params![blob, now_secs()],
        )?;
        let id = self
            .conn
            .query_row("SELECT id FROM folders WHERE path = ?1", [&blob], |r| {
                r.get(0)
            })?;
        Ok((id, n > 0))
    }

    pub fn folders(&self) -> Result<Vec<(i64, PathBuf)>> {
        let mut st = self
            .conn
            .prepare("SELECT id, path FROM folders ORDER BY id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, blob_to_path(r.get(1)?))))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Forget one imported root and every media row under it (labels, markers
    /// and playlist entries go with them through `ON DELETE CASCADE`).
    /// Only database rows are touched, never a file on disk. Returns how many
    /// media rows were dropped, or `None` when `path` is not an imported root.
    pub fn remove_folder(&mut self, path: &Path) -> Result<Option<u64>> {
        let blob = path_to_blob(path);
        let tx = self.conn.transaction()?;
        let id: Option<i64> = tx
            .query_row("SELECT id FROM folders WHERE path = ?1", [&blob], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM media WHERE folder_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM folders WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(Some(n as u64))
    }

    /// `VACUUM INTO <stem>.pre-remove-<ts>.db` beside the database (same key
    /// as the library: the copy is made by this connection); keeps the newest
    /// `KEEP_BACKUPS`. `None` for an in-memory database.
    pub fn backup_before_remove(&self) -> Result<Option<PathBuf>> {
        self.backup_tagged("pre-remove")
    }

    /// `VACUUM INTO <stem>.<tag>-<ts>.db` beside the database, newest
    /// `KEEP_BACKUPS` per tag kept. Used for `pre-remove` and `pre-check`
    /// (taken when the start-up integrity check fails). Works on a read-only
    /// connection. `None` for an in-memory database.
    pub fn backup_tagged(&self, tag: &str) -> Result<Option<PathBuf>> {
        let Some(db_path) = self.path.as_deref() else {
            return Ok(None);
        };
        let dir = db_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let stem = db_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("library");
        let mut ts = now_secs();
        let target = loop {
            let t = dir.join(format!("{stem}.{tag}-{ts}.db"));
            if !t.exists() {
                break t;
            }
            ts += 1;
        };
        let target_str = target
            .to_str()
            .ok_or_else(|| Error::Invalid("database path is not UTF-8".into()))?;
        // `query_only` (read-only handles) refuses VACUUM INTO although the
        // source is only read; the file flag still guards the library itself.
        let query_only: bool = self
            .conn
            .pragma_query_value(None, "query_only", |r| r.get(0))?;
        if query_only {
            self.conn.pragma_update(None, "query_only", false)?;
        }
        let r = self.conn.execute("VACUUM INTO ?1", [target_str]);
        if query_only {
            let _ = self.conn.pragma_update(None, "query_only", true);
        }
        r?;
        rotate_backups(dir, &format!("{stem}.{tag}-"), |tail| {
            !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
        })?;
        Ok(Some(target))
    }

    /// Like [`Db::remove_folder`], for jobs: writes a `pre-remove` backup
    /// first (a failed backup aborts, nothing is deleted), then deletes the
    /// media rows in bounded chunks, checking `cancel` between chunks, and
    /// finally the root itself. `progress` gets the running count of dropped
    /// rows. `None` when `path` is not an imported root (no backup made).
    pub fn remove_folder_safely(
        &mut self,
        path: &Path,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<Option<RemoveReport>> {
        use std::sync::atomic::Ordering;
        let blob = path_to_blob(path);
        let id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM folders WHERE path = ?1", [&blob], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let backed_up = self.backup_before_remove()?.is_some();
        let mut removed = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Some(RemoveReport {
                    removed,
                    cancelled: true,
                    backed_up,
                }));
            }
            let n = self.conn.execute(
                "DELETE FROM media WHERE id IN \
                 (SELECT id FROM media WHERE folder_id = ?1 LIMIT ?2)",
                params![id, REMOVE_CHUNK],
            )?;
            if n == 0 {
                break;
            }
            removed += n as u64;
            progress(removed);
        }
        self.conn
            .execute("DELETE FROM folders WHERE id = ?1", [id])?;
        Ok(Some(RemoveReport {
            removed,
            cancelled: false,
            backed_up,
        }))
    }

    // ---- media ---------------------------------------------------------

    fn where_clause(f: &Filter) -> (String, Vec<Value>) {
        let mut conds: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if let Some(k) = f.kind {
            conds.push("kind = ?".into());
            args.push(Value::Integer(k.as_i64()));
        }
        if f.favorites {
            conds.push("favorite = 1".into());
        }
        if f.missing_only {
            conds.push("missing = 1".into());
        } else if !f.include_missing {
            conds.push("missing = 0".into());
        }
        match f.hidden {
            HiddenMode::Exclude => conds.push("hidden = 0".into()),
            HiddenMode::Only => conds.push("hidden = 1".into()),
            HiddenMode::Include => {}
        }
        if let Some(r) = f.min_rating {
            conds.push("rating >= ?".into());
            args.push(Value::Integer(r.min(5) as i64));
        }
        if f.unrated {
            conds.push("rating = 0".into());
        }
        if f.in_progress {
            conds.push("resume_ms IS NOT NULL".into());
        }
        for (v, sql) in [
            (f.min_duration_ms, "duration_ms >= ?"),
            (f.max_duration_ms, "duration_ms <= ?"),
            (f.min_size, "size >= ?"),
            (f.max_size, "size <= ?"),
        ] {
            if let Some(v) = v {
                conds.push(sql.into());
                args.push(Value::Integer(v.min(i64::MAX as u64) as i64));
            }
        }
        match f.orientation {
            Some(Orientation::Landscape) => conds.push("width > height".into()),
            Some(Orientation::Portrait) => conds.push("height > width".into()),
            Some(Orientation::Square) => conds.push("width = height".into()),
            None => {}
        }
        if let Some(days) = f.added_within_days {
            conds.push("media.added_at >= ?".into());
            args.push(Value::Integer(now_secs() - days as i64 * 86_400));
        }
        if f.inbox {
            conds.push("inbox = 1".into());
        }
        if f.damaged {
            conds.push("damaged > 0".into());
        }
        if f.has_markers {
            conds.push("EXISTS (SELECT 1 FROM markers k WHERE k.media_id = media.id)".into());
        }
        if let Some(d) = &f.dir {
            let mut blob = path_to_blob(d);
            while blob.len() > 1 && blob.last() == Some(&crate::pathbytes::SEP) {
                blob.pop();
            }
            if f.recursive {
                let mut prefix = blob.clone();
                if prefix.last() != Some(&crate::pathbytes::SEP) {
                    prefix.push(crate::pathbytes::SEP);
                }
                conds.push("(dir = ? OR substr(dir, 1, ?) = ?)".into());
                args.push(Value::Blob(blob));
                args.push(Value::Integer(prefix.len() as i64));
                args.push(Value::Blob(prefix));
            } else {
                conds.push("dir = ?".into());
                args.push(Value::Blob(blob));
            }
        }
        match f.played {
            Some(true) => conds.push("last_played_at IS NOT NULL".into()),
            Some(false) => conds.push("play_count = 0 AND last_played_at IS NULL".into()),
            None => {}
        }
        if let Some(n) = f.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            let esc = n
                .to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            // Full-text match (names, tags, categories, marker notes, prefix
            // and accent insensitive) or, as before, a substring of the name.
            match crate::search::fts_query(n) {
                Some(q) => {
                    conds.push(
                        "(media.id IN (SELECT rowid FROM media_fts WHERE media_fts MATCH ?) \
                         OR name_lc LIKE ? ESCAPE '\\')"
                            .into(),
                    );
                    args.push(Value::Text(q));
                }
                None => conds.push("name_lc LIKE ? ESCAPE '\\'".into()),
            }
            args.push(Value::Text(format!("%{esc}%")));
        }
        crate::organize::push_conds(&f.org, &mut conds, &mut args);
        let w = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        (w, args)
    }

    pub fn query(&self, f: &Filter) -> Result<Vec<Media>> {
        if let (SortKey::Position, Some(pl)) = (f.sort, f.org.playlist) {
            return self.query_playlist_order(f, pl);
        }
        let (w, mut args) = Self::where_clause(f);
        let dir = if f.desc { "DESC" } else { "ASC" };
        // Missing durations always sort last; `id` keeps paging stable.
        let nulls = match f.sort {
            SortKey::Duration => "duration_ms IS NULL, ",
            SortKey::LastPlayed => "last_played_at IS NULL, ",
            _ => "",
        };
        let order = if f.sort == SortKey::Random {
            // Stable for a given seed, so paging never repeats or skips rows.
            // Knuth multiplicative hash of (id + seed) mod 2^32. The seed is masked
            // to 31 bits so the product stays inside SQLite's 64-bit integers.
            args.push(Value::Integer((f.seed & 0x7fff_ffff) as i64));
            "(((id + ?) * 2654435761) % 4294967296)".to_owned()
        } else {
            f.sort.column().to_owned()
        };
        let mut sql = format!("SELECT {COLS} FROM media{w} ORDER BY {nulls}{order} {dir}, id");
        if let Some(l) = f.limit {
            sql.push_str(" LIMIT ? OFFSET ?");
            args.push(Value::Integer(l as i64));
            args.push(Value::Integer(f.offset as i64));
        }
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), row_to_media)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// `SortKey::Position`: walk the playlist's own index so a page costs
    /// O(offset + limit), not O(playlist).
    fn query_playlist_order(&self, f: &Filter, pl: i64) -> Result<Vec<Media>> {
        let mut rest = f.clone();
        rest.org.playlist = None;
        let (w, wargs) = Self::where_clause(&rest);
        let mut args = vec![Value::Integer(pl)];
        args.extend(wargs);
        let cols: Vec<String> = COLS.split(", ").map(|c| format!("media.{c}")).collect();
        let w = match w.strip_prefix(" WHERE ") {
            Some(c) => format!(" AND {c}"),
            None => String::new(),
        };
        let dir = if f.desc { "DESC" } else { "ASC" };
        let sql = if let (true, Some(l)) = (w.is_empty(), f.limit) {
            // No other condition: page the playlist's index first, join after.
            args.push(Value::Integer(l as i64));
            args.push(Value::Integer(f.offset as i64));
            format!(
                "SELECT {} FROM (SELECT media_id, pos, added_at FROM playlist_items \
                 WHERE playlist_id = ? ORDER BY pos {dir}, added_at, media_id LIMIT ? OFFSET ?) p \
                 JOIN media ON media.id = p.media_id ORDER BY p.pos {dir}, p.added_at, p.media_id",
                cols.join(", ")
            )
        } else {
            let mut sql = format!(
                "SELECT {} FROM playlist_items p JOIN media ON media.id = p.media_id \
                 WHERE p.playlist_id = ?{w} ORDER BY p.pos {dir}, p.added_at, p.media_id",
                cols.join(", ")
            );
            if let Some(l) = f.limit {
                sql.push_str(" LIMIT ? OFFSET ?");
                args.push(Value::Integer(l as i64));
                args.push(Value::Integer(f.offset as i64));
            }
            sql
        };
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), row_to_media)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn count(&self, f: &Filter) -> Result<u64> {
        let (w, args) = Self::where_clause(f);
        let n: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM media{w}"),
            rusqlite::params_from_iter(args),
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn get(&self, id: i64) -> Result<Option<Media>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLS} FROM media WHERE id = ?1"),
                [id],
                row_to_media,
            )
            .optional()?)
    }

    pub fn set_favorite(&self, id: i64, fav: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE media SET favorite = ?2 WHERE id = ?1",
            params![id, fav],
        )?;
        if n == 0 {
            return Err(Error::NotFound(format!("media id {id}")));
        }
        Ok(())
    }

    /// Returns the new favorite state.
    pub fn toggle_favorite(&self, id: i64) -> Result<bool> {
        let cur = self
            .get(id)?
            .ok_or_else(|| Error::NotFound(format!("media id {id}")))?
            .favorite;
        self.set_favorite(id, !cur)?;
        Ok(!cur)
    }

    /// Fill in metadata discovered later (video probing). Only overwrites
    /// fields that are `Some`.
    pub fn set_meta(&self, id: i64, m: &MediaMeta) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET
                width = COALESCE(?2, width), height = COALESCE(?3, height),
                duration_ms = COALESCE(?4, duration_ms), codec = COALESCE(?5, codec)
             WHERE id = ?1",
            params![
                id,
                m.width,
                m.height,
                m.duration_ms.map(|v| v as i64),
                m.codec
            ],
        )?;
        Ok(())
    }

    pub fn stats(&self) -> Result<Stats> {
        let (total, images, videos, favorites, missing, bytes): (i64, i64, i64, i64, i64, i64) =
            self.conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(kind = 0), 0), COALESCE(SUM(kind = 1), 0),
                        COALESCE(SUM(favorite = 1), 0), COALESCE(SUM(missing = 1), 0),
                        COALESCE(SUM(size), 0)
                 FROM media",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )?;
        let folders: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM folders", [], |r| r.get(0))?;
        let (rated, hidden, inbox, damaged, played): (i64, i64, i64, i64, i64) =
            self.conn.query_row(
                "SELECT COALESCE(SUM(rating > 0), 0), COALESCE(SUM(hidden = 1), 0),
                        COALESCE(SUM(inbox = 1), 0), COALESCE(SUM(damaged > 0), 0),
                        COALESCE(SUM(last_played_at IS NOT NULL), 0)
                 FROM media",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
        Ok(Stats {
            rated: rated as u64,
            hidden: hidden as u64,
            inbox: inbox as u64,
            damaged: damaged as u64,
            played: played as u64,
            total: total as u64,
            images: images as u64,
            videos: videos as u64,
            favorites: favorites as u64,
            missing: missing as u64,
            folders: folders as u64,
            total_bytes: bytes as u64,
        })
    }
}

pub(crate) fn row_to_media(r: &Row<'_>) -> rusqlite::Result<Media> {
    Ok(Media {
        id: r.get(0)?,
        path: blob_to_path(r.get(1)?),
        name: r.get(2)?,
        kind: MediaKind::from_i64(r.get(3)?),
        size: r.get::<_, i64>(4)? as u64,
        mtime: r.get(5)?,
        width: r.get(6)?,
        height: r.get(7)?,
        duration_ms: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        codec: r.get(9)?,
        added_at: r.get(10)?,
        missing: r.get::<_, i64>(11)? != 0,
        favorite: r.get::<_, i64>(12)? != 0,
        rating: r.get::<_, i64>(13)?.clamp(0, 5) as u8,
        resume_ms: r.get::<_, Option<i64>>(14)?.map(|v| v as u64),
        play_count: r.get::<_, i64>(15)?.max(0) as u32,
        last_played_at: r.get(16)?,
        hidden: r.get::<_, i64>(17)? != 0,
        inbox: r.get::<_, i64>(18)? != 0,
        damaged: r.get::<_, i64>(19)?.clamp(0, 2) as u8,
        check_note: r.get(20)?,
        checked_at: r.get(21)?,
        fp: r.get::<_, Option<Vec<u8>>>(22)?.map(|b| fp_string(&b)),
    })
}
