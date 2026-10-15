//! Plain <-> encrypted conversion of the library and crash recovery
//! (issue #13, step 4). Engine only: no UI, no CLI, no thumbnail store.
//!
//! Disk state is the truth: the first 16 bytes of `library.db` decide the mode
//! (`db::probe_format`). The commit point of a conversion is the single
//! `rename(tmp, library.db)`; everything before it only creates files that
//! `recover` deletes, everything after it is completed or cleaned by
//! `recover`. Passphrases are only passed to `vault::wrap` / `vault::unlock`;
//! they are never logged and errors are the fixed `VaultError` messages.
//!
//! Order of an `enable` (see `docs/vault.md`):
//!
//! 1. `wal_checkpoint(TRUNCATE)`, `journal_mode = DELETE`, exclusive lock held
//!    for the whole conversion (`locking_mode = EXCLUSIVE`), no `-wal` left;
//! 2. `library.vault.new` written and synced;
//! 3. `ATTACH library.db.enc-tmp KEY ...` and `sqlcipher_export`;
//! 4. verification of the copy (key, cipher integrity, quick check, row
//!    counts, `user_version`, FTS integrity check);
//! 5. fsync of the file and the directory;
//! 6. **commit point**: rename `enc-tmp` over `library.db`;
//! 7. rename `library.vault.new` to `library.vault`;
//! 8. delete the clear `pre-vN` backups and the clear thumbnail caches.

use crate::db::{FileFormat, probe_format};
use crate::vault::{
    DbKey, HEADER_LEN, KdfParams, VaultError, VaultKey, header_path, unlock, validate_header, wrap,
};
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

type Res<T> = Result<T, VaultError>;

/// Points where a test stops the conversion to emulate a crash: the function
/// returns `VaultError::Interrupted` and leaves the disk exactly as it is.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    /// WAL checkpointed, journal mode DELETE, lock held.
    Checkpoint,
    /// `library.vault.new` written (enable) or new header written (change).
    HeaderNew,
    /// Half of the new header written (change passphrase).
    TornHeader,
    /// `sqlcipher_export` finished.
    Export,
    /// Copy verified.
    Verify,
    /// Copy and directory synced.
    Sync,
    /// Renamed over `library.db`.
    Commit,
    /// Header renamed (enable) or deleted (disable).
    Header,
    /// Clear leftovers half removed.
    Cleanup,
    /// Test action, not a stop: right after the commit, `library.vault.new`
    /// is deleted (what a racing `recover` used to do).
    LoseHeaderNew,
}

/// Where to look for the clear thumbnail caches, plus test hooks.
#[derive(Debug, Default, Clone)]
pub struct ConvertOptions {
    /// Cache directory holding `thumbs/` and `previews/`; `None` means
    /// `paths::cache_dir()`.
    pub cache_dir: Option<PathBuf>,
    #[doc(hidden)]
    pub fail_at: Option<FailPoint>,
    /// Set by the caller to stop the conversion. Looked at only before the
    /// commit point; after it the conversion always finishes.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Set by the engine just before the commit point (the rename): from then
    /// on a cancel request has no effect.
    pub committed: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Test synchronisation: when the conversion reaches the point it waits
    /// on the barrier twice (reached, then released).
    #[doc(hidden)]
    pub pause: Option<(FailPoint, std::sync::Arc<std::sync::Barrier>)>,
}

impl ConvertOptions {
    /// Cache directory to clean. An explicit `cache_dir` wins. Otherwise the
    /// global cache is used only when `db_path` is the library the process
    /// resolves by itself (default location or `--home`); a library given
    /// with `--db` / `JIZZCRAFT_DB` gets a `cache` directory next to it, so
    /// converting it never wipes the cache of another library.
    fn cache(&self, db_path: &Path) -> PathBuf {
        if let Some(c) = &self.cache_dir {
            return c.clone();
        }
        let env_db = std::env::var_os("JIZZCRAFT_DB").is_some();
        let resolved = db_path == crate::paths::db_path();
        if resolved && (!env_db || crate::paths::home().is_some()) {
            crate::paths::cache_dir()
        } else {
            dir_of(db_path).join("cache")
        }
    }

    /// The cache directory these options clean for `db_path`.
    #[doc(hidden)]
    pub fn cache_for(&self, db_path: &Path) -> PathBuf {
        self.cache(db_path)
    }

