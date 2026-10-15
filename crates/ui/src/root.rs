//! The envelope around [`JizzApp`]: lock screen, unlocking, locking.
//!
//! ```text
//!            submit              key ok
//!   Locked ---------> Unlocking ---------> Open(app) --lock--> Locking(app)
//!      ^                  | wrong passphrase                        |
//!      +------------------+            jobs finished, app dropped --+
//! ```
//!
//! The state machine ([`Gate`]) knows nothing about egui or the real app: it
//! is generic over [`Lockable`] so it is tested without a window. Rules it
//! enforces:
//!
//! * `Open` is reachable only from a worker's [`Unlocked`] (a value that can
//!   only be built by unlocking the header *and* opening the database with
//!   the derived key) or at start-up for a library that is not encrypted.
//! * Key derivation (Argon2id) runs on a worker thread, never on the UI
//!   thread.
//! * A lock hides the app at once, cancels its jobs and drops it only when
//!   they have all ended; "Locked" is shown after that.
//! * Nothing here (or in the messages it shows) carries a name, a path or a
//!   passphrase. Messages are fixed, translated sentences.
//!
//! The passphrase lives in a `Zeroizing` buffer with a fixed capacity (no
//! reallocation while typing) and is wiped after each attempt. egui's own
//! text layout caches are outside this guarantee, like RAM in general
//! (see `docs/vault.md`).

use jizzcraft_core::db::{FileFormat, LibraryOpener, probe_format};
use jizzcraft_core::vault::{DbKey, KdfParams, ThumbKeys, VaultError};
use jizzcraft_core::{Db, Error, vault_convert};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Longest passphrase accepted, in characters: the engine's limit (1024). A
/// longer one is refused with a message, never cut.
pub use jizzcraft_core::vault::MAX_PASSPHRASE_CHARS;

/// Capacity of a passphrase buffer: one character more than the limit (so an
/// overlong paste is seen, not silently cut), four bytes each. No
/// reallocation while typing.
pub const MAX_PASSPHRASE_BYTES: usize = (MAX_PASSPHRASE_CHARS + 1) * 4;

/// True when `p` is over the limit (counted in characters, not bytes).
pub fn too_long(p: &str) -> bool {
    p.len() > MAX_PASSPHRASE_CHARS && p.chars().count() > MAX_PASSPHRASE_CHARS
}

/// Forget what egui keeps of the text fields: cursor, selection and the undo
/// history, which holds copies of what was typed (a passphrase included).
pub(crate) fn purge_edit_state(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove_by_type::<egui::text_edit::TextEditState>());
}

/// A fixed, translatable reason a library cannot be unlocked or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    /// The passphrase did not open the header.
    Wrong,
    /// The typed passphrase is over [`MAX_PASSPHRASE_CHARS`]: nothing was
    /// tried and nothing was cut.
    TooLong,
    /// Header missing, damaged, unsupported or orphaned: no prompt helps.
    Header,
    /// Encrypted, but the key file was not finalised: unlocking again
    /// finishes the job. Nothing may be deleted.
    Finish,
    /// Anything else (damaged database, busy, I/O).
    Other,
}

impl Fail {
    pub fn from_vault(e: VaultError) -> Fail {
        match e {
            VaultError::WrongPassphrase => Fail::Wrong,
            VaultError::HeaderMissing
            | VaultError::HeaderCorrupt
            | VaultError::HeaderOrphan
            | VaultError::UnsupportedVersion
            | VaultError::ParamsOutOfBounds => Fail::Header,
            VaultError::FinishFailed => Fail::Finish,
            _ => Fail::Other,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Fail::Wrong => crate::i18n::tr("Wrong passphrase."),
            Fail::TooLong => {
                crate::i18n::tr("The passphrase is too long (1024 characters at most).")
            }
            Fail::Header => crate::i18n::tr(
                "The vault header of this library is missing or damaged. \
                 The library cannot be opened and nothing was changed.",
            ),
            Fail::Finish => crate::i18n::tr(
                "Library encrypted, but finalisation failed. Unlock it again; do not delete any file.",
            ),
            Fail::Other => crate::i18n::tr("The library could not be opened."),
        }
    }
}

/// What the disk says at start-up (after recovery of an interrupted
/// conversion).
#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    /// Not encrypted (or no library yet): open it as before.
    Plain,
    /// Encrypted: show the lock screen, touch nothing else.
    Encrypted,
    /// Header missing or orphaned, conversion unrecoverable: error screen,
    /// and never a new library next to the old one.
    Failed(Fail),
}

/// Recover, then decide. Nothing is opened and no key is asked for.
pub fn startup(db_path: &Path) -> Startup {
    match vault_convert::recover(db_path) {
        Ok(FileFormat::Encrypted) => Startup::Encrypted,
        Ok(_) => Startup::Plain,
        Err(e) => Startup::Failed(Fail::from_vault(e)),
    }
}

/// An opened library with the keys it was opened with. Only
/// [`unlock_blocking`] builds one, so holding it proves the passphrase opened
/// the header and the derived key opened the database.
pub struct Unlocked {
    pub db: Db,
    pub read_only: bool,
    pub opener: LibraryOpener,
    pub thumb_keys: Arc<ThumbKeys>,
}

impl std::fmt::Debug for Unlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unlocked(<redacted>)")
    }
}

/// Header unwrap (Argon2id, slow) and a first open of the database. The data
/// key itself is dropped (and zeroed) here: only the derived database and
/// thumbnail keys leave, behind `Arc`s that the last user frees.
pub fn unlock_blocking(db_path: &Path, passphrase: &str) -> Result<Unlocked, Fail> {
    if too_long(passphrase) {
        return Err(Fail::TooLong);
    }
    let vault = vault_convert::unlock_library(db_path, passphrase).map_err(Fail::from_vault)?;
    let db_key: Arc<DbKey> = Arc::new(vault.db_key());
    let thumb_keys = Arc::new(vault.thumb_keys());
    drop(vault);
    let opener = LibraryOpener::new(db_path, Some(db_key));
    let (db, read_only) = match opener.open() {
        Ok(db) => (db, false),
        Err(Error::FutureSchema { .. }) => {
            (opener.open_read_only().map_err(|_| Fail::Other)?, true)
        }
        Err(Error::Vault(e)) => return Err(Fail::from_vault(e)),
        Err(_) => return Err(Fail::Other),
    };
    Ok(Unlocked {
        db,
        read_only,
        opener,
        thumb_keys,
    })
}

type UnlockResult = Result<Unlocked, Fail>;

/// Run [`unlock_blocking`] on a worker thread. `wake` is called when it ends.
pub fn spawn_unlock(
    db_path: PathBuf,
    passphrase: Zeroizing<String>,
    wake: Box<dyn FnOnce() + Send>,
) -> Receiver<UnlockResult> {
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("jizzcraft-unlock".into())
        .spawn(move || {
            let r = unlock_blocking(&db_path, &passphrase);
            drop(passphrase);
            let _ = tx.send(r);
            wake();
        });
    if spawned.is_err() {
        // No thread: the receiver reports a disconnect, shown as `Other`.
    }
    rx
}

/// Soft anti-spam delay after `failures` wrong passphrases in this process.
/// Not persistent: it protects against a held key, not against an attacker
/// (who has the file and Argon2id, not this window).
pub fn backoff(failures: u32) -> Duration {
    if failures < 3 {
        Duration::ZERO
    } else {
        Duration::from_secs((1u64 << (failures - 3).min(5)).min(30))
    }
}

/// What a hosted app must offer for the gate to lock it.
pub trait Lockable {
    /// True when the app was opened with a vault key (a plain library cannot
    /// be locked).
    fn vault_active(&self) -> bool;
    /// Hide-state work: cancel jobs, release the player and every handle
    /// that could keep a decrypted view alive. Must not block.
    fn begin_lock(&mut self);
    /// Every job that holds a key has ended.
    fn jobs_finished(&self) -> bool;
    /// Every background decode thread (thumbnails, previews) has returned and
    /// let go of its cipher. Until then nothing may clean the cache, convert
    /// or drop the keys: a worker could still write a clear file.
    fn workers_finished(&self) -> bool {
        true
    }
    /// The inactivity delay elapsed (a playing video counts as activity).
    /// Called by the gate before the app is drawn, and from a timer when no
    /// frame is drawn at all.
    fn idle_due(&mut self, _now: Instant) -> bool {
        false
    }
    /// Input happened: restart the inactivity clock.
    fn touch(&mut self, _now: Instant) {}
    /// When the inactivity lock falls due if nothing happens (`None`: never).
    fn idle_deadline(&self, _now: Instant) -> Option<Instant> {
        None
    }
    /// The user asked for a lock since the last call.
    fn take_lock_request(&mut self) -> bool;
    /// A conversion (encrypt, decrypt, change passphrase) was asked for since
    /// the last call. The app has already refused it while jobs run.
    fn take_convert_request(&mut self) -> Option<ConvertRequest> {
        None
    }
    /// The keys the library is open with, to reopen it after a conversion
    /// that did not change them.
    fn keys(&self) -> Option<Keep> {
        None
    }
    /// The conversion ended and the library is open again: the one toast.
    fn conversion_finished(&mut self, _kind: ConvertKind, _outcome: ConvertOutcome) {}
}

