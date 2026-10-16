//! Settings > Privacy: encrypt, change the passphrase, turn encryption off,
//! automatic lock, and the warning in front of the clear JSON export.
//!
//! Nothing here touches the library: a form builds a [`ConvertRequest`], the
//! app hands it to the root envelope (`crate::root`), which closes the
//! library, converts on a worker and opens it again. The pure parts
//! (validation, strength, the inactivity clock) are tested without a window.
//!
//! Rules: no recovery, no hint, no "remember me"; every message is a fixed,
//! translated sentence; a passphrase only ever lives in a `Zeroizing` buffer
//! that is moved, not copied.

use super::JizzApp;
use crate::i18n::{tr, trf};
use crate::root::{
    ConvertKind, ConvertOutcome, ConvertRequest, Fail, MAX_PASSPHRASE_CHARS, MIN_PASSPHRASE_CHARS,
    purge_edit_state, secret_buf, too_long,
};
use crate::theme::{DANGER, FAINT, MUTED};
use crate::widgets;
use egui::RichText;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Default inactivity delay, in minutes.
pub const DEFAULT_AUTOLOCK_MINUTES: u32 = 15;
/// Upper bound of the setting (one day).
pub const MAX_AUTOLOCK_MINUTES: u32 = 24 * 60;

// ---- pure parts -------------------------------------------------------------

/// Why a form is refused. Fixed sentences, translated by [`FormError::text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormError {
    TooShort,
    /// Over `MAX_PASSPHRASE_CHARS`: refused, never cut.
    TooLong,
    Mismatch,
    NotAcknowledged,
    EmptyCurrent,
    /// A job holds the library: wait for it.
    BusyJobs,
    /// Not possible now (read-only library, wrong mode).
    Unavailable,
}

impl FormError {
    pub fn text(self) -> &'static str {
        match self {
            FormError::TooShort => tr("Use at least 12 characters."),
            FormError::TooLong => tr("The passphrase is too long (1024 characters at most)."),
            FormError::Mismatch => tr("The two passphrases do not match."),
            FormError::NotAcknowledged => tr("Tick the box to confirm that you understand."),
            FormError::EmptyCurrent => tr("Enter your current passphrase."),
            FormError::BusyJobs => tr("Wait for running tasks to finish, then try again."),
            FormError::Unavailable => {
                tr("Encryption cannot be changed now (read-only library or a change in progress).")
            }
        }
    }
}

