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

use jizzcraft_core::db::{FileFormat, LibraryOpener};
use jizzcraft_core::vault::{DbKey, ThumbKeys, VaultError};
use jizzcraft_core::{Db, Error, vault_convert};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Longest passphrase taken from the field, in bytes (same as the CLI).
pub const MAX_PASSPHRASE_BYTES: usize = 4096;

/// A fixed, translatable reason a library cannot be unlocked or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    /// The passphrase did not open the header.
    Wrong,
    /// Header missing, damaged, unsupported or orphaned: no prompt helps.
    Header,
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
            _ => Fail::Other,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Fail::Wrong => crate::i18n::tr("Wrong passphrase."),
            Fail::Header => crate::i18n::tr(
                "The vault header of this library is missing or damaged. \
                 The library cannot be opened and nothing was changed.",
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
    /// The user asked for a lock since the last call.
    fn take_lock_request(&mut self) -> bool;
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
            State::Failed(_) => "failed",
        }
    }
}

/// The state machine. `A` is the hosted app.
pub struct Gate<A> {
    pub state: State<A>,
    db_path: PathBuf,
}

impl<A: Lockable> Gate<A> {
    pub fn locked(db_path: PathBuf) -> Self {
        Gate {
            state: State::Locked(LockScreen::new()),
            db_path,
        }
    }

    pub fn failed(db_path: PathBuf, why: Fail) -> Self {
        Gate {
            state: State::Failed(why),
            db_path,
        }
    }

    /// A library that is not encrypted: no key was ever involved.
    pub fn open_plain(db_path: PathBuf, app: A) -> Self {
        Gate {
            state: State::Open(Box::new(app)),
            db_path,
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

    /// Drop the app once its jobs have ended; only then show "Locked".
    pub fn poll_lock(&mut self) {
        let State::Locking(app) = &self.state else {
            return;
        };
        if app.jobs_finished() {
            // Dropping closes the database, frees textures and the
            // thumbnail service and releases the last key references.
            self.state = State::Locked(LockScreen::new());
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

impl eframe::App for Root {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();

        // Transitions first, so this frame already draws the new state.
        let ctx2 = ctx.clone();
        let (opts, rs) = (&self.opts, &self.render_state);
        self.gate.poll_unlock(now, |u| {
            let lib = Library::from_unlocked(u);
            JizzApp::open(&ctx2, rs.clone(), opts.clone(), lib)
        });
        self.gate.poll_lock();
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

        match &mut self.gate.state {
            State::Open(app) => {
                eframe::App::ui(&mut **app, ui, frame);
                if app.take_lock_request() {
                    self.gate.request_lock();
                    ctx.request_repaint();
                }
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
                        .char_limit(MAX_PASSPHRASE_BYTES / 4)
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
                if let Some(e) = ls.error {
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
                if go && wait == 0 && !ls.buf.is_empty() {
                    // The passphrase moves out of the field; egui's edit
                    // state (cursor, undo) is dropped with it.
                    submitted = Some(ls.take());
                    ui.ctx().data_mut(|d| {
                        d.remove_by_type::<egui::text_edit::TextEditState>();
                    });
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
        cancel: Arc<AtomicBool>,
        request: bool,
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
        fn take_lock_request(&mut self) -> bool {
            std::mem::take(&mut self.request)
        }
    }

    fn fake(vault: bool) -> Fake {
        Fake {
            vault,
            began: Rc::new(Cell::new(0)),
            jobs: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            request: false,
        }
    }

    const PASS: &str = "correct horse battery staple";

    /// An encrypted library with one folder row, fast Argon2 parameters.
    fn encrypted_library() -> (tempfile::TempDir, PathBuf) {
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
        for f in [Fail::Wrong, Fail::Header, Fail::Other] {
            let t = f.text();
            assert!(!t.contains('/') && !t.contains("library.db"));
        }
        assert_eq!(Fail::from_vault(VaultError::WrongPassphrase), Fail::Wrong);
        assert_eq!(Fail::from_vault(VaultError::HeaderOrphan), Fail::Header);
        assert_eq!(Fail::from_vault(VaultError::DbCorrupt), Fail::Other);
    }
}