/// Keys kept across a conversion so a refused one reopens without asking.
pub type Keep = (Arc<DbKey>, Arc<ThumbKeys>);

/// What a conversion does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvertKind {
    Enable,
    Disable,
    Change,
}

/// How a conversion ended. Fixed values, translated by the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvertOutcome {
    Done,
    /// Cancelled before the commit point: nothing changed.
    Cancelled,
    WrongPassphrase,
    /// Another program holds the library: nothing changed.
    Busy,
    /// Encrypted, but the key file was not finalised: the library is
    /// encrypted on disk, unlock it again. Never "intact".
    FinishFailed,
    Failed,
}

impl ConvertOutcome {
    pub fn from_vault(e: VaultError) -> ConvertOutcome {
        match e {
            VaultError::Cancelled => ConvertOutcome::Cancelled,
            VaultError::WrongPassphrase => ConvertOutcome::WrongPassphrase,
            VaultError::Busy => ConvertOutcome::Busy,
            VaultError::FinishFailed => ConvertOutcome::FinishFailed,
            _ => ConvertOutcome::Failed,
        }
    }
}

/// Shortest accepted new passphrase, in characters (same as the CLI).
pub const MIN_PASSPHRASE_CHARS: usize = 12;

/// A passphrase buffer with a fixed capacity: no reallocation while typing,
/// wiped on drop.
pub fn secret_buf() -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(MAX_PASSPHRASE_BYTES))
}

/// A conversion asked for from Settings. The secrets live in `Zeroizing`
/// buffers and are moved, never copied, to the worker.
pub struct ConvertRequest {
    pub kind: ConvertKind,
    /// The current passphrase (to enable: the new one).
    pub pass: Zeroizing<String>,
    /// The new passphrase of a change.
    pub new_pass: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for ConvertRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConvertRequest({:?}, <redacted>)", self.kind)
    }
}

/// The library opened again after a conversion.
pub struct Reopened {
    pub db: Db,
    pub read_only: bool,
    pub opener: LibraryOpener,
    pub thumb_keys: Option<Arc<ThumbKeys>>,
}

/// What the conversion worker answers. `lib` is the library opened again in
/// whatever mode the disk is in now.
pub struct Converted {
    pub outcome: ConvertOutcome,
    pub lib: Result<Reopened, Fail>,
}

fn open_opener(opener: &LibraryOpener) -> Result<(Db, bool), Fail> {
    match opener.open() {
        Ok(db) => Ok((db, false)),
        Err(Error::FutureSchema { .. }) => opener
            .open_read_only()
            .map(|d| (d, true))
            .map_err(|_| Fail::Other),
        Err(Error::Vault(e)) => Err(Fail::from_vault(e)),
        Err(_) => Err(Fail::Other),
    }
}

fn reopen_plain(db_path: &Path) -> Result<Reopened, Fail> {
    let opener = LibraryOpener::new(db_path, None);
    let (db, read_only) = open_opener(&opener)?;
    Ok(Reopened {
        db,
        read_only,
        opener,
        thumb_keys: None,
    })
}

fn reopen_keyed(db_path: &Path, keep: Option<Keep>) -> Result<Reopened, Fail> {
    let (k, t) = keep.ok_or(Fail::Other)?;
    let opener = LibraryOpener::new(db_path, Some(k));
    let (db, read_only) = open_opener(&opener)?;
    Ok(Reopened {
        db,
        read_only,
        opener,
        thumb_keys: Some(t),
    })
}

fn reopen_unlocked(db_path: &Path, pass: &str) -> Result<Reopened, Fail> {
    let u = unlock_blocking(db_path, pass)?;
    Ok(Reopened {
        db: u.db,
        read_only: u.read_only,
        opener: u.opener,
        thumb_keys: Some(u.thumb_keys),
    })
}

/// Run one conversion and reopen the library. Slow (Argon2id, a full copy):
/// workers only. The library must not be open in this process. `keep` are the
/// keys of an encrypted library, used to reopen it when the conversion is
/// refused (wrong passphrase, busy, cancelled) without asking again.
pub fn convert_blocking(
    db_path: &Path,
    req: ConvertRequest,
    keep: Option<Keep>,
    cancel: &Arc<AtomicBool>,
    committed: &Arc<AtomicBool>,
    params: KdfParams,
) -> Converted {
    let opts = vault_convert::ConvertOptions {
        cancel: Some(Arc::clone(cancel)),
        committed: Some(Arc::clone(committed)),
        ..Default::default()
    };
    let short = |p: &str| p.chars().count() < MIN_PASSPHRASE_CHARS;
    let long = too_long(&req.pass) || req.new_pass.as_deref().is_some_and(|n| too_long(n));
    let result = match (req.kind, req.new_pass.as_ref()) {
        // Refused, never cut: a longer passphrase cannot be typed again by
        // someone who only knows its first characters.
        _ if long => Err(VaultError::EmptyPassphrase),
        (ConvertKind::Enable, _) if short(&req.pass) => Err(VaultError::EmptyPassphrase),
        (ConvertKind::Change, Some(n)) if short(n) => Err(VaultError::EmptyPassphrase),
        (ConvertKind::Change, None) => Err(VaultError::EmptyPassphrase),
        (ConvertKind::Enable, _) => vault_convert::enable_with(db_path, &req.pass, params, &opts),
        (ConvertKind::Disable, _) => vault_convert::disable_with(db_path, &req.pass, &opts),
        (ConvertKind::Change, Some(n)) => {
            vault_convert::change_passphrase_with(db_path, &req.pass, n, &opts)
        }
    };
    let (outcome, ok) = match result {
        Ok(()) => (ConvertOutcome::Done, true),
        Err(e) => (ConvertOutcome::from_vault(e), false),
    };
    // The disk decides which way the library opens now.
    let lib = match (req.kind, ok) {
        // Encrypted on disk, key not finalised: the lock screen, not a reopen.
        _ if outcome == ConvertOutcome::FinishFailed => Err(Fail::Finish),
        (ConvertKind::Enable, true) => reopen_unlocked(db_path, &req.pass),
        (ConvertKind::Enable, false) | (ConvertKind::Disable, true) => reopen_plain(db_path),
        (ConvertKind::Change, true) => {
            let n = req.new_pass.as_deref().map_or("", |n| n.as_str());
            reopen_unlocked(db_path, n)
        }
        (ConvertKind::Disable, false) | (ConvertKind::Change, false) => reopen_keyed(db_path, keep),
    };
    Converted { outcome, lib }
}

type ConvertWorker = Receiver<Converted>;

/// Run [`convert_blocking`] on a worker thread. `wake` is called when it ends.
pub fn spawn_convert(
    db_path: PathBuf,
    req: ConvertRequest,
    keep: Option<Keep>,
    cancel: Arc<AtomicBool>,
    committed: Arc<AtomicBool>,
    wake: Box<dyn FnOnce() + Send>,
) -> ConvertWorker {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("jizzcraft-convert".into())
        .spawn(move || {
            let r = convert_blocking(
                &db_path,
                req,
                keep,
                &cancel,
                &committed,
                KdfParams::default(),
            );
            let _ = tx.send(r);
            wake();
        });
    rx
}

enum ConvPhase<A> {
    /// The app is closing: jobs finish, then it is dropped.
    Quiesce {
        app: Box<A>,
        req: ConvertRequest,
        keep: Option<Keep>,
    },
    Running(ConvertWorker),
}

/// A conversion in progress. The library is closed in this process while it
/// runs: no other job can start.
pub struct Converting<A> {
    phase: ConvPhase<A>,
    kind: ConvertKind,
    cancel: Arc<AtomicBool>,
    committed: Arc<AtomicBool>,
}

impl<A> Converting<A> {
    pub fn kind(&self) -> ConvertKind {
        self.kind
    }