/// Check a form before anything leaves the window. Order matters: the first
/// problem is the one shown.
pub fn validate(
    kind: ConvertKind,
    current: &str,
    new: &str,
    again: &str,
    ack: bool,
) -> Result<(), FormError> {
    if too_long(current) || too_long(new) || too_long(again) {
        return Err(FormError::TooLong);
    }
    let new_checks = || -> Result<(), FormError> {
        if new.chars().count() < MIN_PASSPHRASE_CHARS {
            return Err(FormError::TooShort);
        }
        if new != again {
            return Err(FormError::Mismatch);
        }
        Ok(())
    };
    match kind {
        ConvertKind::Enable => {
            new_checks()?;
            if !ack {
                return Err(FormError::NotAcknowledged);
            }
            Ok(())
        }
        ConvertKind::Disable => {
            if current.is_empty() {
                return Err(FormError::EmptyCurrent);
            }
            Ok(())
        }
        ConvertKind::Change => {
            if current.is_empty() {
                return Err(FormError::EmptyCurrent);
            }
            new_checks()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    Weak,
    Fair,
    Good,
    Strong,
}

/// A rough local estimate, not a guarantee: character pool times length, with
/// repeated characters discounted. Nothing is sent anywhere.
pub fn strength(p: &str) -> Strength {
    let len = p.chars().count();
    if len == 0 {
        return Strength::Weak;
    }
    let (mut lower, mut upper, mut digit, mut other) = (false, false, false, false);
    // Distinct characters without a per-call set: a 128-bit map for ASCII, a
    // sorted list only when something else is typed.
    let mut ascii: u128 = 0;
    let mut wide: Vec<char> = Vec::new();
    for c in p.chars() {
        if c.is_ascii() {
            ascii |= 1u128 << (c as u32);
        } else {
            wide.push(c);
        }
        if c.is_lowercase() {
            lower = true;
        } else if c.is_uppercase() {
            upper = true;
        } else if c.is_ascii_digit() {
            digit = true;
        } else {
            other = true;
        }
    }
    wide.sort_unstable();
    wide.dedup();
    let distinct = ascii.count_ones() as usize + wide.len();
    // `wide` held copies of typed characters: wipe them.
    zeroize::Zeroize::zeroize(&mut wide[..]);
    let pool =
        (lower as u32) * 26 + (upper as u32) * 26 + (digit as u32) * 10 + (other as u32) * 33;
    let effective = len.min(distinct * 2) as f32;
    let bits = effective * (pool.max(2) as f32).log2();
    match bits {
        b if b < 40.0 => Strength::Weak,
        b if b < 60.0 => Strength::Fair,
        b if b < 80.0 => Strength::Good,
        _ => Strength::Strong,
    }
}

/// The inactivity clock behind the automatic lock.
#[derive(Debug, Clone, Copy)]
pub struct AutoLock {
    last: Instant,
}

impl AutoLock {
    pub fn new(now: Instant) -> Self {
        AutoLock { last: now }
    }

    /// Input happened.
    pub fn touch(&mut self, now: Instant) {
        self.last = now;
    }

    /// Should the library lock now? `minutes == 0` never locks. A video that
    /// is playing counts as activity, so it resets the clock.
    pub fn due(&mut self, now: Instant, minutes: u32, playing: bool) -> bool {
        if playing {
            self.last = now;
            return false;
        }
        minutes > 0
            && now.saturating_duration_since(self.last)
                >= Duration::from_secs(u64::from(minutes) * 60)
    }

    /// Time left before the lock, `None` when the setting is off.
    pub fn remaining(&self, now: Instant, minutes: u32) -> Option<Duration> {
        (minutes > 0).then(|| {
            Duration::from_secs(u64::from(minutes) * 60)
                .saturating_sub(now.saturating_duration_since(self.last))
        })
    }
}

/// The one toast of a finished conversion (fixed, no name, no path).
pub fn finished_text(kind: ConvertKind, outcome: ConvertOutcome) -> &'static str {
    match (outcome, kind) {
        (ConvertOutcome::Done, ConvertKind::Enable) => tr("Library encrypted."),
        (ConvertOutcome::Done, ConvertKind::Disable) => tr("Encryption turned off."),
        (ConvertOutcome::Done, ConvertKind::Change) => tr("Passphrase changed."),
        (ConvertOutcome::Cancelled, _) => tr("Cancelled. Nothing was changed."),
        (ConvertOutcome::WrongPassphrase, _) => tr("Wrong passphrase. Nothing was changed."),
        (ConvertOutcome::Busy, _) => {
            tr("The library is in use by another program. Nothing was changed.")
        }
        (ConvertOutcome::FinishFailed, _) => Fail::Finish.text(),
        (ConvertOutcome::Failed, _) => tr("The operation failed. Your library is intact."),
    }
}

// ---- state ------------------------------------------------------------------

pub(super) struct Dialog {
    kind: ConvertKind,
    current: Zeroizing<String>,
    new: Zeroizing<String>,
    again: Zeroizing<String>,
    ack: bool,
    error: Option<FormError>,
    focus: bool,
    /// Estimated when the new passphrase changes, not every frame.
    strength: Strength,
}

/// A buffer for a field this dialog does not show: nothing allocated.
fn unused() -> Zeroizing<String> {
    Zeroizing::new(String::new())
}

impl Dialog {
    fn new(kind: ConvertKind) -> Dialog {
        let needs_current = kind != ConvertKind::Enable;
        let needs_new = kind != ConvertKind::Disable;
        Dialog {
            kind,
            current: if needs_current {
                secret_buf()
            } else {
                unused()
            },
            new: if needs_new { secret_buf() } else { unused() },
            again: if needs_new { secret_buf() } else { unused() },
            strength: Strength::Weak,
            ack: false,
            error: None,
            focus: true,
        }
    }
}

#[derive(Default)]
pub(super) struct VaultUi {
    pub(super) dialog: Option<Dialog>,
    /// Command and arguments of a clear-text export (`meta.export`,
    /// `meta.export_nfo`) waiting for the user to accept an unencrypted file.
    pub(super) export_warn: Option<(&'static str, Value)>,
    /// Playlist whose `.m3u8` export waits for the user to accept that the
    /// file lists media paths in clear.
    pub(super) m3u_warn: Option<i64>,
}

impl VaultUi {
    pub(super) fn open(&mut self, kind: ConvertKind) {
        self.dialog = Some(Dialog::new(kind));
    }

    pub(super) fn warn_clear_export(&mut self, cmd: &'static str, args: Value) {
        self.export_warn = Some((cmd, args));
    }
}

fn field(ui: &mut egui::Ui, buf: &mut String, hint: &str, focus: bool) -> egui::Response {
    let r = ui.add(
        egui::TextEdit::singleline(buf)
            .password(true)
            // One more than the limit: an overlong paste is seen and refused
            // with a message, not silently cut.
            .char_limit(MAX_PASSPHRASE_CHARS + 1)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
    if focus {
        r.request_focus();
    }
    r
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(FAINT)).wrap());
}

impl JizzApp {
    /// A job holds, or is about to hold, a connection to the library.
    pub(super) fn jobs_running(&self) -> bool {
        self.jobs.load(std::sync::atomic::Ordering::SeqCst) > 0
            || self.busy > 0
            || self.remove_cancel.is_some()
    }

    /// Hand a validated request to the root envelope. Refused while any job
    /// runs: the library is closed for the whole conversion.
    pub(crate) fn request_convert(&mut self, req: ConvertRequest) -> Result<(), FormError> {
        if self.convert_request.is_some() {
            return Err(FormError::Unavailable);
        }
        if self.jobs_running() {
            return Err(FormError::BusyJobs);
        }
        let mode_ok = match req.kind {
            ConvertKind::Enable => self.opener.key.is_none(),
            ConvertKind::Disable | ConvertKind::Change => self.opener.key.is_some(),
        };
        if !mode_ok || self.read_only {
            return Err(FormError::Unavailable);
        }
        self.convert_request = Some(req);
        self.ctx.request_repaint();
        Ok(())
    }

    /// Both dialogs, drawn every frame after the settings window.
    pub(super) fn vault_windows(&mut self, ctx: &egui::Context) {
        self.vault_dialog(ctx);
        self.export_warning_window(ctx);
        self.m3u_warning_window(ctx);
    }

    fn m3u_warning_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.vault_ui.m3u_warn else {
            return;
        };
        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        egui::Window::new(tr("Playlist file in clear"))
            .open(&mut open)
            .id(egui::Id::new("vault-m3u-warning"))
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(tr(
                        "This M3U8 file is not encrypted, even though your library is. \
                         It lists the paths of your files in clear. Anyone who can read \
                         the file can read them.",
                    ))
                    .wrap(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if widgets::button_solid(ui, tr("Export anyway")).clicked() {
                        proceed = true;
                    }
                    if widgets::button_outline(ui, tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if proceed {
            self.vault_ui.m3u_warn = None;
            self.org_export_confirmed(id);
        } else if cancel || !open {
            self.vault_ui.m3u_warn = None;
        }
    }

    fn vault_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.vault_ui.dialog.take() else {
            return;
        };
        let title = match d.kind {
            ConvertKind::Enable => tr("Encrypt library"),
            ConvertKind::Disable => tr("Turn off encryption"),
            ConvertKind::Change => tr("Change passphrase"),
        };
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        let focus = std::mem::take(&mut d.focus);
        egui::Window::new(title)
            .open(&mut open)
            .id(egui::Id::new("vault-dialog"))
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let needs_current = d.kind != ConvertKind::Enable;
                let needs_new = d.kind != ConvertKind::Disable;
                match d.kind {
                    ConvertKind::Enable => {
                        note(
                            ui,
                            tr("Your tags, notes, history, playlists and thumbnails will be \
                                stored encrypted. Your media files are not touched."),
                        );
                    }
                    ConvertKind::Disable => {
                        note(
                            ui,
                            tr("Turning encryption off leaves your library readable by anyone \
                                who has the files."),
                        );
                    }
                    ConvertKind::Change => {
                        note(
                            ui,
                            tr("A copy of the old header file still opens with the old passphrase."),
                        );
                        note(
                            ui,
                            tr("Changing the passphrase does not regenerate the data key: \
                                after a passphrase leak, turn encryption off and on again."),
                        );
                    }
                }
                ui.add_space(6.0);
                if needs_current {
                    note(ui, tr("Enter your passphrase again to confirm."));
                    field(ui, &mut d.current, tr("Current passphrase"), focus);
                }
                if needs_new {
                    let hint = if d.kind == ConvertKind::Enable {
                        tr("Passphrase")
                    } else {
                        tr("New passphrase")
                    };
                    if field(ui, &mut d.new, hint, focus && !needs_current).changed() {
                        d.strength = strength(&d.new);
                    }
                    field(ui, &mut d.again, tr("Repeat the passphrase"), false);
                    let (label, color) = match d.strength {
                        Strength::Weak => (tr("weak"), DANGER),
                        Strength::Fair => (tr("fair"), MUTED),
                        Strength::Good => (tr("good"), MUTED),
                        Strength::Strong => (tr("strong"), MUTED),
                    };
                    ui.label(
                        RichText::new(trf("Strength: {}", &[label]))
                            .size(12.0)
                            .color(color),
                    );
                    note(
                        ui,
                        tr("At least 12 characters. The strength is estimated on this \
                            machine; nothing is sent anywhere."),
                    );
                }
                if d.kind == ConvertKind::Enable {
                    ui.add_space(6.0);
                    ui.checkbox(
                        &mut d.ack,
                        tr("If I forget this passphrase, my library (tags, notes, history, \
                            playlists) is lost permanently. No recovery exists."),
                    );
                    note(
                        ui,
                        tr("Data already written in clear can stay readable on the disk. \
                            Also use full-disk encryption."),
                    );
                    note(
                        ui,
                        tr("Clear backups and clear thumbnails will be deleted. Thumbnails \
                            are rebuilt encrypted."),
                    );
                    note(
                        ui,
                        tr("JSON exports of your metadata stay in clear."),
                    );
                }
                // Over the limit: said while typing, and the form is refused.
                let live = (too_long(&d.current) || too_long(&d.new) || too_long(&d.again))
                    .then_some(FormError::TooLong);
                if let Some(e) = live.or(d.error) {
                    ui.add(
                        egui::Label::new(RichText::new(e.text()).size(13.0).color(DANGER)).wrap(),
                    );
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let label = match d.kind {
                        ConvertKind::Enable => tr("Encrypt"),
                        ConvertKind::Disable => tr("Turn off"),
                        ConvertKind::Change => tr("Change"),
                    };
                    if widgets::button_solid(ui, label).clicked() {
                        go = true;
                    }
                    if widgets::button_outline(ui, tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel || !open {
            // Buffers are wiped by their `Zeroizing` drop; egui's edit state
            // goes too.
            purge_edit_state(ctx);
            return;
        }
        if go {
            match validate(d.kind, &d.current, &d.new, &d.again, d.ack) {
                Err(e) => {
                    d.error = Some(e);
                    if e == FormError::TooLong {
                        // Nothing leaves the window and nothing stays in it.
                        d.current.zeroize();
                        d.new.zeroize();
                        d.again.zeroize();
                        purge_edit_state(ctx);
                    }
                }
                Ok(()) => {
                    let kind = d.kind;
                    let req = match kind {
                        ConvertKind::Enable => ConvertRequest {
                            kind,
                            pass: std::mem::replace(&mut d.new, secret_buf()),
                            new_pass: None,
                        },
                        ConvertKind::Disable => ConvertRequest {
                            kind,
                            pass: std::mem::replace(&mut d.current, secret_buf()),
                            new_pass: None,
                        },
                        ConvertKind::Change => ConvertRequest {
                            kind,
                            pass: std::mem::replace(&mut d.current, secret_buf()),
                            new_pass: Some(std::mem::replace(&mut d.new, secret_buf())),
                        },
                    };
                    match self.request_convert(req) {
                        Ok(()) => {
                            purge_edit_state(ctx);
                            self.settings.open = false;
                            return;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
            }
        }
        self.vault_ui.dialog = Some(d);
    }

    fn export_warning_window(&mut self, ctx: &egui::Context) {
        if self.vault_ui.export_warn.is_none() {
            return;
        }
        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;
        egui::Window::new(tr("Export in clear"))
            .open(&mut open)
            .id(egui::Id::new("vault-export-warning"))
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let nfo = self
                    .vault_ui
                    .export_warn
                    .as_ref()
                    .is_some_and(|(c, _)| *c == "meta.export_nfo");
                ui.add(
                    egui::Label::new(if nfo {
                        tr("These .nfo files are not encrypted, even though your library is. \
                            Anyone who can read them can read the titles, performers, tags and ratings they contain.")
                    } else {
                        tr("This JSON file is not encrypted, even though your library is. \
                           Anyone who can read the file can read what it contains.")
                    })
                    .wrap(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if widgets::button_solid(ui, tr("Export anyway")).clicked() {
                        proceed = true;
                    }
                    if widgets::button_outline(ui, tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if proceed {
            if let Some((cmd, Value::Object(mut a))) = self.vault_ui.export_warn.take() {
                a.insert("ack_clear".into(), json!(true));
                self.push(cmd, Value::Object(a));
            }
        } else if cancel || !open {
            self.vault_ui.export_warn = None;
        }
    }

    /// The Privacy page of the settings window.
    pub(super) fn settings_privacy(&mut self, ui: &mut egui::Ui) {
        widgets::eyebrow(ui, "", tr("Encrypted library"), "");
        let active = self.vault_active_flag();
        ui.label(if active {
            tr("Encryption is on.")
        } else {
            tr("Encryption is off.")
        });
        ui.add_space(4.0);
        if active {
            ui.horizontal_wrapped(|ui| {
                if widgets::button_outline(ui, tr("Lock library")).clicked() {
                    self.push("app.vault.lock", json!({}));
                }
                if widgets::button_outline(ui, tr("Change passphrase…")).clicked() {
                    self.vault_ui.open(ConvertKind::Change);
                }
                if widgets::button_outline(ui, tr("Turn off encryption…")).clicked() {
                    self.vault_ui.open(ConvertKind::Disable);
                }
            });
        } else {
            if widgets::button_outline(ui, tr("Encrypt library…")).clicked() {
                self.vault_ui.open(ConvertKind::Enable);
            }
            note(
                ui,
                tr(
                    "Encrypts the library database and the thumbnails with a passphrase. \
                    There is no recovery if you forget it.",
                ),
            );
        }
        if self.read_only {
            note(ui, FormError::Unavailable.text());
        }
        if active {
            ui.add_space(8.0);
            widgets::eyebrow(ui, "", tr("Automatic lock"), "");
            let mut minutes = self.cfg.autolock_minutes();
            ui.horizontal(|ui| {
                ui.label(tr("Lock after this many idle minutes (0 = never)"));
                if ui
                    .add(egui::DragValue::new(&mut minutes).range(0..=MAX_AUTOLOCK_MINUTES))
                    .changed()
                {
                    self.push("app.vault.settings", json!({ "autolock_minutes": minutes }));
                }
            });
            note(ui, tr("A video that is playing counts as activity."));
            let mut boss = self.cfg.boss_locks;
            if ui
                .checkbox(&mut boss, tr("The boss key also locks the library"))
                .changed()
            {
                self.push("app.vault.settings", json!({ "boss_locks": boss }));
            }
        }
    }

    fn vault_active_flag(&self) -> bool {
        self.opener.key.is_some()
    }
}

pub(super) fn register(reg: &mut jizzcraft_core::Registry<JizzApp>) {
    reg.register(
        "app.vault.settings",
        "Automatic lock and boss key: {autolock_minutes?: 0-1440, boss_locks?: bool}",
        |app, a| {
            if let Some(m) = a.get("autolock_minutes").and_then(Value::as_u64) {
                app.cfg.autolock_minutes = Some(m.min(u64::from(MAX_AUTOLOCK_MINUTES)) as u32);
            }
            if let Some(b) = a.get("boss_locks").and_then(Value::as_bool) {
                app.cfg.boss_locks = b;
            }
            app.save_cfg();
            Ok(json!({
                "autolock_minutes": app.cfg.autolock_minutes(),
                "boss_locks": app.cfg.boss_locks,
            }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConvertKind::*;

    #[test]
    fn enable_needs_length_match_and_the_box() {
        let long = "correct horse battery";
        assert_eq!(
            validate(Enable, "", "short", "short", true),
            Err(FormError::TooShort)
        );
        assert_eq!(
            validate(Enable, "", long, "other other other", true),
            Err(FormError::Mismatch)
        );
        assert_eq!(
            validate(Enable, "", long, long, false),
            Err(FormError::NotAcknowledged)
        );
        assert_eq!(validate(Enable, "", long, long, true), Ok(()));
        // Eleven characters is too short, twelve is enough; accents count once.
        assert_eq!(
            validate(Enable, "", "abcdefghijk", "abcdefghijk", true),
            Err(FormError::TooShort)
        );
        assert_eq!(
            validate(Enable, "", "éééééééééééé", "éééééééééééé", true),
            Ok(())
        );
    }

    #[test]
    fn disable_and_change_need_the_current_passphrase() {
        assert_eq!(
            validate(Disable, "", "", "", false),
            Err(FormError::EmptyCurrent)
        );
        assert_eq!(validate(Disable, "x", "", "", false), Ok(()));
        assert_eq!(
            validate(
                Change,
                "",
                "a long new passphrase",
                "a long new passphrase",
                false
            ),
            Err(FormError::EmptyCurrent)
        );
        assert_eq!(
            validate(Change, "old", "short", "short", false),
            Err(FormError::TooShort)
        );
        assert_eq!(
            validate(
                Change,
                "old",
                "a long new passphrase",
                "a long new passphrasf",
                false
            ),
            Err(FormError::Mismatch)
        );
        assert_eq!(
            validate(
                Change,
                "old",
                "a long new passphrase",
                "a long new passphrase",
                false
            ),
            Ok(())
        );
    }

    #[test]
    fn an_overlong_passphrase_is_refused_with_its_own_message() {
        let over = "x".repeat(MAX_PASSPHRASE_CHARS + 1);
        let ok = "y".repeat(MAX_PASSPHRASE_CHARS);
        assert_eq!(
            validate(Enable, "", &over, &over, true),
            Err(FormError::TooLong)
        );
        assert_eq!(
            validate(Disable, &over, "", "", false),
            Err(FormError::TooLong)
        );
        assert_eq!(validate(Enable, "", &ok, &ok, true), Ok(()));
        assert!(!FormError::TooLong.text().is_empty());
        assert_ne!(FormError::TooLong.text(), FormError::TooShort.text());
    }

    #[test]
    fn strength_counts_distinct_characters_without_a_set() {
        // Same verdicts as the set-based estimate, accents included.
        assert_eq!(strength("ééééééééééééééééééé"), Strength::Weak);
        assert!(strength("éàüöçñßøåæœ-2024-Zx!") >= Strength::Good);
        assert_eq!(strength("Tr0ub4dor&3-xylophone-9Q!"), Strength::Strong);
    }

    #[test]
    fn dialog_allocates_only_the_secrets_it_shows() {
        let d = Dialog::new(ConvertKind::Disable);
        assert!(d.current.capacity() >= MAX_PASSPHRASE_CHARS);
        assert_eq!((d.new.capacity(), d.again.capacity()), (0, 0));
        let d = Dialog::new(ConvertKind::Enable);
        assert_eq!(d.current.capacity(), 0);
        assert!(d.new.capacity() >= (MAX_PASSPHRASE_CHARS + 1) * 4);
    }

    #[test]
    fn strength_is_monotone_and_punishes_repeats() {
        assert_eq!(strength(""), Strength::Weak);
        assert_eq!(strength("aaaaaaaaaaaaaaaaaaaa"), Strength::Weak);
        assert_eq!(strength("password"), Strength::Weak);
        assert!(strength("correct horse battery staple") >= Strength::Good);
        assert_eq!(strength("Tr0ub4dor&3-xylophone-9Q!"), Strength::Strong);
        assert!(strength("abcdefghijkl") <= strength("abcdefghijklMNOP1234"));
    }

    #[test]
    fn autolock_counts_idle_time_and_a_playing_video_is_activity() {
        let t0 = Instant::now();
        let mut a = AutoLock::new(t0);
        let m = Duration::from_secs(60);
        assert!(!a.due(t0 + m * 14, 15, false));
        assert!(a.due(t0 + m * 15, 15, false));
        // 0 means never.
        assert!(!a.due(t0 + m * 10_000, 0, false));
        assert_eq!(a.remaining(t0, 0), None);
        // Input resets the clock.
        a.touch(t0 + m * 14);
        assert!(!a.due(t0 + m * 20, 15, false));
        assert!(a.due(t0 + m * 29, 15, false));
        // A video that is playing resets it too, every time it is asked.
        let mut a = AutoLock::new(t0);
        assert!(!a.due(t0 + m * 100, 15, true));
        assert!(!a.due(t0 + m * 110, 15, false));
        assert!(a.due(t0 + m * 115, 15, false));
        assert_eq!(a.remaining(t0 + m * 105, 15), Some(m * 10));
    }

    #[test]
    fn a_failed_finish_never_says_intact() {
        for k in [Enable, Disable, Change] {
            let t = finished_text(k, ConvertOutcome::FinishFailed);
            assert!(!t.contains("intact"));
            assert_ne!(t, finished_text(k, ConvertOutcome::Failed));
        }
    }

    #[test]
    fn one_fixed_sentence_per_outcome() {
        let all = [
            ConvertOutcome::Done,
            ConvertOutcome::Cancelled,
            ConvertOutcome::WrongPassphrase,
            ConvertOutcome::Busy,
            ConvertOutcome::FinishFailed,
            ConvertOutcome::Failed,
        ];
        for k in [Enable, Disable, Change] {
            for o in all {
                assert!(!finished_text(k, o).is_empty());
            }
        }
    }
}
