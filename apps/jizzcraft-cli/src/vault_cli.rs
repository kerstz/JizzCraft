//! Vault side of the CLI: secret input, exit codes, the opening of a library
//! that may be encrypted, and the `vault` subcommands.
//!
//! A passphrase comes from a terminal without echo, or from a file
//! descriptor / stdin. It is never an argument and never an environment
//! variable, and it is held in `Zeroizing` buffers. Messages here are fixed
//! strings: no secret, key, path or SQLCipher text.

use clap::Subcommand;
use jizzcraft_core::db::{FileFormat, LibraryOpener, probe_format};
use jizzcraft_core::vault::{self, KdfParams, ThumbKeys, VaultError, VaultKey};
use jizzcraft_core::{Db, Error, vault_convert};
use serde_json::{Value, json};
use std::io::{BufRead, IsTerminal, Read};
use std::path::Path;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Wrong passphrase.
pub const EXIT_WRONG_PASSPHRASE: u8 = 3;
/// Library is locked and no passphrase can be asked (non-interactive).
pub const EXIT_LOCKED: u8 = 4;
/// Vault header missing, corrupt or orphaned.
pub const EXIT_HEADER: u8 = 5;

/// Minimum length of a new passphrase, in characters.
pub const MIN_PASSPHRASE_CHARS: usize = 12;
/// Longest passphrase accepted, in characters: the same limit as the GUI.
pub const MAX_PASSPHRASE_CHARS: usize = vault::MAX_PASSPHRASE_CHARS;
/// Read cap in bytes: the longest passphrase in UTF-8 plus a line ending.
const MAX_PASSPHRASE_BYTES: u64 = (MAX_PASSPHRASE_CHARS as u64) * 4 + 2;
/// Attempts at a new passphrase on a terminal before giving up.
const PROMPT_ATTEMPTS: usize = 3;

const STDIN_TTY_REFUSED: &str =
    "--passphrase-stdin needs piped input: use the interactive prompt instead";

const NO_RECOVERY_WARNING: &str = "WARNING: there is no recovery. If you forget the passphrase, the library database\n\
and its encrypted caches are lost for good. Your media files are not touched,\n\
but tags, ratings, history and everything else in the database cannot be read\n\
again. Data written in clear before now may stay on disk (SSD, copy-on-write):\n\
use full-disk encryption if that matters. config.json and JSON exports stay in clear.";

/// An error with the process exit code it maps to.
#[derive(Debug)]
pub struct Fail {
    pub code: u8,
    pub msg: String,
}

impl Fail {
    pub fn new(code: u8, msg: impl Into<String>) -> Fail {
        Fail {
            code,
            msg: msg.into(),
        }
    }
}

impl From<String> for Fail {
    fn from(msg: String) -> Fail {
        Fail { code: 1, msg }
    }
}

impl From<&str> for Fail {
    fn from(msg: &str) -> Fail {
        Fail::new(1, msg)
    }
}

pub fn from_vault(e: VaultError) -> Fail {
    let code = match e {
        VaultError::WrongPassphrase => EXIT_WRONG_PASSPHRASE,
        VaultError::HeaderMissing
        | VaultError::HeaderCorrupt
        | VaultError::HeaderOrphan
        | VaultError::UnsupportedVersion
        | VaultError::ParamsOutOfBounds => EXIT_HEADER,
        _ => 1,
    };
    Fail::new(code, e.to_string())
}

pub fn from_core(e: Error) -> Fail {
    match e {
        Error::Vault(v) => from_vault(v),
        Error::Locked => Fail::new(EXIT_LOCKED, LOCKED_TEXT),
        other => Fail::new(1, other.to_string()),
    }
}

const LOCKED_TEXT: &str = "the library is locked: give the passphrase on a terminal, or with \
--passphrase-fd N or --passphrase-stdin";

/// Where secrets may come from, taken from the global flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sources {
    pub passphrase_fd: Option<i32>,
    pub passphrase_stdin: bool,
    pub new_passphrase_fd: Option<i32>,
}

