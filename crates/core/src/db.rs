//! SQLite library. Versioned schema through `PRAGMA user_version`.
//! Paths are stored as raw bytes so non-UTF-8 filenames survive a round trip.

use crate::error::{Error, Result};
use crate::model::{Media, MediaKind, MediaMeta, Stats};
use rusqlite::{Connection, OptionalExtension, Row, params, types::Value};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Each entry migrates from version `index` to `index + 1`. SQL is append-only;
/// Rust backfills are keyed by target version in `post_migrate`.
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
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

const COLS: &str = "id, path, name, kind, size, mtime, width, height, duration_ms, codec, \
                    added_at, missing, favorite, rating, resume_ms, play_count, \
                    last_played_at, hidden, inbox, damaged, check_note, checked_at, fp";

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
}

impl SortKey {
    pub const ALL: [SortKey; 7] = [
        SortKey::Name,
        SortKey::Added,
        SortKey::Size,
        SortKey::Duration,
        SortKey::Rating,
        SortKey::LastPlayed,
        SortKey::PlayCount,
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
        }
    }

    /// History-derived keys are hidden in discreet mode by the UI.
    pub fn is_history(self) -> bool {
        matches!(self, SortKey::LastPlayed | SortKey::PlayCount)
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
    pub limit: Option<u64>,
    pub offset: u64,
}

pub struct Db {
    pub(crate) conn: Connection,
}

pub fn path_to_blob(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

pub fn blob_to_path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(b))
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

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.pragma_update(None, "foreign_keys", true)?;
        // Background jobs open their own connections; wait instead of failing.
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        // Cleared history must not linger in free pages.
        conn.pragma_update(None, "secure_delete", true)?;
        // WAL is a no-op on in-memory databases; ignore the returned mode.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let mut db = Db { conn };
        db.migrate()?;
        Ok(db)
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

    fn migrate(&mut self) -> Result<()> {
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(Error::Invalid(format!(
                "library schema v{current} is newer than this build (v{SCHEMA_VERSION})"
            )));
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
        if !f.include_missing {
            conds.push("missing = 0".into());
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
            conds.push("name_lc LIKE ? ESCAPE '\\'".into());
            args.push(Value::Text(format!("%{esc}%")));
        }
        let w = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        (w, args)
    }

    pub fn query(&self, f: &Filter) -> Result<Vec<Media>> {
        let (w, mut args) = Self::where_clause(f);
        let dir = if f.desc { "DESC" } else { "ASC" };
        // Missing durations always sort last; `id` keeps paging stable.
        let nulls = match f.sort {
            SortKey::Duration => "duration_ms IS NULL, ",
            SortKey::LastPlayed => "last_played_at IS NULL, ",
            _ => "",
        };
        let mut sql = format!(
            "SELECT {COLS} FROM media{w} ORDER BY {nulls}{} {dir}, id",
            f.sort.column()
        );
        if let Some(l) = f.limit {
            sql.push_str(" LIMIT ? OFFSET ?");
            args.push(Value::Integer(l as i64));
            args.push(Value::Integer(f.offset as i64));
        }
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

fn row_to_media(r: &Row<'_>) -> rusqlite::Result<Media> {
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