    /// Past the commit point: cancelling is no longer possible.
    pub fn committed(&self) -> bool {
        self.committed.load(Ordering::SeqCst)
    }

    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// The lock screen's own state: the passphrase buffer and the soft limit.
pub struct LockScreen {
    pub(crate) buf: Zeroizing<String>,
    pub(crate) error: Option<Fail>,
    pub(crate) failures: u32,
    pub(crate) not_before: Option<Instant>,
    pub(crate) focus: bool,
}

impl LockScreen {
    pub fn new() -> LockScreen {
        LockScreen {
            buf: Zeroizing::new(String::with_capacity(MAX_PASSPHRASE_BYTES)),
            error: None,
            failures: 0,
            not_before: None,
            focus: true,
        }
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn error(&self) -> Option<Fail> {
        self.error
    }

    /// Seconds left before another attempt is accepted (0 when none).
    pub fn wait_left(&self, now: Instant) -> u64 {
        self.not_before.map_or(0, |t| {
            t.saturating_duration_since(now).as_secs_f32().ceil() as u64
        })
    }

    /// Wipe what was typed and keep the capacity (no reallocation later).
    pub fn wipe(&mut self) {
        self.buf.zeroize();
    }

    /// Take the typed passphrase out, wiping nothing twice: the returned
    /// buffer owns the only copy, and a fresh fixed-capacity one is ready.
    fn take(&mut self) -> Zeroizing<String> {
        let fresh = Zeroizing::new(String::with_capacity(MAX_PASSPHRASE_BYTES));
        let mut old = std::mem::replace(&mut self.buf, fresh);
        Zeroizing::new(std::mem::take(&mut *old))
    }
}

impl Default for LockScreen {
    fn default() -> Self {
        LockScreen::new()
    }
}

pub struct Unlocking {
    rx: Receiver<UnlockResult>,
    failures: u32,
}

pub enum State<A> {
    Locked(LockScreen),
    Unlocking(Unlocking),
    Open(Box<A>),
    Locking(Box<A>),
    /// Encrypting, decrypting or changing the passphrase (the app is closed).
    Converting(Converting<A>),
    /// Unrecoverable on this start: error text only, no prompt.
    Failed(Fail),
}

impl<A> State<A> {
    pub fn name(&self) -> &'static str {
        match self {
            State::Locked(_) => "locked",
            State::Unlocking(_) => "unlocking",
            State::Open(_) => "open",
            State::Locking(_) => "locking",
            State::Converting(_) => "converting",
            State::Failed(_) => "failed",
        }
    }
}

/// The state machine. `A` is the hosted app.
pub struct Gate<A> {
    pub state: State<A>,
    db_path: PathBuf,
    /// The user asked to close the window during a conversion and has not
    /// answered the warning yet.
    close_asked: bool,
    /// The user accepted to interrupt the conversion: the next close goes
    /// through.
    close_confirmed: bool,
}

/// What to do with a window close request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseVerdict {
    /// Let the window close.
    Allow,
    /// Keep the window open (a conversion is running and the warning is up).
    Hold,
}

impl<A: Lockable> Gate<A> {
    pub fn locked(db_path: PathBuf) -> Self {
        Gate {
            state: State::Locked(LockScreen::new()),
            db_path,
            close_asked: false,
            close_confirmed: false,
        }
    }

    pub fn failed(db_path: PathBuf, why: Fail) -> Self {
        Gate {
            state: State::Failed(why),
            db_path,
            close_asked: false,
            close_confirmed: false,
        }
    }

    /// A library that is not encrypted: no key was ever involved.
    pub fn open_plain(db_path: PathBuf, app: A) -> Self {
        Gate {
            state: State::Open(Box::new(app)),
            db_path,
            close_asked: false,
            close_confirmed: false,
        }
    }

    /// Start unlocking with `passphrase`. Refused (the buffer is wiped) when
    /// not locked, empty, or inside the soft delay. Returns whether a worker
    /// was started.
    pub fn submit(
        &mut self,
        passphrase: Zeroizing<String>,
        now: Instant,
        spawn: impl FnOnce(PathBuf, Zeroizing<String>) -> Receiver<UnlockResult>,
    ) -> bool {
        let State::Locked(ls) = &mut self.state else {
            return false;
        };
        if passphrase.is_empty() || ls.wait_left(now) > 0 {
            return false;
        }
        let failures = ls.failures;
        let rx = spawn(self.db_path.clone(), passphrase);
        self.state = State::Unlocking(Unlocking { rx, failures });
        true
    }