type Secret = Zeroizing<String>;

fn read_stream(mut r: impl Read) -> Result<Secret, Fail> {
    let mut buf = Zeroizing::new(Vec::new());
    r.by_ref()
        .take(MAX_PASSPHRASE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| Fail::new(1, "cannot read the passphrase"))?;
    if buf.len() as u64 > MAX_PASSPHRASE_BYTES {
        return Err(Fail::new(1, "the passphrase is too long"));
    }
    // Exactly one line ending is removed, nothing else is trimmed.
    if buf.ends_with(b"\r\n") {
        let n = buf.len() - 2;
        buf.truncate(n);
    } else if buf.ends_with(b"\n") {
        let n = buf.len() - 1;
        buf.truncate(n);
    }
    let s = String::from_utf8(std::mem::take(&mut *buf))
        .map_err(|_| Fail::new(1, "the passphrase is not valid UTF-8"))?;
    let s = Zeroizing::new(s);
    if s.chars().count() > MAX_PASSPHRASE_CHARS {
        return Err(Fail::new(1, "the passphrase is too long"));
    }
    Ok(s)
}

#[cfg(unix)]
fn read_fd(fd: i32) -> Result<Secret, Fail> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return Err(Fail::new(1, "invalid file descriptor"));
    }
    // SAFETY: the caller of the CLI hands us ownership of this descriptor on
    // purpose (`--passphrase-fd N`); it is read once and closed.
    let f = unsafe { std::fs::File::from_raw_fd(fd) };
    read_stream(f)
}