    /// `Cancelled` when the caller asked to stop (only valid before commit).
    fn check_cancel(&self) -> Res<()> {
        let on = self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst));
        if on {
            Err(VaultError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn mark_committed(&self) {
        if let Some(c) = &self.committed {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn hit(&self, p: FailPoint) -> Res<()> {
        if let Some((at, b)) = &self.pause
            && *at == p
        {
            b.wait();
            b.wait();
        }
        if self.fail_at == Some(p) {
            Err(VaultError::Interrupted)
        } else {
            Ok(())
        }
    }
}

fn suffixed(db_path: &Path, suffix: &str) -> PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn enc_tmp(db: &Path) -> PathBuf {
    suffixed(db, ".enc-tmp")
}

fn plain_tmp(db: &Path) -> PathBuf {
    suffixed(db, ".plain-tmp")
}

fn header_new(db: &Path) -> PathBuf {
    suffixed(&header_path(db), ".new")
}

/// Intent marker of a disable (`library.vault.disabling`): written and synced
/// before the commit point, removed at the very end. It is the only thing that
/// lets `recover` delete the header of a library that is found clear.
fn disabling_marker(db: &Path) -> PathBuf {
    suffixed(&header_path(db), ".disabling")
}

/// Advisory lock `library.lock` next to the library, held for the whole of a
/// conversion, a passphrase change or a recovery. Another holder (a
/// conversion in another thread or process) makes the call `Busy`: `recover`
/// must never delete what a live conversion is about to commit. It is
/// best effort where the lock file cannot be created (read-only directory):
/// nothing can be converted there anyway.
struct ConvertLock(#[allow(dead_code)] Option<fs::File>);

fn lock_path(db: &Path) -> PathBuf {
    header_path(db).with_extension("lock")
}

fn acquire(db: &Path) -> Res<ConvertLock> {
    let f = crate::fsutil::private_file_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(db));
    let Ok(f) = f else {
        return Ok(ConvertLock(None));
    };
    match f.try_lock() {
        Ok(()) => Ok(ConvertLock(Some(f))),
        Err(fs::TryLockError::WouldBlock) => Err(VaultError::Busy),
        Err(fs::TryLockError::Error(_)) => Ok(ConvertLock(None)),
    }
}

fn sqlite_err(e: rusqlite::Error) -> VaultError {
    match e {
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(
                f.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            VaultError::Busy
        }
        _ => VaultError::ConvertFailed,
    }
}

fn io_err(_: std::io::Error) -> VaultError {
    VaultError::ConvertFailed
}

fn dir_of(p: &Path) -> &Path {
    p.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn sync_file(p: &Path) -> Res<()> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(p)
        .and_then(|f| f.sync_all())
        .map_err(io_err)
}

fn remove(p: &Path) {
    let _ = fs::remove_file(p);
}

/// Write `bytes` to `dst` (replacing it) and sync. `torn` writes half only.
fn write_synced(dst: &Path, bytes: &[u8], torn: bool) -> Res<()> {
    use std::io::Write;
    remove(dst);
    let mut f = crate::fsutil::private_file_options()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(io_err)?;
    let n = if torn { bytes.len() / 2 } else { bytes.len() };
    f.write_all(&bytes[..n]).map_err(io_err)?;
    f.sync_all().map_err(io_err)?;
    sync_dir(dir_of(dst));
    Ok(())
}

/// Backups next to the database: `<stem>.pre-v<digits>.db` (migrations) and
/// `<stem>.pre-remove-<digits>.db` / `<stem>.pre-check-<digits>.db` (folder
/// removal, failed start-up check).
fn pre_vn_backups(db_path: &Path) -> Vec<PathBuf> {
    let stem = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("library");
    let prefixes = [
        format!("{stem}.pre-v"),
        format!("{stem}.pre-remove-"),
        format!("{stem}.pre-check-"),
    ];
    let Ok(rd) = fs::read_dir(dir_of(db_path)) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            prefixes.iter().any(|prefix| {
                n.starts_with(prefix.as_str())
                    && n.ends_with(".db")
                    && n.len() > prefix.len() + 3
                    && n[prefix.len()..n.len() - 3]
                        .chars()
                        .all(|c| c.is_ascii_digit())
            })
        })
        .map(|e| e.path())
        .collect()
}

/// Remove backups whose format is `fmt` (clear ones after enabling, encrypted
/// ones after disabling: nobody could read the latter any more).
fn remove_backups(db_path: &Path, fmt: FileFormat) {
    for b in pre_vn_backups(db_path) {
        if probe_format(&b).ok() == Some(fmt) {
            remove(&b);
        }
    }
}

fn remove_clear_caches(cache: &Path) {
    let _ = fs::remove_dir_all(cache.join("thumbs"));
    let _ = fs::remove_dir_all(cache.join("previews"));
}

fn open_keyed(path: &Path, key: Option<&DbKey>, flags: OpenFlags) -> Res<Connection> {
    let conn = Connection::open_with_flags(path, flags).map_err(sqlite_err)?;
    let _ = conn.execute_batch("PRAGMA cipher_log_level = NONE;");
    if let Some(k) = key {
        let pragma = Zeroizing::new(format!("PRAGMA key = {};", k.pragma_value().as_str()));
        conn.execute_batch(&pragma)
            .map_err(|_| VaultError::DbCorrupt)?;
    }
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::NotADatabase => {
            VaultError::DbCorrupt
        }
        e => sqlite_err(e),
    })?;
    Ok(conn)
}