    /// Collect the worker's answer. `build` turns the proof of unlocking into
    /// the hosted app (it may fail: the library is then locked again).
    pub fn poll_unlock(&mut self, now: Instant, build: impl FnOnce(Unlocked) -> Result<A, String>) {
        let State::Unlocking(u) = &self.state else {
            return;
        };
        let answer = match u.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(Fail::Other),
        };
        let failures = u.failures;
        match answer {
            Ok(unlocked) => match build(unlocked) {
                Ok(app) => self.state = State::Open(Box::new(app)),
                Err(_) => self.relock(failures, Fail::Other, now),
            },
            Err(Fail::Header) => self.state = State::Failed(Fail::Header),
            Err(f) => {
                let failures = if f == Fail::Wrong {
                    failures + 1
                } else {
                    failures
                };
                self.relock(failures, f, now);
            }
        }
    }

    fn relock(&mut self, failures: u32, why: Fail, now: Instant) {
        let mut ls = LockScreen::new();
        ls.failures = failures;
        ls.error = Some(why);
        let d = backoff(failures);
        ls.not_before = (!d.is_zero()).then(|| now + d);
        self.state = State::Locked(ls);
    }

    /// Lock an open, encrypted app: hide it, cancel its jobs. Anything else
    /// (a plain library, a state other than `Open`) is ignored.
    pub fn request_lock(&mut self) -> bool {
        let State::Open(app) = &mut self.state else {
            return false;
        };
        if !app.vault_active() {
            return false;
        }
        app.begin_lock();
        if let State::Open(app) =
            std::mem::replace(&mut self.state, State::Locked(LockScreen::new()))
        {
            self.state = State::Locking(app);
        }
        true
    }

    /// The inactivity deadline, tested against `now`. Call it before the app
    /// is drawn: when due the app is hidden at once (`Locking`) and no frame of
    /// its content is built. Returns whether a lock started.
    ///
    /// Order matters: test the deadline first, then [`Gate::touch`] for this
    /// frame's input, so the click that wakes a window after the deadline
    /// cannot rescue the library.
    pub fn tick(&mut self, now: Instant) -> bool {
        let due = match &mut self.state {
            State::Open(app) => app.vault_active() && app.idle_due(now),
            _ => false,
        };
        due && self.request_lock()
    }

    /// Input happened in the open app.
    pub fn touch(&mut self, now: Instant) {
        if let State::Open(app) = &mut self.state {
            app.touch(now);
        }
    }

    /// When the open, encrypted app falls idle (what the timer sleeps until).
    pub fn idle_deadline(&self, now: Instant) -> Option<Instant> {
        match &self.state {
            State::Open(app) if app.vault_active() => app.idle_deadline(now),
            _ => None,
        }
    }

    /// Start a lock or a conversion the open app asked for. Returns whether
    /// one started.
    pub fn take_requests(&mut self) -> bool {
        let lock = match &mut self.state {
            State::Open(app) => app.take_lock_request(),
            _ => return false,
        };
        if lock {
            self.request_lock()
        } else {
            self.start_convert()
        }
    }

    /// Take a pending conversion from an open app and start closing it.
    /// Returns whether one started. Needs an open app; the app itself has
    /// refused the request while jobs run.
    pub fn start_convert(&mut self) -> bool {
        let State::Open(app) = &mut self.state else {
            return false;
        };
        let Some(req) = app.take_convert_request() else {
            return false;
        };
        let ok = match req.kind {
            ConvertKind::Enable => !app.vault_active(),
            ConvertKind::Disable | ConvertKind::Change => app.vault_active(),
        };
        if !ok {
            return false;
        }
        let keep = app.keys();
        app.begin_lock();
        let kind = req.kind;
        if let State::Open(app) =
            std::mem::replace(&mut self.state, State::Locked(LockScreen::new()))
        {
            self.state = State::Converting(Converting {
                phase: ConvPhase::Quiesce { app, req, keep },
                kind,
                cancel: Arc::new(AtomicBool::new(false)),
                committed: Arc::new(AtomicBool::new(false)),
            });
        }
        true
    }

    /// A conversion owns the library right now (closing the app, running, or
    /// finishing). Closing the process then interrupts it.
    pub fn converting(&self) -> bool {
        matches!(self.state, State::Converting(_))
    }

    /// The window was asked to close. During a conversion the close is held
    /// and a warning is raised; once the user confirmed it, it goes through.
    /// The engine is crash-safe: an interrupted conversion is finished or
    /// undone by `recover` at the next start, and the user starts it again.
    pub fn on_close_request(&mut self) -> CloseVerdict {
        if !self.converting() || self.close_confirmed {
            return CloseVerdict::Allow;
        }
        self.close_asked = true;
        CloseVerdict::Hold
    }

    /// The close warning has to be shown.
    pub fn close_warning(&self) -> bool {
        self.close_asked && self.converting()
    }

    /// The user answered the close warning. `close` true: interrupt the
    /// conversion, the caller then asks the window to close again.
    pub fn answer_close(&mut self, close: bool) {
        self.close_asked = false;
        self.close_confirmed = close;
    }

    /// Ask the running conversion to stop. Only possible before its commit
    /// point; returns whether the request was recorded.
    pub fn cancel_convert(&mut self) -> bool {
        match &self.state {
            State::Converting(c) if !c.committed() => {
                c.cancel.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    /// Advance a conversion: when the app's jobs ended it is dropped (its
    /// connection closes) and the worker starts; when the worker answered the
    /// library is open again with the one toast.
    pub fn poll_convert(
        &mut self,
        spawn: impl FnOnce(
            PathBuf,
            ConvertRequest,
            Option<Keep>,
            Arc<AtomicBool>,
            Arc<AtomicBool>,
        ) -> ConvertWorker,
        build: impl FnOnce(Reopened) -> Result<A, String>,
    ) {
        let State::Converting(c) = &self.state else {
            return;
        };
        let quiescing = match &c.phase {
            ConvPhase::Quiesce { app, .. } => Some(app.jobs_finished() && app.workers_finished()),
            ConvPhase::Running(_) => None,
        };
        if let Some(ready) = quiescing {
            if !ready {
                return;
            }
            let State::Converting(c) =
                std::mem::replace(&mut self.state, State::Failed(Fail::Other))
            else {
                return;
            };
            let Converting {
                phase,
                kind,
                cancel,
                committed,
            } = c;
            let ConvPhase::Quiesce { app, req, keep } = phase else {
                return;
            };
            // Closes the database and releases this app's key references.
            drop(app);
            let rx = spawn(
                self.db_path.clone(),
                req,
                keep,
                Arc::clone(&cancel),
                Arc::clone(&committed),
            );
            self.state = State::Converting(Converting {
                phase: ConvPhase::Running(rx),
                kind,
                cancel,
                committed,
            });
            return;
        }
        let ConvPhase::Running(rx) = &c.phase else {
            return;
        };
        let kind = c.kind;
        let answer = match rx.try_recv() {
            Ok(a) => a,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Converted {
                outcome: ConvertOutcome::Failed,
                lib: Err(Fail::Other),
            },
        };
        let outcome = answer.outcome;
        let next = match answer.lib {
            Ok(lib) => match build(lib) {
                Ok(mut app) => {
                    app.conversion_finished(kind, outcome);
                    State::Open(Box::new(app))
                }
                Err(_) => self.after_failed_reopen(),
            },
            Err(Fail::Header) => State::Failed(Fail::Header),
            Err(Fail::Finish) => {
                let mut ls = LockScreen::new();
                ls.error = Some(Fail::Finish);
                State::Locked(ls)
            }
            Err(_) => self.after_failed_reopen(),
        };
        self.state = next;
    }

    /// The library could not be opened again: encrypted on disk means the
    /// lock screen (the passphrase may well work), anything else an error.
    fn after_failed_reopen(&self) -> State<A> {
        match probe_format(&self.db_path) {
            Ok(FileFormat::Encrypted) => State::Locked(LockScreen::new()),
            _ => State::Failed(Fail::Other),
        }
    }

    /// Drop the app once its jobs and decode workers have ended; only then
    /// show "Locked".
    pub fn poll_lock(&mut self) {
        let State::Locking(app) = &self.state else {
            return;
        };
        if app.jobs_finished() && app.workers_finished() {
            // Dropping closes the database, frees textures and the
            // thumbnail service and releases the last key references.
            self.state = State::Locked(LockScreen::new());
        }
    }
}

/// Wakes the UI at a deadline from its own thread, so the inactivity lock
/// does not depend on the window being drawn (eframe throttles hidden ones).
/// The wake-up only asks for a pass (`request_repaint`, which also runs
/// `App::logic` for a minimized window); the lock itself is decided there.
pub struct IdleTimer {
    shared: Arc<TimerShared>,
    handle: Option<std::thread::JoinHandle<()>>,
}

struct TimerShared {
    state: std::sync::Mutex<TimerState>,
    cv: std::sync::Condvar,
}

#[derive(Default)]
struct TimerState {
    deadline: Option<Instant>,
    /// The deadline already announced: not announced twice.
    fired: Option<Instant>,
    stop: bool,
}

impl IdleTimer {
    /// `fire` runs on the timer thread once per deadline.
    pub fn new(fire: Box<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(TimerShared {
            state: std::sync::Mutex::new(TimerState::default()),
            cv: std::sync::Condvar::new(),
        });
        let s = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("jizzcraft-idle".into())
            .spawn(move || {
                let mut st = s.state.lock().unwrap();
                loop {
                    if st.stop {
                        return;
                    }
                    match st.deadline {
                        Some(d) if st.fired != Some(d) => {
                            let now = Instant::now();
                            if now >= d {
                                st.fired = Some(d);
                                drop(st);
                                fire();
                                st = s.state.lock().unwrap();
                            } else {
                                st = s.cv.wait_timeout(st, d - now).unwrap().0;
                            }
                        }
                        _ => st = s.cv.wait(st).unwrap(),
                    }
                }
            })
            .ok();
        IdleTimer { shared, handle }
    }

    /// The instant the lock falls due, or `None` (nothing to wait for).
    pub fn set(&self, deadline: Option<Instant>) {
        let mut st = self.shared.state.lock().unwrap();
        if st.deadline != deadline {
            st.deadline = deadline;
            drop(st);
            self.shared.cv.notify_all();
        }
    }
}

impl Drop for IdleTimer {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stop = true;
        self.shared.cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---- eframe side -----------------------------------------------------------

use crate::app::{JizzApp, Library, Options};
use crate::theme::{self, BG, FAINT, MUTED, S};
use crate::widgets;
use egui::{FontId, Rect, UiBuilder, vec2};

const VIEWER_PNG: &[u8] = include_bytes!("../../../assets/icons/viewer-256.png");

/// What eframe runs: the lock envelope around the real app.
pub struct Root {
    gate: Gate<JizzApp>,
    opts: Options,
    render_state: Option<egui_wgpu::RenderState>,
    /// The neutral window icon, sent once while locked.
    icon: Arc<egui::IconData>,
    neutral_sent: bool,
    /// Wakes the app at the inactivity deadline even when no frame is drawn.
    timer: IdleTimer,
    /// State of the previous pass, to purge text-edit history on a change.
    last_state: &'static str,
}

impl Root {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Result<Self, String> {
        theme::apply(&cc.egui_ctx);
        // The lock screen is translated like the rest: the language comes
        // from the clear config, as it does for the app.
        JizzApp::apply_language(&opts, opts.config.as_ref());
        let db_path = opts
            .db_path
            .clone()
            .unwrap_or_else(jizzcraft_core::paths::db_path);
        let gate = match startup(&db_path) {
            Startup::Encrypted => Gate::locked(db_path),
            Startup::Failed(why) => Gate::failed(db_path, why),
            Startup::Plain => {
                let lib = Library::open_plain(&db_path)?;
                let app = JizzApp::open(
                    &cc.egui_ctx,
                    cc.wgpu_render_state.clone(),
                    opts.clone(),
                    lib,
                )?;
                Gate::open_plain(db_path, app)
            }
        };
        Ok(Root {
            gate,
            opts,
            render_state: cc.wgpu_render_state.clone(),
            icon: crate::app::window_icon(VIEWER_PNG),
            neutral_sent: false,
            timer: IdleTimer::new({
                let ctx = cc.egui_ctx.clone();
                Box::new(move || ctx.request_repaint())
            }),
            last_state: "",
        })
    }

    /// Neutral title and icon for every screen that is not the app.
    fn neutral_window(&mut self, ctx: &egui::Context) {
        if !self.neutral_sent {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title("Viewer".into()));
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(self.icon.clone())));
            self.neutral_sent = true;
        }
    }

    fn screen(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let w = 420.0_f32.min(rect.width() - 48.0).max(120.0);
                let col = Rect::from_center_size(rect.center(), vec2(w, 220.0));
                ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
                    ui.spacing_mut().item_spacing = vec2(S * 2.0, S * 2.0);
                    body(ui);
                });
            });
    }

    fn text_screen(ui: &mut egui::Ui, title: &str, line: Option<&str>) {
        Self::screen(ui, |ui| {
            widgets::mono_label(ui, title, MUTED);
            if let Some(l) = line {
                ui.add(egui::Label::new(egui::RichText::new(l).size(13.0).color(FAINT)).wrap());
            }
        });
    }
}