#[cfg(not(unix))]
fn read_fd(_fd: i32) -> Result<Secret, Fail> {
    Err(Fail::new(
        1,
        "--passphrase-fd is not supported here, use --passphrase-stdin",
    ))
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn prompt(text: &str) -> Result<Secret, Fail> {
    eprint!("{text}");
    let r = rpassword::read_password();
    eprintln!();
    let s = r
        .map(Zeroizing::new)
        .map_err(|_| Fail::new(1, "cannot read the passphrase"))?;
    if s.chars().count() > MAX_PASSPHRASE_CHARS {
        return Err(Fail::new(1, "the passphrase is too long"));
    }
    Ok(s)
}

/// Descriptors 0, 1 and 2 are the standard streams, not a secret channel.
fn fd_refused(fd: i32) -> bool {
    (0..=2).contains(&fd)
}

/// `--passphrase-stdin` cannot set a new passphrase from a terminal: it would
/// be one silent entry with no confirmation.
pub fn stdin_flag_refused(passphrase_stdin: bool, stdin_is_terminal: bool) -> bool {
    passphrase_stdin && stdin_is_terminal
}

impl Sources {
    /// True when a descriptor or stdin was named.
    pub fn explicit(&self) -> bool {
        self.passphrase_fd.is_some() || self.passphrase_stdin
    }

    pub fn check(&self) -> Result<(), Fail> {
        if self.passphrase_fd.is_some() && self.passphrase_stdin {
            return Err(Fail::new(
                1,
                "use only one of --passphrase-fd and --passphrase-stdin",
            ));
        }
        if self.passphrase_fd.is_some() && self.passphrase_fd == self.new_passphrase_fd {
            return Err(Fail::new(
                1,
                "--passphrase-fd and --new-passphrase-fd must differ",
            ));
        }
        for fd in [self.passphrase_fd, self.new_passphrase_fd]
            .into_iter()
            .flatten()
        {
            if fd_refused(fd) {
                return Err(Fail::new(
                    1,
                    "descriptors 0, 1 and 2 are not accepted: use --passphrase-stdin for standard input",
                ));
            }
        }
        if self.passphrase_stdin && self.new_passphrase_fd == Some(0) {
            return Err(Fail::new(
                1,
                "--passphrase-stdin and --new-passphrase-fd 0 read the same stream",
            ));
        }
        Ok(())
    }

    /// The current passphrase. `Ok(None)` when nothing can supply it without
    /// a terminal (the caller decides which exit code that is).
    pub fn current(&self, what: &str) -> Result<Option<Secret>, Fail> {
        if let Some(fd) = self.passphrase_fd {
            return read_fd(fd).map(Some);
        }
        if self.passphrase_stdin && !std::io::stdin().is_terminal() {
            return read_stream(std::io::stdin().lock()).map(Some);
        }
        if interactive() {
            return prompt(what).map(Some);
        }
        Ok(None)
    }

    /// Refuse `--passphrase-stdin` when stdin is a terminal, for the commands
    /// that set a new passphrase (the prompt asks twice, the flag would not).
    pub fn check_new(&self) -> Result<(), Fail> {
        if stdin_flag_refused(self.passphrase_stdin, std::io::stdin().is_terminal()) {
            return Err(Fail::new(1, STDIN_TTY_REFUSED));
        }
        Ok(())
    }

    /// A new passphrase for `enable` or `change-passphrase`: from
    /// `--new-passphrase-fd`, else (`allow_primary`) the primary source, else
    /// a terminal asked twice.
    pub fn new_passphrase(&self, allow_primary: bool) -> Result<Secret, Fail> {
        let s = if let Some(fd) = self.new_passphrase_fd {
            read_fd(fd)?
        } else if allow_primary && self.explicit() {
            self.current("")?
                .ok_or_else(|| Fail::new(1, "no passphrase given"))?
        } else if interactive() {
            self.ask_twice()?
        } else {
            return Err(Fail::new(
                1,
                "a new passphrase is required: use a terminal or --new-passphrase-fd N",
            ));
        };
        too_short(&s)?;
        Ok(s)
    }

    /// Ask on the terminal, twice, again when the entries differ or are too
    /// short (a typo on a single silent entry would lock the library).
    fn ask_twice(&self) -> Result<Secret, Fail> {
        for attempt in 1..=PROMPT_ATTEMPTS {
            let a = prompt("New passphrase: ")?;
            if too_short(&a).is_err() {
                eprintln!("The passphrase must have at least {MIN_PASSPHRASE_CHARS} characters.");
                continue;
            }
            let b = prompt("Repeat the new passphrase: ")?;
            if *a == *b {
                return Ok(a);
            }
            if attempt < PROMPT_ATTEMPTS {
                eprintln!("The two entries differ, try again.");
            }
        }
        Err(Fail::new(1, "the two entries differ"))
    }
}

fn too_short(s: &str) -> Result<(), Fail> {
    if s.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(Fail::new(
            1,
            format!("the passphrase must have at least {MIN_PASSPHRASE_CHARS} characters"),
        ));
    }
    Ok(())
}

/// An opened library and, in vault mode, the keys derived from it.
pub struct Opened {
    pub db: Db,
    pub thumb_keys: Option<Arc<ThumbKeys>>,
}

/// Recover any interrupted conversion, ask for the passphrase when the library
/// is encrypted, and open it through a `LibraryOpener`. Locked and not able to
/// ask: exit 4 with a fixed sentence.
pub fn open_library(db_path: &Path, src: &Sources) -> Result<Opened, Fail> {
    src.check()?;
    let fmt = vault_convert::recover(db_path).map_err(from_vault)?;
    if fmt != FileFormat::Encrypted {
        let db = LibraryOpener::new(db_path, None)
            .open()
            .map_err(from_core)?;
        return Ok(Opened {
            db,
            thumb_keys: None,
        });
    }
    let pass = src
        .current("Passphrase: ")?
        .ok_or_else(|| Fail::new(EXIT_LOCKED, LOCKED_TEXT))?;
    let key = vault_convert::unlock_library(db_path, &pass).map_err(from_vault)?;
    open_with_key(db_path, &key)
}