/// Ordinary tables (virtual and shadow ones included) and their row counts.
fn row_counts(conn: &Connection) -> Res<Vec<(String, i64)>> {
    let names: Vec<String> = {
        let mut st = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(sqlite_err)?;
        st.query_map([], |r| r.get::<_, String>(0))
            .map_err(sqlite_err)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_err)?
    };
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        // `n` comes from sqlite_master; quoted identifier, not user input.
        let q = format!("SELECT count(*) FROM \"{}\"", n.replace('"', "\"\""));
        let c: i64 = conn.query_row(&q, [], |r| r.get(0)).map_err(sqlite_err)?;
        out.push((n, c));
    }
    Ok(out)
}

fn user_version(conn: &Connection) -> Res<i64> {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sqlite_err)
}

fn fts_tables(conn: &Connection) -> Res<Vec<String>> {
    let mut st = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND sql LIKE 'CREATE VIRTUAL TABLE%USING fts5%'",
        )
        .map_err(sqlite_err)?;
    st.query_map([], |r| r.get::<_, String>(0))
        .map_err(sqlite_err)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_err)
}

/// Everything the copy must satisfy before it may replace the library.
fn verify(
    copy: &Path,
    key: Option<&DbKey>,
    want_counts: &[(String, i64)],
    want_version: i64,
) -> Res<()> {
    let bad = |_| VaultError::VerifyFailed;
    let want_fmt = if key.is_some() {
        FileFormat::Encrypted
    } else {
        FileFormat::Plain
    };
    if probe_format(copy).map_err(|_| VaultError::VerifyFailed)? != want_fmt {
        return Err(VaultError::VerifyFailed);
    }
    let conn = open_keyed(copy, key, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|e| match e {
        VaultError::Busy => e,
        _ => VaultError::VerifyFailed,
    })?;
    if key.is_some() {
        let mut st = conn.prepare("PRAGMA cipher_integrity_check").map_err(bad)?;
        let n = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(bad)?
            .count();
        if n != 0 {
            return Err(VaultError::VerifyFailed);
        }
    }
    let q: Vec<String> = {
        let mut st = conn.prepare("PRAGMA quick_check").map_err(bad)?;
        st.query_map([], |r| r.get::<_, String>(0))
            .map_err(bad)?
            .collect::<Result<_, _>>()
            .map_err(bad)?
    };
    if q != ["ok"] {
        return Err(VaultError::VerifyFailed);
    }
    if row_counts(&conn).map_err(|_| VaultError::VerifyFailed)? != want_counts {
        return Err(VaultError::VerifyFailed);
    }
    if user_version(&conn).map_err(|_| VaultError::VerifyFailed)? != want_version {
        return Err(VaultError::VerifyFailed);
    }
    for t in fts_tables(&conn).map_err(|_| VaultError::VerifyFailed)? {
        let sql = format!(
            "INSERT INTO \"{0}\"(\"{0}\") VALUES('integrity-check')",
            t.replace('"', "\"\"")
        );
        conn.execute(&sql, []).map_err(bad)?;
    }
    Ok(())
}