impl Root {
    /// Everything that moves the gate and draws nothing: the inactivity
    /// deadline first (before any input of this frame can rescue the library),
    /// then the workers' answers, then what the app asked for. Safe to call
    /// twice per frame; `logic` runs it when the window is hidden too.
    fn advance(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.gate.tick(now);
        let ctx2 = ctx.clone();
        let (opts, rs) = (&self.opts, &self.render_state);
        self.gate.poll_unlock(now, |u| {
            let lib = Library::from_unlocked(u);
            JizzApp::open(&ctx2, rs.clone(), opts.clone(), lib)
        });
        self.gate.poll_lock();
        {
            let wake = ctx.clone();
            self.gate.poll_convert(
                |path, req, keep, cancel, committed| {
                    spawn_convert(
                        path,
                        req,
                        keep,
                        cancel,
                        committed,
                        Box::new(move || wake.request_repaint()),
                    )
                },
                |r| {
                    let lib = Library::from_reopened(r);
                    JizzApp::open(&ctx2, rs.clone(), opts.clone(), lib)
                },
            );
        }
        if matches!(self.gate.state, State::Open(_)) {
            // Start-up work (`--import`, `--cmd`, a pre-opened item, bench and
            // screenshot) runs once, not again after a lock and an unlock.
            self.opts.import = None;
            self.opts.commands.clear();
            self.opts.open_index = None;
            self.opts.bench = None;
            self.opts.screenshot = None;
            // A fresh app sets its own title; the neutral one is due again
            // after the next lock.
            self.neutral_sent = false;
        }
        // A lock or a conversion asked for outside a frame (a command, the
        // boss key from `logic`): started here, before the app is drawn.
        if self.gate.take_requests() {
            ctx.request_repaint();
        }
        self.sync(ctx, now);
    }

    /// After every gate move: purge what egui remembers of typed text when
    /// the screen changes to a locked one, and tell the timer the deadline.
    fn sync(&mut self, ctx: &egui::Context, now: Instant) {
        let name = self.gate.state.name();
        if name != self.last_state {
            if matches!(name, "locking" | "locked" | "unlocking" | "converting") {
                purge_edit_state(ctx);
            }
            self.last_state = name;
        }
        self.timer.set(self.gate.idle_deadline(now));
    }

    /// Hide the app for the frame being built: a full-window fill above every
    /// layer, so the last buffer carries no content of the library.
    fn cover(ctx: &egui::Context) {
        let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::new("vault-cover"));
        ctx.layer_painter(layer)
            .rect_filled(ctx.content_rect(), 0.0, BG);
    }
}