fn open_with_key(db_path: &Path, key: &VaultKey) -> Result<Opened, Fail> {
    let opener = LibraryOpener::new(db_path, Some(Arc::new(key.db_key())));
    let db = opener.open().map_err(from_core)?;
    Ok(Opened {
        db,
        thumb_keys: Some(Arc::new(key.thumb_keys())),
    })
}

#[derive(Subcommand)]
pub enum VaultCmd {
    /// Say whether the library is encrypted (never asks for a passphrase).
    Status,
    /// Encrypt the library. There is no recovery if the passphrase is lost.
    Enable {
        /// Required without a terminal: acknowledges that a lost passphrase
        /// means a lost library database.
        #[arg(long)]
        i_understand_no_recovery: bool,
    },
    /// Decrypt the library back to a plain database.
    Disable,
    /// Re-wrap the key under a new passphrase (the data is not rewritten).
    ChangePassphrase,
    /// Unlock and check the encrypted database end to end.
    Verify,
}

fn emit(json_out: bool, v: Value, human: &str) {
    if json_out {
        println!("{v}");
    } else {
        println!("{human}");
    }
}

fn header_state(db_path: &Path) -> &'static str {
    let h = vault::header_path(db_path);
    match std::fs::read(&h) {
        Ok(b) if vault::validate_header(&b).is_ok() => "ok",
        Ok(_) => "corrupt",
        Err(_) if vault::valid_header_exists(db_path) => "ok",
        Err(_) => "missing",
    }
}

/// Run one `vault` subcommand. Returns the exit code on success.
pub fn run(cmd: &VaultCmd, db_path: &Path, src: &Sources, json_out: bool) -> Result<(), Fail> {
    src.check()?;
    match cmd {
        VaultCmd::Status => {
            // Read-only look at the disk: no recovery, no passphrase.
            let fmt = probe_format(db_path).map_err(from_core)?;
            let header = header_state(db_path);
            let (state, fail): (&str, Option<VaultError>) = match (fmt, header) {
                (FileFormat::Encrypted, "ok") => ("locked", None),
                (FileFormat::Encrypted, "corrupt") => {
                    ("header damaged", Some(VaultError::HeaderCorrupt))
                }
                (FileFormat::Encrypted, _) => ("header missing", Some(VaultError::HeaderMissing)),
                (_, "ok") => ("header orphaned", Some(VaultError::HeaderOrphan)),
                _ => ("off", None),
            };
            emit(
                json_out,
                json!({
                    "encrypted": fmt == FileFormat::Encrypted,
                    "state": state,
                    "header": header,
                }),
                &format!("vault: {state}"),
            );
            match fail {
                Some(e) => Err(from_vault(e)),
                None => Ok(()),
            }
        }
        VaultCmd::Enable {
            i_understand_no_recovery,
        } => {
            src.check_new()?;
            match vault_convert::recover(db_path).map_err(from_vault)? {
                FileFormat::Encrypted => return Err(from_vault(VaultError::AlreadyEncrypted)),
                FileFormat::Absent => return Err(Fail::new(1, "there is no library to encrypt")),
                FileFormat::Plain => {}
            }
            eprintln!("{NO_RECOVERY_WARNING}");
            if !*i_understand_no_recovery {
                if !interactive() {
                    return Err(Fail::new(
                        1,
                        "refused: pass --i-understand-no-recovery to confirm without a terminal",
                    ));
                }
                eprint!("Type YES to encrypt the library: ");
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .map_err(|_| Fail::new(1, "cannot read the answer"))?;
                if line.trim() != "YES" {
                    return Err(Fail::new(1, "cancelled, nothing was changed"));
                }
            }
            let pass = src.new_passphrase(true)?;
            vault_convert::enable(db_path, &pass, KdfParams::default()).map_err(from_vault)?;
            emit(
                json_out,
                json!({ "enabled": true }),
                "vault: on. The library is encrypted.",
            );
            Ok(())
        }
        VaultCmd::Disable => {
            let pass = needed(src, db_path)?;
            vault_convert::disable(db_path, &pass).map_err(from_vault)?;
            emit(
                json_out,
                json!({ "enabled": false }),
                "vault: off. The library is no longer encrypted.",
            );
            Ok(())
        }
        VaultCmd::ChangePassphrase => {
            src.check_new()?;
            let old = needed(src, db_path)?;
            let new = src.new_passphrase(false)?;
            vault_convert::change_passphrase(db_path, &old, &new).map_err(from_vault)?;
            emit(
                json_out,
                json!({ "changed": true }),
                "The passphrase was changed. Copies of the old header stay valid with the old passphrase.",
            );
            Ok(())
        }
        VaultCmd::Verify => {
            let pass = needed(src, db_path)?;
            let key = vault_convert::unlock_library(db_path, &pass).map_err(from_vault)?;
            let o = open_with_key(db_path, &key)?;
            o.db.cipher_integrity_check().map_err(|_| {
                Fail::new(
                    1,
                    "the encrypted library failed its page authentication check",
                )
            })?;
            o.db.quick_check()
                .map_err(|_| Fail::new(1, "the encrypted library failed its integrity check"))?;
            let items = o.db.stats().map_err(from_core)?.total;
            emit(
                json_out,
                json!({ "ok": true, "items": items }),
                &format!("vault: ok ({items} items)"),
            );
            Ok(())
        }
    }
}