/// Result of the locked copy phase, used to decide cleanup.
struct Staged {
    /// Connection that still holds the exclusive lock on the source.
    /// Only read off Windows, where the lock is dropped before the rename.
    #[cfg_attr(windows, allow(dead_code))]
    lock: Connection,
}

/// Steps 1 to 5: lock the source, export into `tmp`, verify, sync.
fn stage(
    db_path: &Path,
    src_key: Option<&DbKey>,
    tmp: &Path,
    dst_key: Option<&DbKey>,
    header_to_write: Option<&[u8; HEADER_LEN]>,
    opts: &ConvertOptions,
) -> Res<Staged> {
    opts.check_cancel()?;
    remove(tmp);
    // The copy is owner-only from its first byte (an empty file is a new
    // database for ATTACH).
    crate::fsutil::precreate_private(tmp);
    // CREATE is needed so ATTACH can create the copy (it inherits the flags);
    // the source exists, `require_format` checked.
    let src = open_keyed(
        db_path,
        src_key,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    src.busy_timeout(Duration::from_millis(300))
        .map_err(sqlite_err)?;
    // 1. fold the WAL into the main file and leave WAL mode.
    let (busy, _, _): (i64, i64, i64) = src
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_err)?;
    if busy != 0 {
        return Err(VaultError::Busy);
    }
    let mode: String = src
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .map_err(sqlite_err)?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(VaultError::Busy);
    }
    // Exclusive lock kept until the connection closes, not just for one
    // transaction: nobody can write between the export and the rename.
    src.query_row("PRAGMA locking_mode = EXCLUSIVE", [], |_| Ok(()))
        .map_err(sqlite_err)?;
    src.execute_batch("BEGIN EXCLUSIVE; COMMIT;")
        .map_err(sqlite_err)?;
    if suffixed(db_path, "-wal").exists() {
        return Err(VaultError::ConvertFailed);
    }
    opts.hit(FailPoint::Checkpoint)?;

    // 2. the new header, durable before the export starts.
    if let Some(h) = header_to_write {
        write_synced(&header_new(db_path), h, false)?;
        opts.hit(FailPoint::HeaderNew)?;
    }

    // 3. export.
    opts.check_cancel()?;
    let want_counts = row_counts(&src)?;
    let version = user_version(&src)?;
    let attach_key: Zeroizing<String> = match dst_key {
        Some(k) => {
            let p = k.pragma_value();
            // pragma_value is `"x'..'"` (with the double quotes for PRAGMA).
            Zeroizing::new(p.trim_matches('"').to_string())
        }
        None => Zeroizing::new(String::new()),
    };
    let tmp_str = tmp.to_str().ok_or(VaultError::ConvertFailed)?;
    src.execute(
        "ATTACH DATABASE ?1 AS exported KEY ?2",
        rusqlite::params![tmp_str, attach_key.as_str()],
    )
    .map_err(sqlite_err)?;
    src.query_row("SELECT sqlcipher_export('exported')", [], |_| Ok(()))
        .map_err(sqlite_err)?;
    src.execute_batch(&format!("PRAGMA exported.user_version = {version};"))
        .map_err(sqlite_err)?;
    src.execute_batch("DETACH DATABASE exported;")
        .map_err(sqlite_err)?;
    opts.hit(FailPoint::Export)?;

    // 4. verify the copy as a stranger would open it.
    opts.check_cancel()?;
    verify(tmp, dst_key, &want_counts, version)?;
    opts.hit(FailPoint::Verify)?;

    // 5. durable.
    sync_file(tmp)?;
    sync_dir(dir_of(db_path));
    opts.hit(FailPoint::Sync)?;
    Ok(Staged { lock: src })
}

/// Step 6: the commit point. Never with a `-wal` next to the target.
fn commit(db_path: &Path, tmp: &Path, staged: Staged, opts: &ConvertOptions) -> Res<()> {
    if suffixed(db_path, "-wal").exists() || suffixed(db_path, "-journal").exists() {
        return Err(VaultError::ConvertFailed);
    }
    // Windows cannot rename over an open file; the window is documented.
    #[cfg(windows)]
    drop(staged);
    #[cfg(not(windows))]
    let _keep_lock = staged.lock;
    // Last chance to stop; after this line the conversion runs to its end.
    opts.check_cancel()?;
    opts.mark_committed();
    crate::fsutil::replace(tmp, db_path).map_err(io_err)?;
    sync_dir(dir_of(db_path));
    // The old file's side files must not be applied to the new one.
    remove(&suffixed(db_path, "-shm"));
    Ok(())
}