impl eframe::App for Root {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Also called while the window is minimized or hidden, when `ui` is
        // not: the inactivity lock must not depend on being drawn.
        self.advance(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();

        // A close during a conversion is held until the user confirms.
        if ctx.input(|i| i.viewport().close_requested())
            && self.gate.on_close_request() == CloseVerdict::Hold
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        // Transitions first, so this frame already draws the new state. The
        // deadline is tested before this frame's input is counted.
        self.advance(&ctx);
        if matches!(self.gate.state, State::Open(_)) {
            let input =
                ctx.input(|i| !i.events.is_empty() || i.pointer.delta() != egui::Vec2::ZERO);
            if input {
                self.gate.touch(now);
            }
        }

        match &mut self.gate.state {
            State::Open(app) => {
                eframe::App::ui(&mut **app, ui, frame);
                // Asked for while drawing (menu, shortcut, boss key): the
                // library is hidden before this frame is presented.
                if self.gate.take_requests() {
                    Self::cover(&ctx);
                    ctx.request_repaint();
                }
                self.sync(&ctx, now);
            }
            State::Converting(c) => {
                let (kind, committed, cancelling) = (c.kind(), c.committed(), c.cancelling());
                self.neutral_window(&ctx);
                let mut cancel = false;
                Self::screen(ui, |ui| {
                    let title = match kind {
                        ConvertKind::Enable => crate::i18n::tr("Encrypting…"),
                        ConvertKind::Disable => crate::i18n::tr("Decrypting…"),
                        ConvertKind::Change => crate::i18n::tr("Changing passphrase…"),
                    };
                    widgets::mono_label(ui, title, MUTED);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(crate::i18n::tr(
                                "Keep this window open until it finishes.",
                            ))
                            .size(13.0)
                            .color(FAINT),
                        )
                        .wrap(),
                    );
                    if cancelling && !committed {
                        ui.label(
                            egui::RichText::new(crate::i18n::tr("Cancelling…"))
                                .size(13.0)
                                .color(FAINT),
                        );
                    } else if !committed
                        && widgets::button_outline(ui, crate::i18n::tr("Cancel")).clicked()
                    {
                        cancel = true;
                    }
                });
                if cancel {
                    self.gate.cancel_convert();
                }
                if self.gate.close_warning() {
                    let mut answer = None;
                    egui::Window::new(crate::i18n::tr("Conversion in progress"))
                        .id(egui::Id::new("vault-close-warning"))
                        .collapsible(false)
                        .resizable(false)
                        .default_width(420.0)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            ui.add(
                                egui::Label::new(crate::i18n::tr(
                                    "A conversion is in progress. Closing interrupts the \
                                     operation. This is safe, but you will have to start it again.",
                                ))
                                .wrap(),
                            );
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if widgets::button_solid(ui, crate::i18n::tr("Close anyway"))
                                    .clicked()
                                {
                                    answer = Some(true);
                                }
                                if widgets::button_outline(ui, crate::i18n::tr("Keep waiting"))
                                    .clicked()
                                {
                                    answer = Some(false);
                                }
                            });
                        });
                    if let Some(close) = answer {
                        self.gate.answer_close(close);
                        if close {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            State::Locking(_) => {
                self.neutral_window(&ctx);
                Self::text_screen(ui, crate::i18n::tr("Locking…"), None);
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            State::Unlocking(_) => {
                self.neutral_window(&ctx);
                Self::text_screen(ui, crate::i18n::tr("Unlocking…"), None);
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            State::Failed(why) => {
                let why = *why;
                self.neutral_window(&ctx);
                Self::text_screen(ui, crate::i18n::tr("Locked"), Some(why.text()));
            }
            State::Locked(_) => {
                self.neutral_window(&ctx);
                if let Some(pass) = lock_screen_ui(ui, &mut self.gate, now) {
                    let wake_ctx = ctx.clone();
                    self.gate.submit(pass, now, |path, p| {
                        spawn_unlock(path, p, Box::new(move || wake_ctx.request_repaint()))
                    });
                    ctx.request_repaint();
                }
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }
}

/// Draw the prompt. Returns the passphrase when the user submitted it.
fn lock_screen_ui(
    ui: &mut egui::Ui,
    gate: &mut Gate<JizzApp>,
    now: Instant,
) -> Option<Zeroizing<String>> {
    let State::Locked(ls) = &mut gate.state else {
        return None;
    };
    let mut submitted = None;
    let wait = ls.wait_left(now);
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter().clone();
            widgets::corner_crosshairs(&p, rect, 16.0);
            let w = 420.0_f32.min(rect.width() - 48.0).max(120.0);
            let col = Rect::from_center_size(rect.center(), vec2(w, 220.0));
            ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
                ui.spacing_mut().item_spacing = vec2(S * 2.0, S * 2.0);
                ui.label(
                    egui::RichText::new(crate::i18n::tr("Locked"))
                        .font(FontId::new(32.0, theme::title_family()))
                        .color(egui::Color32::WHITE),
                );
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut *ls.buf)
                        .password(true)
                        .char_limit(MAX_PASSPHRASE_CHARS + 1)
                        .hint_text(crate::i18n::tr("Passphrase"))
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut ls.focus) {
                    resp.request_focus();
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let mut go = enter;
                ui.horizontal(|ui| {
                    if widgets::button_solid(ui, crate::i18n::tr("Unlock")).clicked() {
                        go = true;
                    }
                    if widgets::button_outline(ui, crate::i18n::tr("Exit")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                // Over the limit: said at once, and nothing is cut.
                let over = too_long(&ls.buf).then_some(Fail::TooLong);
                if let Some(e) = over.or(ls.error) {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(e.text())
                                .size(13.0)
                                .color(theme::DANGER),
                        )
                        .wrap(),
                    );
                }
                if wait > 0 {
                    let n = wait.to_string();
                    ui.label(
                        egui::RichText::new(crate::i18n::trf("Try again in {} s.", &[&n]))
                            .size(13.0)
                            .color(FAINT),
                    );
                    ui.ctx().request_repaint_after(Duration::from_millis(250));
                }
                if go && wait == 0 && too_long(&ls.buf) {
                    ls.wipe();
                    ls.error = Some(Fail::TooLong);
                    purge_edit_state(ui.ctx());
                } else if go && wait == 0 && !ls.buf.is_empty() {
                    // The passphrase moves out of the field; egui's edit
                    // state (cursor, undo) is dropped with it.
                    submitted = Some(ls.take());
                    purge_edit_state(ui.ctx());
                } else if go && wait > 0 {
                    ls.wipe();
                }
            });
        });
    submitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use jizzcraft_core::vault::KdfParams;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// A stand-in app: records what the gate asked of it.
    struct Fake {
        vault: bool,
        began: Rc<Cell<u32>>,
        jobs: Arc<AtomicU32>,
        /// Decode threads still alive (thumbnails, previews).
        workers: Arc<AtomicU32>,
        /// The inactivity delay: `Some(idle_for)` is due once `now` is past.
        idle_at: Option<Instant>,
        touched: Rc<Cell<u32>>,
        cancel: Arc<AtomicBool>,
        request: bool,
        convert: Option<ConvertRequest>,
        keys: Option<Keep>,
        finished: Rc<Cell<Option<(ConvertKind, ConvertOutcome)>>>,
    }

    impl Lockable for Fake {
        fn vault_active(&self) -> bool {
            self.vault
        }
        fn begin_lock(&mut self) {
            self.began.set(self.began.get() + 1);
            self.cancel.store(true, Ordering::SeqCst);
        }
        fn jobs_finished(&self) -> bool {
            self.jobs.load(Ordering::SeqCst) == 0
        }
        fn workers_finished(&self) -> bool {
            self.workers.load(Ordering::SeqCst) == 0
        }
        fn idle_due(&mut self, now: Instant) -> bool {
            self.idle_at.is_some_and(|t| now >= t)
        }
        fn touch(&mut self, now: Instant) {
            self.touched.set(self.touched.get() + 1);
            self.idle_at = self.idle_at.map(|_| now + Duration::from_secs(60));
        }
        fn idle_deadline(&self, _now: Instant) -> Option<Instant> {
            self.idle_at
        }
        fn take_lock_request(&mut self) -> bool {
            std::mem::take(&mut self.request)
        }
        fn take_convert_request(&mut self) -> Option<ConvertRequest> {
            self.convert.take()
        }
        fn keys(&self) -> Option<Keep> {
            self.keys.clone()
        }
        fn conversion_finished(&mut self, kind: ConvertKind, outcome: ConvertOutcome) {
            self.finished.set(Some((kind, outcome)));
        }
    }

    fn fake(vault: bool) -> Fake {
        Fake {
            vault,
            began: Rc::new(Cell::new(0)),
            jobs: Arc::new(AtomicU32::new(0)),
            workers: Arc::new(AtomicU32::new(0)),
            idle_at: None,
            touched: Rc::new(Cell::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            request: false,
            convert: None,
            keys: None,
            finished: Rc::new(Cell::new(None)),
        }
    }

    const PASS: &str = "correct horse battery staple";

    /// An encrypted library with one folder row, fast Argon2 parameters.
    fn encrypted_library() -> (tempfile::TempDir, PathBuf) {
        // Enabling cleans the cache directory: never the user's own.
        crate::test_support::isolate_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        {
            let mut db = Db::open(&path).unwrap();
            db.add_folder(dir.path()).unwrap();
        }
        let p = KdfParams {
            m_cost: 19456,
            t_cost: 2,
            p_cost: 1,
        };
        vault_convert::enable(&path, PASS, p).unwrap();
        (dir, path)
    }

    fn pass(s: &str) -> Zeroizing<String> {
        Zeroizing::new(s.to_owned())
    }

    /// Submit and wait for the worker (the gate polls, like a frame would).
    fn run_unlock(g: &mut Gate<Fake>, p: &str, built: &Cell<u32>, now: Instant) -> bool {
        let started = g.submit(pass(p), now, |path, p| {
            spawn_unlock(path, p, Box::new(|| {}))
        });
        if !started {
            return false;
        }
        for _ in 0..2000 {
            g.poll_unlock(now, |u| {
                built.set(built.get() + 1);
                assert!(u.opener.key.is_some());
                Ok(fake(true))
            });
            if !matches!(g.state, State::Unlocking(_)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("unlock did not finish");
    }

    #[test]
    fn startup_classifies_the_disk() {
        let d = tempfile::tempdir().unwrap();
        let plain = d.path().join("plain.db");
        Db::open(&plain).unwrap();
        assert_eq!(startup(&plain), Startup::Plain);
        let none = d.path().join("none.db");
        assert_eq!(startup(&none), Startup::Plain);
        let (_d, enc) = encrypted_library();
        assert_eq!(startup(&enc), Startup::Encrypted);
    }

    #[test]
    fn missing_header_is_an_error_screen_and_nothing_is_created() {
        let (_d, path) = encrypted_library();
        std::fs::remove_file(vault_header(&path)).unwrap();
        assert_eq!(startup(&path), Startup::Failed(Fail::Header));
        assert!(
            !vault_header(&path).exists(),
            "no new header, no new library"
        );
    }

    #[test]
    fn orphaned_header_with_no_library_never_creates_one() {
        let (_d, path) = encrypted_library();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(startup(&path), Startup::Failed(Fail::Header));
        assert!(!path.exists());
    }

    fn vault_header(db: &Path) -> PathBuf {
        jizzcraft_core::vault::header_path(db)
    }

    #[test]
    fn right_passphrase_opens_and_wrong_one_does_not() {
        let (_d, path) = encrypted_library();
        let built = Cell::new(0);
        let now = Instant::now();
        let mut g: Gate<Fake> = Gate::locked(path);

        assert!(run_unlock(&mut g, "not the passphrase", &built, now));
        assert_eq!(built.get(), 0, "no app is built from a wrong passphrase");
        match &g.state {
            State::Locked(ls) => {
                assert_eq!(ls.error(), Some(Fail::Wrong));
                assert_eq!(ls.failures(), 1);
                assert!(ls.buf.is_empty());
            }
            s => panic!("expected locked, got {}", s.name()),
        }

        assert!(run_unlock(&mut g, PASS, &built, now));
        assert_eq!(built.get(), 1);
        assert_eq!(g.state.name(), "open");
    }

    #[test]
    fn open_needs_a_worker_answer() {
        let mut g: Gate<Fake> = Gate::locked(PathBuf::from("/nonexistent/library.db"));
        // Polling with nothing pending does nothing.
        g.poll_unlock(Instant::now(), |_| panic!("no key, no app"));
        assert_eq!(g.state.name(), "locked");
        // An empty passphrase is not even sent to a worker.
        let started = g.submit(pass(""), Instant::now(), |_, _| panic!("no worker"));
        assert!(!started);
        // A locked gate cannot be "locked" nor opened by a lock request.
        assert!(!g.request_lock());
        assert_eq!(g.state.name(), "locked");
    }

    #[test]
    fn a_worker_that_vanishes_leaves_the_library_locked() {
        let mut g: Gate<Fake> = Gate::locked(PathBuf::from("x"));
        let now = Instant::now();
        g.submit(pass("anything"), now, |_, _| {
            let (_tx, rx) = channel::<UnlockResult>();
            rx // sender dropped at once
        });
        g.poll_unlock(now, |_| panic!("no key, no app"));
        match &g.state {
            State::Locked(ls) => assert_eq!(ls.error(), Some(Fail::Other)),
            s => panic!("got {}", s.name()),
        }
    }

    #[test]
    fn lock_cancels_jobs_and_waits_for_them() {
        let mut app = fake(true);
        let (began, jobs, cancel) = (app.began.clone(), app.jobs.clone(), app.cancel.clone());
        jobs.fetch_add(1, Ordering::SeqCst);
        app.request = true;
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert!(g.request_lock());
        assert_eq!(began.get(), 1);
        assert!(cancel.load(Ordering::SeqCst), "jobs were told to stop");
        assert_eq!(g.state.name(), "locking");

        // One job still runs: still "locking", the app is not dropped.
        g.poll_lock();
        assert_eq!(g.state.name(), "locking");
        // A second lock request in this state is a no-op.
        assert!(!g.request_lock());
        assert_eq!(began.get(), 1);

        jobs.fetch_sub(1, Ordering::SeqCst);
        g.poll_lock();
        assert_eq!(g.state.name(), "locked");
    }

    #[test]
    fn a_plain_library_cannot_be_locked() {
        let app = fake(false);
        let began = app.began.clone();
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert!(!g.request_lock());
        assert_eq!(g.state.name(), "open");
        assert_eq!(began.get(), 0);
    }

    #[test]
    fn full_cycle_lock_then_unlock_again() {
        let (_d, path) = encrypted_library();
        let built = Cell::new(0);
        let now = Instant::now();
        let mut g: Gate<Fake> = Gate::locked(path);
        assert!(run_unlock(&mut g, PASS, &built, now));
        assert!(g.request_lock());
        g.poll_lock();
        assert_eq!(g.state.name(), "locked");
        assert!(run_unlock(&mut g, PASS, &built, now));
        assert_eq!(g.state.name(), "open");
        assert_eq!(built.get(), 2);
    }

    #[test]
    fn soft_delay_grows_and_blocks_submission() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(2), Duration::ZERO);
        assert_eq!(backoff(3), Duration::from_secs(1));
        assert_eq!(backoff(4), Duration::from_secs(2));
        assert_eq!(backoff(5), Duration::from_secs(4));
        assert_eq!(backoff(40), Duration::from_secs(30));

        let (_d, path) = encrypted_library();
        let built = Cell::new(0);
        let now = Instant::now();
        let mut g: Gate<Fake> = Gate::locked(path);
        for _ in 0..3 {
            assert!(run_unlock(&mut g, "wrong wrong wrong", &built, now));
        }
        // Three failures: the next attempt inside the delay is refused, even
        // with the right passphrase, and no worker is started.
        assert!(!g.submit(pass(PASS), now, |_, _| panic!("no worker in the delay")));
        assert_eq!(g.state.name(), "locked");
        // After the delay it is accepted.
        let later = now + Duration::from_secs(2);
        assert!(run_unlock(&mut g, PASS, &built, later));
        assert_eq!(g.state.name(), "open");
    }

    #[test]
    fn typed_passphrase_is_wiped_and_never_reallocates() {
        let mut ls = LockScreen::new();
        let cap = ls.buf.capacity();
        assert!(cap >= MAX_PASSPHRASE_BYTES);
        ls.buf.push_str("hunter2 hunter2");
        let taken = ls.take();
        assert_eq!(&**taken, "hunter2 hunter2");
        assert!(ls.buf.is_empty());
        assert!(ls.buf.capacity() >= MAX_PASSPHRASE_BYTES);
        ls.buf.push_str("again");
        ls.wipe();
        assert!(ls.buf.is_empty());
    }

    #[test]
    fn fail_text_carries_no_detail() {
        for f in [Fail::Wrong, Fail::Header, Fail::Finish, Fail::Other] {
            let t = f.text();
            assert!(!t.contains('/') && !t.contains("library.db"));
        }
        assert_eq!(Fail::from_vault(VaultError::WrongPassphrase), Fail::Wrong);
        assert_eq!(Fail::from_vault(VaultError::HeaderOrphan), Fail::Header);
        assert_eq!(Fail::from_vault(VaultError::DbCorrupt), Fail::Other);
        assert_eq!(Fail::from_vault(VaultError::FinishFailed), Fail::Finish);
        assert_eq!(
            ConvertOutcome::from_vault(VaultError::FinishFailed),
            ConvertOutcome::FinishFailed
        );
        assert!(!Fail::Finish.text().contains("intact"));
    }

    #[test]
    fn a_failed_finish_goes_to_the_lock_screen_with_its_own_sentence() {
        let (_d, path) = plain_library();
        let mut g = Gate::open_plain(path, fake(false));
        let (tx, rx) = channel();
        tx.send(Converted {
            outcome: ConvertOutcome::FinishFailed,
            lib: Err(Fail::Finish),
        })
        .unwrap();
        g.state = State::Converting(Converting {
            phase: ConvPhase::Running(rx),
            kind: ConvertKind::Enable,
            cancel: Arc::new(AtomicBool::new(false)),
            committed: Arc::new(AtomicBool::new(true)),
        });
        g.poll_convert(|_, _, _, _, _| unreachable!(), |_| Err("unused".into()));
        match &g.state {
            State::Locked(ls) => assert_eq!(ls.error(), Some(Fail::Finish)),
            _ => panic!("expected the lock screen"),
        }
    }

    // ---- conversion state machine -------------------------------------------

    const FAST: KdfParams = KdfParams {
        m_cost: 19456,
        t_cost: 2,
        p_cost: 1,
    };
    type Record = Rc<Cell<Option<(ConvertKind, ConvertOutcome)>>>;

    fn req(kind: ConvertKind, pass_: &str, new: Option<&str>) -> ConvertRequest {
        ConvertRequest {
            kind,
            pass: pass(pass_),
            new_pass: new.map(pass),
        }
    }

    fn keys_of(path: &Path) -> Keep {
        let v = vault_convert::unlock_library(path, PASS).unwrap();
        (Arc::new(v.db_key()), Arc::new(v.thumb_keys()))
    }

    fn open_gate(path: &Path, app: Fake) -> Gate<Fake> {
        Gate::open_plain(path.to_path_buf(), app)
    }

    /// Poll like frames do until the conversion left `Converting`. Returns
    /// the keyed-ness of the reopened library, if one was built.
    fn run_convert(g: &mut Gate<Fake>, rec: &Record) -> Option<bool> {
        let built = Cell::new(None);
        for _ in 0..4000 {
            g.poll_convert(
                |path, r, keep, cancel, committed| {
                    let (tx, rx) = channel();
                    std::thread::spawn(move || {
                        let _ =
                            tx.send(convert_blocking(&path, r, keep, &cancel, &committed, FAST));
                    });
                    rx
                },
                |r| {
                    built.set(Some(r.opener.key.is_some()));
                    let mut f = fake(r.opener.key.is_some());
                    f.finished = rec.clone();
                    Ok(f)
                },
            );
            if !matches!(g.state, State::Converting(_)) {
                return built.get();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("conversion did not finish");
    }

    fn plain_library() -> (tempfile::TempDir, PathBuf) {
        crate::test_support::isolate_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        let mut db = Db::open(&path).unwrap();
        db.add_folder(dir.path()).unwrap();
        (dir, path)
    }

    #[test]
    fn enabling_closes_the_app_converts_on_a_worker_and_reopens_with_the_key() {
        let (_d, path) = plain_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let began = app.began.clone();
        let jobs = app.jobs.clone();
        jobs.store(1, Ordering::SeqCst); // a job still holds the library
        let mut g = open_gate(&path, app);

        assert!(g.start_convert());
        assert_eq!(g.state.name(), "converting");
        assert_eq!(began.get(), 1, "jobs were told to stop");
        // Nothing converts while a job holds the connection.
        for _ in 0..3 {
            g.poll_convert(
                |_, _, _, _, _| panic!("worker must not start"),
                |_| Err(String::new()),
            );
        }
        assert_eq!(
            probe_format(&path).unwrap(),
            FileFormat::Plain,
            "still clear while quiescing"
        );
        jobs.store(0, Ordering::SeqCst);
        assert_eq!(run_convert(&mut g, &rec), Some(true));
        assert_eq!(g.state.name(), "open");
        assert_eq!(rec.get(), Some((ConvertKind::Enable, ConvertOutcome::Done)));
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Encrypted);
    }

    #[test]
    fn wrong_passphrase_on_disable_changes_nothing_and_reopens_without_asking() {
        let (_d, path) = encrypted_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(true);
        app.keys = Some(keys_of(&path));
        app.convert = Some(req(ConvertKind::Disable, "definitely not it", None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert_eq!(
            run_convert(&mut g, &rec),
            Some(true),
            "reopened with the old key"
        );
        assert_eq!(
            rec.get(),
            Some((ConvertKind::Disable, ConvertOutcome::WrongPassphrase))
        );
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Encrypted);
        assert_eq!(g.state.name(), "open");
    }

    #[test]
    fn disable_with_the_right_passphrase_reopens_clear() {
        let (_d, path) = encrypted_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(true);
        app.keys = Some(keys_of(&path));
        app.convert = Some(req(ConvertKind::Disable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert_eq!(run_convert(&mut g, &rec), Some(false));
        assert_eq!(
            rec.get(),
            Some((ConvertKind::Disable, ConvertOutcome::Done))
        );
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Plain);
    }

    #[test]
    fn change_passphrase_moves_to_the_new_one() {
        let (_d, path) = encrypted_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(true);
        app.keys = Some(keys_of(&path));
        app.convert = Some(req(
            ConvertKind::Change,
            PASS,
            Some("another long passphrase 2"),
        ));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert_eq!(run_convert(&mut g, &rec), Some(true));
        assert_eq!(rec.get(), Some((ConvertKind::Change, ConvertOutcome::Done)));
        assert_eq!(
            unlock_blocking(&path, PASS).err(),
            Some(Fail::Wrong),
            "the old passphrase is refused"
        );
        assert!(unlock_blocking(&path, "another long passphrase 2").is_ok());
    }

    #[test]
    fn a_short_new_passphrase_never_reaches_the_engine() {
        let (_d, path) = plain_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, "short", None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert_eq!(run_convert(&mut g, &rec), Some(false));
        assert_eq!(
            rec.get(),
            Some((ConvertKind::Enable, ConvertOutcome::Failed))
        );
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Plain);
    }

    #[test]
    fn cancelling_before_the_commit_point_changes_nothing() {
        let (_d, path) = plain_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert!(g.cancel_convert(), "recorded while not committed");
        assert_eq!(run_convert(&mut g, &rec), Some(false));
        assert_eq!(
            rec.get(),
            Some((ConvertKind::Enable, ConvertOutcome::Cancelled))
        );
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Plain);
        assert!(!vault_header(&path).exists());
    }

    #[test]
    fn cancel_is_refused_after_the_commit_point() {
        let (_d, path) = plain_library();
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        if let State::Converting(c) = &g.state {
            c.committed.store(true, Ordering::SeqCst);
            assert!(c.committed());
        }
        assert!(!g.cancel_convert());
        if let State::Converting(c) = &g.state {
            assert!(!c.cancelling(), "the flag was not raised");
        }
    }

    #[test]
    fn closing_during_a_conversion_is_held_until_confirmed() {
        let (_d, path) = plain_library();
        // Not converting: the close goes through, no warning.
        let mut g = open_gate(&path, fake(false));
        assert_eq!(g.on_close_request(), CloseVerdict::Allow);
        assert!(!g.close_warning());

        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert!(g.converting());
        // Held, and the warning is up.
        assert_eq!(g.on_close_request(), CloseVerdict::Hold);
        assert!(g.close_warning());
        // Kept waiting: the warning goes, the next close asks again.
        g.answer_close(false);
        assert!(!g.close_warning());
        assert_eq!(g.on_close_request(), CloseVerdict::Hold);
        // Confirmed: the close goes through.
        g.answer_close(true);
        assert_eq!(g.on_close_request(), CloseVerdict::Allow);
        // Nothing was written to the disk by the question itself.
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Plain);
    }

    #[test]
    fn warning_goes_away_when_the_conversion_ends() {
        let (_d, path) = plain_library();
        let rec: Record = Rc::new(Cell::new(None));
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        assert_eq!(g.on_close_request(), CloseVerdict::Hold);
        assert_eq!(run_convert(&mut g, &rec), Some(true));
        assert!(!g.converting());
        assert!(!g.close_warning(), "no stale warning on the open library");
        assert_eq!(g.on_close_request(), CloseVerdict::Allow);
    }

    #[test]
    fn only_a_matching_open_app_converts() {
        let (_d, path) = plain_library();
        // Enable on an app that is already encrypted: refused, request consumed.
        let mut app = fake(true);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(!g.start_convert());
        assert_eq!(g.state.name(), "open");
        // Disable on a clear app: refused too.
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Disable, PASS, None));
        let mut g = open_gate(&path, app);
        assert!(!g.start_convert());
        // Not open: nothing to convert.
        let mut g: Gate<Fake> = Gate::locked(path);
        assert!(!g.start_convert());
        assert!(!g.cancel_convert());
        assert_eq!(g.state.name(), "locked");
    }

    #[test]
    fn a_request_never_prints_its_secret() {
        let r = req(
            ConvertKind::Change,
            "hunter2 hunter2",
            Some("another one 12345"),
        );
        let shown = format!("{r:?}");
        assert!(!shown.contains("hunter2") && !shown.contains("another"));
    }

    // ---- audit round 2: workers, deadline, history ---------------------------

    #[test]
    fn locked_is_shown_only_after_the_decode_workers_are_gone() {
        let app = fake(true);
        let workers = app.workers.clone();
        workers.store(2, Ordering::SeqCst);
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert!(g.request_lock());
        // Jobs are done, workers are not: still "Locking", app kept alive.
        g.poll_lock();
        assert_eq!(g.state.name(), "locking");
        workers.store(1, Ordering::SeqCst);
        g.poll_lock();
        assert_eq!(g.state.name(), "locking");
        workers.store(0, Ordering::SeqCst);
        g.poll_lock();
        assert_eq!(g.state.name(), "locked");
    }

    #[test]
    fn a_conversion_does_not_start_while_a_worker_could_still_write() {
        let (_d, path) = plain_library();
        let mut app = fake(false);
        app.convert = Some(req(ConvertKind::Enable, PASS, None));
        let workers = app.workers.clone();
        workers.store(1, Ordering::SeqCst);
        let mut g = open_gate(&path, app);
        assert!(g.start_convert());
        for _ in 0..3 {
            g.poll_convert(
                |_, _, _, _, _| panic!("no conversion while a worker runs"),
                |_| Err(String::new()),
            );
        }
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Plain);
        workers.store(0, Ordering::SeqCst);
        assert_eq!(run_convert(&mut g, &Rc::new(Cell::new(None))), Some(true));
    }

    #[test]
    fn the_thumbnail_keys_are_freed_before_locked_is_shown() {
        use jizzcraft_core::vault::{ThumbKeys, VaultKey};
        let keys: Arc<ThumbKeys> = Arc::new(VaultKey::from_bytes_for_tests([3; 32]).thumb_keys());
        let weak = Arc::downgrade(&keys);
        let mut app = fake(true);
        app.keys = Some((
            Arc::new(VaultKey::from_bytes_for_tests([3; 32]).db_key()),
            keys,
        ));
        let workers = app.workers.clone();
        workers.store(1, Ordering::SeqCst);
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert!(g.request_lock());
        g.poll_lock();
        assert!(weak.upgrade().is_some(), "still held while locking");
        workers.store(0, Ordering::SeqCst);
        g.poll_lock();
        assert_eq!(g.state.name(), "locked");
        assert!(weak.upgrade().is_none(), "gone before the lock screen");
    }

    #[test]
    fn the_deadline_locks_without_a_frame_and_input_after_it_does_not_save_you() {
        let t0 = Instant::now();
        let mut app = fake(true);
        app.idle_at = Some(t0 + Duration::from_secs(900));
        let touched = app.touched.clone();
        let began = app.began.clone();
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert_eq!(g.idle_deadline(t0), Some(t0 + Duration::from_secs(900)));

        // Before the deadline: nothing, and input pushes it back.
        assert!(!g.tick(t0 + Duration::from_secs(899)));
        g.touch(t0 + Duration::from_secs(899));
        assert_eq!(touched.get(), 1);
        assert_eq!(g.state.name(), "open");

        // The window was minimized past the deadline: the first thing that
        // runs is the tick, with no frame and no input counted before it.
        let late = t0 + Duration::from_secs(899 + 61);
        assert!(g.tick(late));
        assert_eq!(began.get(), 1, "hidden and cancelled at once");
        assert_eq!(g.state.name(), "locking");
        // The click that woke the window cannot rescue it any more.
        g.touch(late);
        assert_eq!(touched.get(), 1, "no app left to touch");
        g.poll_lock();
        assert_eq!(g.state.name(), "locked");
        assert_eq!(g.idle_deadline(late), None);
    }

    #[test]
    fn a_plain_library_never_idles_out() {
        let mut app = fake(false);
        app.idle_at = Some(Instant::now());
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert!(!g.tick(Instant::now() + Duration::from_secs(10)));
        assert_eq!(g.idle_deadline(Instant::now()), None);
        assert_eq!(g.state.name(), "open");
    }

    #[test]
    fn take_requests_locks_before_the_frame_is_drawn() {
        let mut app = fake(true);
        app.request = true;
        let mut g = Gate::open_plain(PathBuf::from("x"), app);
        assert!(g.take_requests());
        assert_eq!(g.state.name(), "locking");
        assert!(!g.take_requests());
    }

    #[test]
    fn the_idle_timer_fires_without_any_frame_and_once_per_deadline() {
        use std::sync::atomic::AtomicUsize;
        let fired = Arc::new(AtomicUsize::new(0));
        let f2 = fired.clone();
        let t = IdleTimer::new(Box::new(move || {
            f2.fetch_add(1, Ordering::SeqCst);
        }));
        let d = Instant::now() + Duration::from_millis(80);
        t.set(Some(d));
        t.set(Some(d)); // setting it again changes nothing
        for _ in 0..400 {
            if fired.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(100));
        t.set(Some(d));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the same deadline fires once"
        );
        // A cleared deadline never fires.
        t.set(Some(Instant::now() + Duration::from_millis(60)));
        t.set(None);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        drop(t); // joins the thread
    }

    #[test]
    fn egui_text_history_is_purged() {
        use egui::text_edit::TextEditState;
        let ctx = egui::Context::default();
        let mut buf = String::from("a passphrase typed here");
        let id = egui::Id::new("pw");
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::TextEdit::singleline(&mut buf)
                .id(id)
                .password(true)
                .show(ui);
        });
        out.textures_delta.clear();
        assert!(
            TextEditState::load(&ctx, id).is_some(),
            "egui keeps the field state"
        );
        purge_edit_state(&ctx);
        assert!(TextEditState::load(&ctx, id).is_none(), "and it is gone");
    }

    #[test]
    fn overlong_passphrases_are_refused_not_cut() {
        let ok = "a".repeat(MAX_PASSPHRASE_CHARS);
        let over = "a".repeat(MAX_PASSPHRASE_CHARS + 1);
        assert!(!too_long(&ok));
        assert!(too_long(&over));
        // Characters, not bytes.
        assert!(!too_long(&"é".repeat(MAX_PASSPHRASE_CHARS)));
        assert!(too_long(&"é".repeat(MAX_PASSPHRASE_CHARS + 1)));
        // The buffer holds the limit plus one character without growing.
        assert!(secret_buf().capacity() >= (MAX_PASSPHRASE_CHARS + 1) * 4);
        // Nothing is tried: no header is read for an overlong one.
        assert_eq!(
            unlock_blocking(Path::new("/nonexistent/library.db"), &over).err(),
            Some(Fail::TooLong)
        );
        assert!(!Fail::TooLong.text().is_empty());
        // The conversion refuses it too, whatever the window did.
        let (_d, path) = plain_library();
        let r = convert_blocking(
            &path,
            req(ConvertKind::Enable, &over, None),
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
            FAST,
        );
        assert_eq!(r.outcome, ConvertOutcome::Failed);
        assert_eq!(probe_format(&path).unwrap(), FileFormat::Plain);
    }
}