/// The current passphrase for a command that only makes sense on an encrypted
/// library; exit 4 when it cannot be asked.
fn needed(src: &Sources, db_path: &Path) -> Result<Secret, Fail> {
    match vault_convert::recover(db_path).map_err(from_vault)? {
        FileFormat::Encrypted => {}
        _ => return Err(from_vault(VaultError::NotEncrypted)),
    }
    src.current("Passphrase: ")?
        .ok_or_else(|| Fail::new(EXIT_LOCKED, LOCKED_TEXT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_ending_is_removed() {
        let r = |b: &[u8]| read_stream(b).unwrap().to_string();
        assert_eq!(r(b"pass\n"), "pass");
        assert_eq!(r(b"pass\r\n"), "pass");
        assert_eq!(r(b"pass\n\n"), "pass\n");
        assert_eq!(r(b"pass \n"), "pass ");
        assert_eq!(r(b"pass"), "pass");
    }

    #[test]
    fn oversized_input_is_refused() {
        let big = vec![b'a'; 5000];
        assert!(read_stream(&big[..]).is_err());
    }

    #[test]
    fn limit_is_1024_characters_not_bytes() {
        assert_eq!(MAX_PASSPHRASE_CHARS, 1024);
        let ok = "a".repeat(1024);
        assert!(read_stream(ok.as_bytes()).is_ok());
        let long = "a".repeat(1025);
        assert!(read_stream(long.as_bytes()).is_err());
        // 1024 four-byte characters are accepted: bytes are not the limit.
        let wide = "\u{1F600}".repeat(1024);
        assert_eq!(wide.len(), 4096);
        assert!(read_stream(wide.as_bytes()).is_ok());
        let wider = "\u{1F600}".repeat(1025);
        assert!(read_stream(wider.as_bytes()).is_err());
    }

    #[test]
    fn standard_descriptors_are_refused() {
        for fd in [0, 1, 2] {
            let s = Sources {
                passphrase_fd: Some(fd),
                ..Default::default()
            };
            assert!(s.check().is_err(), "--passphrase-fd {fd}");
            let s = Sources {
                new_passphrase_fd: Some(fd),
                ..Default::default()
            };
            assert!(s.check().is_err(), "--new-passphrase-fd {fd}");
        }
        let s = Sources {
            passphrase_fd: Some(3),
            new_passphrase_fd: Some(4),
            ..Default::default()
        };
        assert!(s.check().is_ok());
    }

    #[test]
    fn stdin_flag_on_a_terminal_is_refused_for_new_passphrases() {
        assert!(stdin_flag_refused(true, true));
        assert!(!stdin_flag_refused(true, false));
        assert!(!stdin_flag_refused(false, true));
    }
}