fn require_format(db_path: &Path, want: FileFormat) -> Res<()> {
    match probe_format(db_path).map_err(|_| VaultError::ConvertFailed)? {
        f if f == want => Ok(()),
        FileFormat::Encrypted => Err(VaultError::AlreadyEncrypted),
        FileFormat::Plain => Err(VaultError::NotEncrypted),
        FileFormat::Absent => Err(VaultError::ConvertFailed),
    }
}

/// Encrypt a plain library in place. Slow (Argon2id and a full copy): never on
/// a UI thread. On any error before the commit point the library is exactly as
/// it was; `Busy` means another connection or process holds it.
pub fn enable(db_path: &Path, passphrase: &str, params: KdfParams) -> Res<()> {
    enable_with(db_path, passphrase, params, &ConvertOptions::default())
}

pub fn enable_with(
    db_path: &Path,
    passphrase: &str,
    params: KdfParams,
    opts: &ConvertOptions,
) -> Res<()> {
    let _lock = acquire(db_path)?;
    recover_locked(db_path, opts)?;
    require_format(db_path, FileFormat::Plain)?;
    let (dek, header) = crate::vault::create(passphrase, params)?;
    let key = dek.db_key();
    let tmp = enc_tmp(db_path);
    let r = (|| -> Res<()> {
        let staged = stage(db_path, None, &tmp, Some(&key), Some(&header), opts)?;
        commit(db_path, &tmp, staged, opts)?;
        Ok(())
    })();
    match r {
        Ok(()) => {}
        Err(VaultError::Interrupted) => return Err(VaultError::Interrupted),
        Err(e) => {
            // Before the commit point: undo. (After it `recover` finishes.)
            if probe_format(db_path).ok() == Some(FileFormat::Plain) {
                remove(&tmp);
                remove(&header_new(db_path));
            }
            return Err(e);
        }
    }
    opts.hit(FailPoint::Commit)?;
    if opts.fail_at == Some(FailPoint::LoseHeaderNew) {
        remove(&header_new(db_path));
    }
    // 7. header, then 8. leftovers. The library is encrypted from here on: the
    // header bytes are still in memory, so a lost `.vault.new` is rewritten
    // rather than leaving a database nobody can open.
    publish_header(db_path, &header)?;
    opts.hit(FailPoint::Header)?;
    remove_backups(db_path, FileFormat::Plain);
    opts.hit(FailPoint::Cleanup)?;
    remove_clear_caches(&opts.cache(db_path));
    Ok(())
}

/// After the commit: make `library.vault` the header. Falls back to writing
/// the in-memory bytes again when the staged file cannot be renamed.
fn publish_header(db_path: &Path, header: &[u8; HEADER_LEN]) -> Res<()> {
    let (new, dst) = (header_new(db_path), header_path(db_path));
    if crate::fsutil::replace(&new, &dst).is_ok() {
        sync_dir(dir_of(db_path));
        return Ok(());
    }
    write_synced(&new, header, false)
        .and_then(|()| crate::fsutil::replace(&new, &dst).map_err(io_err))
        .map_err(|_| VaultError::FinishFailed)?;
    sync_dir(dir_of(db_path));
    Ok(())
}

/// Decrypt an encrypted library in place (mirror of `enable`). A wrong
/// passphrase is `WrongPassphrase` and nothing is touched.
pub fn disable(db_path: &Path, passphrase: &str) -> Res<()> {
    disable_with(db_path, passphrase, &ConvertOptions::default())
}

