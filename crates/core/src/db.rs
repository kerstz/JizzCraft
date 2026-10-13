//! SQLite library. Versioned schema through `PRAGMA user_version`.
//! Paths are stored as raw bytes so non-UTF-8 filenames survive a round trip.

use crate::error::{Error, Result};
use crate::model::{Media, MediaKind, MediaMeta, Stats};
use rusqlite::{Connection, OptionalExtension, Row, params, types::Value};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Each entry migrates from version `index` to `index + 1`. Append only.
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
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

const COLS: &str = "id, path, name, kind, size, mtime, width, height, duration_ms, codec, \
                    added_at, missing, favorite";

/// Library query. The grid asks for one page at a time.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kind: Option<MediaKind>,
    pub favorites: bool,
    pub name: Option<String>,
    pub include_missing: bool,
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
        // WAL is a no-op on in-memory databases; ignore the returned mode.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let mut db = Db { conn };
        db.migrate()?;
        Ok(db)
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
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
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
        let mut sql = format!("SELECT {COLS} FROM media{w} ORDER BY name_lc, id");
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
        Ok(Stats {
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
    })
}