pub fn disable_with(db_path: &Path, passphrase: &str, opts: &ConvertOptions) -> Res<()> {
    let _lock = acquire(db_path)?;
    let dek = unlock_locked(db_path, passphrase, opts)?;
    let key = dek.db_key();
    let tmp = plain_tmp(db_path);
    let marker = disabling_marker(db_path);
    let r = (|| -> Res<()> {
        let staged = stage(db_path, Some(&key), &tmp, None, None, opts)?;
        // Durable intent, strictly before the commit point.
        write_synced(&marker, b"disabling", false)?;
        commit(db_path, &tmp, staged, opts)
    })();
    match r {
        Ok(()) => {}
        Err(VaultError::Interrupted) => return Err(VaultError::Interrupted),
        Err(e) => {
            if probe_format(db_path).ok() == Some(FileFormat::Encrypted) {
                remove(&tmp);
                remove(&marker);
            }
            return Err(e);
        }
    }
    opts.hit(FailPoint::Commit)?;
    remove(&header_path(db_path));
    remove(&header_new(db_path));
    sync_dir(dir_of(db_path));
    opts.hit(FailPoint::Header)?;
    remove_backups(db_path, FileFormat::Encrypted);
    opts.hit(FailPoint::Cleanup)?;
    let _ = fs::remove_dir_all(opts.cache(db_path).join("vault-thumbs"));
    remove(&marker);
    sync_dir(dir_of(db_path));
    Ok(())
}

/// Change the passphrase: only the key envelope is rewritten, the database is
/// not touched. The new header is written to `library.vault.new`, synced and
/// then renamed over `library.vault` (atomic). Copies of the old header stay
/// openable with the old passphrase.
pub fn change_passphrase(db_path: &Path, old: &str, new: &str) -> Res<()> {
    change_passphrase_with(db_path, old, new, &ConvertOptions::default())
}

pub fn change_passphrase_with(
    db_path: &Path,
    old: &str,
    new: &str,
    opts: &ConvertOptions,
) -> Res<()> {
    let _lock = acquire(db_path)?;
    let dek = unlock_locked(db_path, old, opts)?;
    // The key must really open the library before the envelope changes.
    open_keyed(
        db_path,
        Some(&dek.db_key()),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let current = fs::read(header_path(db_path)).map_err(|_| VaultError::HeaderMissing)?;
    let params = validate_header(&current)?;
    let header = wrap(&dek, new, params)?;
    let newp = header_new(db_path);
    if opts.fail_at == Some(FailPoint::TornHeader) {
        write_synced(&newp, &header, true)?;
        return Err(VaultError::Interrupted);
    }
    write_synced(&newp, &header, false)?;
    opts.hit(FailPoint::HeaderNew)?;
    crate::fsutil::replace(&newp, &header_path(db_path)).map_err(io_err)?;
    sync_dir(dir_of(db_path));
    opts.hit(FailPoint::Header)?;
    Ok(())
}

/// Recover from an interrupted conversion. Call before every open of the
/// library. Returns the mode the library is in afterwards. Decided from disk
/// state alone:
///
/// | library.db | leftovers | action |
/// | --- | --- | --- |
/// | plain | `enc-tmp`, `plain-tmp`, `.vault.new`, invalid `.vault` | delete them |
/// | plain | `.disabling` marker | also delete `.vault`, encrypted backups, `vault-thumbs`, marker |
/// | plain | valid `.vault`, no marker | `HeaderOrphan`, nothing deleted |
/// | absent | valid header | `HeaderOrphan`, nothing deleted (`Db::open` refuses too) |
/// | encrypted | `enc-tmp`, `plain-tmp` | delete |
/// | encrypted | no `.vault`, valid `.vault.new` | rename `.new` to `.vault` |
/// | encrypted | no `.vault`, no usable `.new` | `HeaderMissing`, nothing deleted |
/// | encrypted | `.vault` and a damaged `.new` | delete `.new` |
/// | encrypted | `.vault` and a valid `.new` | kept; `unlock_library` keeps the one that opens |
/// | encrypted | header present | delete clear `pre-vN` and clear caches |
pub fn recover(db_path: &Path) -> Res<FileFormat> {
    recover_with(db_path, &ConvertOptions::default())
}

pub fn recover_with(db_path: &Path, opts: &ConvertOptions) -> Res<FileFormat> {
    // A conversion in progress owns every leftover: `Busy`, nothing deleted.
    let _lock = acquire(db_path)?;
    recover_locked(db_path, opts)
}

/// `recover_with` for a caller that already holds the `library.lock`.
fn recover_locked(db_path: &Path, opts: &ConvertOptions) -> Res<FileFormat> {
    let fmt = probe_format(db_path).map_err(|_| VaultError::ConvertFailed)?;
    let (vault, new, marker) = (
        header_path(db_path),
        header_new(db_path),
        disabling_marker(db_path),
    );
    let valid = |p: &Path| {
        fs::read(p)
            .ok()
            .is_some_and(|b| validate_header(&b).is_ok())
    };
    match fmt {
        FileFormat::Absent => {
            // A header with no library: never delete the key envelope, and
            // `Db::open` refuses to create a new library next to it.
            if valid(&vault) || valid(&new) {
                return Err(VaultError::HeaderOrphan);
            }
            remove(&enc_tmp(db_path));
            remove(&plain_tmp(db_path));
            remove(&marker);
        }
        FileFormat::Plain => {
            let disabling = marker.exists();
            // A valid `.vault` next to a clear library is only legitimate
            // after an interrupted disable (marker). Otherwise the encrypted
            // library was lost or replaced: keep everything.
            if !disabling && valid(&vault) {
                return Err(VaultError::HeaderOrphan);
            }
            let had = enc_tmp(db_path).exists()
                || plain_tmp(db_path).exists()
                || vault.exists()
                || new.exists()
                || disabling;
            remove(&enc_tmp(db_path));
            remove(&plain_tmp(db_path));
            remove(&new);
            remove(&vault);
            if had {
                sync_dir(dir_of(db_path));
            }
            if disabling {
                // Interrupted disable after the commit: the encrypted
                // leftovers are unreadable now, finish the cleanup.
                remove_backups(db_path, FileFormat::Encrypted);
                let _ = fs::remove_dir_all(opts.cache(db_path).join("vault-thumbs"));
                remove(&marker);
                sync_dir(dir_of(db_path));
            }
        }
        FileFormat::Encrypted => {
            remove(&enc_tmp(db_path));
            remove(&plain_tmp(db_path));
            // The disable never committed: the intent is void.
            remove(&marker);
            if new.exists() && !valid(&new) {
                remove(&new);
            }
            if vault.exists() && !valid(&vault) && new.exists() {
                // A damaged current header and a good new one: take the new.
                crate::fsutil::replace(&new, &vault).map_err(io_err)?;
            }
            if !vault.exists() {
                if new.exists() {
                    crate::fsutil::replace(&new, &vault).map_err(io_err)?;
                    sync_dir(dir_of(db_path));
                } else {
                    return Err(VaultError::HeaderMissing);
                }
            }
            remove_backups(db_path, FileFormat::Plain);
            remove_clear_caches(&opts.cache(db_path));
        }
    }
    Ok(fmt)
}

/// Recover, then unwrap the key with `passphrase`. When both a header and a
/// fully written `library.vault.new` exist (a passphrase change interrupted
/// between write and rename), the one that opens wins: the current header is
/// tried first; if only the new one opens it is promoted. Slow (Argon2id).
pub fn unlock_library(db_path: &Path, passphrase: &str) -> Res<VaultKey> {
    unlock_library_with(db_path, passphrase, &ConvertOptions::default())
}

pub fn unlock_library_with(
    db_path: &Path,
    passphrase: &str,
    opts: &ConvertOptions,
) -> Res<VaultKey> {
    let _lock = acquire(db_path)?;
    unlock_locked(db_path, passphrase, opts)
}

fn unlock_locked(db_path: &Path, passphrase: &str, opts: &ConvertOptions) -> Res<VaultKey> {
    if recover_locked(db_path, opts)? != FileFormat::Encrypted {
        return Err(VaultError::NotEncrypted);
    }
    let (vault, new) = (header_path(db_path), header_new(db_path));
    let cur = fs::read(&vault).map_err(|_| VaultError::HeaderMissing)?;
    match unlock(&cur, passphrase) {
        Ok(k) => {
            // The change never committed: drop it.
            remove(&new);
            Ok(k)
        }
        Err(VaultError::WrongPassphrase) if new.exists() => {
            let pending = fs::read(&new).map_err(|_| VaultError::HeaderMissing)?;
            let k = unlock(&pending, passphrase)?;
            crate::fsutil::replace(&new, &vault).map_err(io_err)?;
            sync_dir(dir_of(db_path));
            Ok(k)
        }
        Err(e) => Err(e),
    }
}
