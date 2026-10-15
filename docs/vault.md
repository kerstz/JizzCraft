# Encrypted vault (issue #13)

**Status: alpha, delivered, off by default.** A library is encrypted only if
you turn the vault on (Settings > Privacy > *Encrypt library*, or
`jizzcraft-cli vault enable`); otherwise the library database and the
thumbnail cache are stored in clear, exactly as before. This page records the
promise, the threat model and what each step has shipped. All steps are now
delivered; the feature stays labelled alpha until it has had real-world use on
Windows and macOS (see the limits in each step).

| Step | Content | State |
| --- | --- | --- |
| 0 | This document, `SECURITY.md`, README wording | done |
| 1 | SQLite replaced by SQLCipher (same behaviour, nothing encrypted) | done |
| 2 | Key derivation and header file (pure crypto, `core::vault`) | done |
| 3 | Opening the database with a key | done |
| 4 | Plain to encrypted conversion, crash recovery (engine only) | done |
| 5 | Encrypted thumbnails and previews | done |
| 6 | CLI commands, canary test | done |
| 7a | Lock screen, unlocking, locking, keyed opening in the app | done |
| 7b | Settings (enable, disable, change), conversion as a job, auto-lock | done |
| 8 | Perf in vault mode, packaging smoke test, close guard, M3U8 warning, troubleshooting | done |

The desktop app and the command line can both enable, disable and change the
passphrase (steps 7b and 6, below). Nothing is encrypted until you do.

## Threat model

### What the vault will protect

Confidentiality and integrity **at rest** of:

- `library.db` and its `-wal` file: paths, names, tags, categories, notes,
  markers, ratings, history, playlists and the full-text index;
- the `pre-vN` backups of the database taken before a migration;
- thumbnails and previews in the cache directory.

Attacker: someone holding a stolen or cloned disk, or a copy of a backup, while
the application is closed or locked.

### What it will not protect

- **The media files themselves.** JizzCraft never touches them; encrypt the
  volume if they matter.
- **A running, unlocked machine.** Malware or an administrator on an unlocked
  session can read the key from memory or the decrypted view on screen.
- **Memory, swap and hibernation images.** Keys and decrypted rows pass through
  RAM and may be paged out.
- **Operating-system artefacts**: `recently-used.xbel`, `~/.cache/thumbnails`
  written by other programs, shell history, desktop search indexes.
- **Size and number of cache files** (their count and length leak roughly how
  large the library is).
- **`config.json`, `fetch.json` and the online `state_dir`.** They are out of
  scope for the first version; a follow-up issue tracks them.
- **Data written in clear before the vault is enabled.** Flash storage and
  copy-on-write filesystems (SSD, btrfs) may keep old blocks. Use full-disk
  encryption if that matters.
- **Weak passphrases, evil-maid attacks, coercion.**
- **Rollback.** Replacing `library.db` with an older valid copy is not
  detected.
- **JSON exports and `.m3u8` playlists** remain in clear (the app asks for
  confirmation before writing either while the vault is on; the CLI prints no
  warning). A playlist file lists the paths of your media.

### Forgotten passphrase

**There is no recovery.** No key, hint, hash of the passphrase or key file is
stored anywhere, and there is no backdoor. If the passphrase is lost the
library database and the encrypted caches are permanently unreadable. Your
media files are untouched and can be imported again into a new library, but
tags, ratings, history and every other piece of data in the database are lost.

## Design

- Database: SQLCipher (AES-256, page level) with a raw 32-byte key given as
  `PRAGMA key = "x'...'"`. FTS5 and the schema are unchanged; there is no
  schema migration for the vault.
- Key hierarchy: a random 32-byte data key (DEK) is wrapped by a key derived
  from the passphrase with Argon2id. Sub-keys for the database and for
  thumbnails are derived from the DEK with `blake3::derive_key` and fixed
  context strings.
- Thumbnails: one XChaCha20-Poly1305 file per thumbnail under
  `cache/vault-thumbs/`, with names that are keyed hashes, so neither content
  nor path appears on disk.
- Changing the passphrase re-wraps the DEK only. A copy of the old header file
  stays openable with the old passphrase (see "Changing the passphrase does
  not rotate the key" below).
- No homemade cryptography: only audited primitives from maintained crates.
- Secrets never go through argv, environment variables, `--cmd` JSON, logs,
  toasts, errors or crash reports.

### Header file `library.vault` (version 1, 111 bytes, little-endian)

It sits next to `library.db`.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | magic `JZVAULT\0` |
| 8 | 2 | format version, `1` |
| 10 | 1 | KDF id, `1` = Argon2id v0x13 |
| 11 | 4 | memory cost, KiB |
| 15 | 4 | time cost (iterations) |
| 19 | 4 | parallelism |
| 23 | 16 | salt |
| 39 | 24 | XChaCha20-Poly1305 nonce |
| 63 | 48 | wrapped DEK (32 bytes) + Poly1305 tag (16 bytes) |

The fields add up to 111 bytes (an earlier draft of this page said 135, which
was an arithmetic slip; the field table above is the format).

The associated data of the AEAD is the first 39 bytes. Defaults: 64 MiB
(65536 KiB), 3 iterations, 1 lane. Parameters read from a file are bounded
(memory 19456 to 1048576 KiB, time 2 to 10, lanes 1 to 8) so a hostile header
cannot exhaust memory. The passphrase is NFC-normalised and never trimmed.
A wrong passphrase fails the AEAD check; a good key that SQLCipher still
refuses means a corrupt database.

### Changing the passphrase does not rotate the key

`change-passphrase` unwraps the data key (DEK) and wraps the **same** DEK under
the new passphrase with a fresh salt and nonce. The database is not
re-encrypted. Consequences:

- Anyone who kept a copy of the old `library.vault` (a backup, a cloud-synced
  folder, a disk image) together with a copy of the database can still open
  that database with the **old** passphrase, and so can anyone who learned the
  old passphrase and obtains the old header. The change protects the live
  header only.
- The DEK, and therefore the database key and the thumbnail keys, stay the
  same for the life of the vault.
- **Remedy when the old passphrase or an old header copy is compromised:**
  `vault disable` (decrypts to a plain database), then `vault enable` with the
  new passphrase. That draws a new random DEK. Do it on a machine you trust,
  and destroy the old header copies and old backups: they remain valid for the
  old data.

### Concurrency: `library.lock`

`enable`, `disable`, `change-passphrase`, `recover` and `unlock_library` take
an exclusive advisory lock on `library.lock` (a zero-byte file, mode 0600, next
to the database; `File::try_lock`, `flock` on Unix and `LockFileEx` on
Windows) for their whole duration. When another conversion holds it, the call
returns `Busy` and **deletes nothing**: a `recover` started in parallel with an
`enable` used to be able to remove `library.vault.new` right after the commit
rename and leave an encrypted database without its header. The lock is
released by the operating system when the process dies, so a `kill -9` never
leaves it held. The file is kept after the operation (empty). Where the lock
file cannot be created (read-only directory) the operation is not locked, and
no conversion can happen there anyway.

If the header rename that follows the commit fails, the engine writes the
header again from the bytes it still holds in memory (`library.vault.new`, then
rename). Only if that fails too does `enable` return `FinishFailed` ("the
library is encrypted but the key file was not finalised: unlock it again, do
not delete library.vault.new"); unlocking then finishes the job. That error is
never presented as "library intact": the commit point has passed.

### Permissions

On Unix, directories created by the engine (library, data, cache) are 0700 and
the sensitive files are 0600: `library.db` and its `-wal`, `-shm` and
`-journal`, `library.vault` and its `.new` / `.disabling` files,
`library.lock`, the conversion copies, every `pre-*` backup, the `backup` copy
and its header, JSON exports and crash notes. A library written by an older
build with mode 0644 is tightened the next time it is opened. Existing
directories (a folder you pointed `--db` at) are never changed. Windows ACLs
are those inherited from the user profile; nothing is set explicitly.

### Cache directory of a conversion

`enable` and `disable` delete the clear thumbnail caches (`thumbs/`,
`previews/`), and `disable` deletes `vault-thumbs/`. The directory comes from
`ConvertOptions::cache_dir` when the caller sets it. Otherwise the global cache
(`--home`, or the default location) is used only when the library is the one
the process resolves by itself; a library opened with `--db` or
`JIZZCRAFT_DB` uses a `cache/` directory next to it, so converting one
library never empties the cache of another. Limit: with `--db`, clear
thumbnails the CLI wrote to the global cache before the conversion stay there
(use `cache.clear` or `--home`).

## Step 1: SQLCipher, and what changed

`rusqlite` now uses the feature `bundled-sqlcipher-vendored-openssl`:
SQLCipher 4.14 (community, SQLite 3.51.3 inside) is compiled in, with a
vendored OpenSSL (`openssl-src`, statically linked) as its crypto provider.
Behaviour is unchanged: databases are still plain files, and nothing calls
`PRAGMA key` yet.

Consequences worth knowing:

- **Build requirements**: a C compiler, `make` and `perl` (plus NASM on
  Windows/MSVC). They are present on the GitHub Linux and macOS runners and
  Windows has Strawberry Perl; CI installs NASM. Distribution recipes list
  them.
- **SQLite is older.** SQLCipher 4.14 embeds SQLite 3.51.3, older than the
  3.53.x that plain `bundled` shipped. Future migrations must not use SQL
  features newer than 3.51.
- **`openssl-sys` is now in the default tree**, only as a dependency of
  `libsqlite3-sys`. `crates/core/tests/offline_deps.rs` fails if anything
  else depends on it, and `openssl`, `native-tls` and every HTTP/TLS crate
  stay banned. It is a crypto library, not a network one.
- **Licences**: OpenSSL 3 (Apache-2.0) and SQLCipher (BSD-3-Clause). They are
  build dependencies for `cargo about`, so they are listed by hand in
  `NOTICE` and `ATTRIBUTION.md`.
- **Advisories**: `openssl-src` is tracked by `cargo deny` like any crate;
  there is no global ignore for it.

### `OPENSSL_NO_VENDOR=1` (distribution packagers)

Checked on Arch with OpenSSL 3.6 and the dev headers installed:
`OPENSSL_NO_VENDOR=1 cargo build -p jizzcraft-core` skips the vendored build
(the build takes seconds instead of compiling OpenSSL) and `openssl-sys` links
the system `libssl` and `libcrypto` dynamically (`cargo:rustc-link-lib=ssl`,
`crypto`, headers from `/usr/include`). The test-suite of `offline_deps`
passes in that mode. Notes:

- The result then needs the system `libcrypto` at runtime (package
  dependency `openssl-libs` / `libssl3` / `openssl`). With the default
  vendored build there is no runtime dependency.
- `OPENSSL_DIR`, `OPENSSL_LIB_DIR` and `OPENSSL_INCLUDE_DIR` apply as usual.
- The Flatpak, AppImage, Windows zip and release tarballs use the vendored
  build. Only distribution packages built against the distro's own OpenSSL
  should set the variable.
- Not tested: macOS and Windows. The Windows build (MSVC and the zig
  cross-check) could not be run from the development machine.

## Steps 2 and 3: key handling and keyed opening (engine only)

Code only; nothing in the application or the CLI calls it yet, so no library
is encrypted by this build.

Lot 5 (`jizzcraft_media::thumbs::ThumbStore`, `Plain | Vault`): thumbnails live
in `cache/vault-thumbs/xx/<keyed name>.jzt` (`"JZT1" || nonce || ciphertext`,
AAD `"JZT1" || name`), scrub previews in `.jzp` files of the same directory
with names bound to the bucket. Writes go through a temporary file and
`fsutil::replace`. A file that fails to authenticate or decode is deleted and
treated as absent. In vault mode nothing is written under `thumbs/` or
`previews/`. `ThumbService::with_keys` and `PreviewService::with_keys` take
`Option<Arc<ThumbKeys>>`; `prune_store` and `clear_cache` cover `vault-thumbs`.

- `jizzcraft_core::vault`: header encode/decode, Argon2id, key envelope, the
  three BLAKE3 sub-keys, `ThumbCipher`. The reference vector in its tests is
  frozen: a change there is a format break.
- `Db::open_with(path, Option<&DbKey>)` and `open_read_only_with`. The key is
  set before any read. The mode comes from the first 16 bytes of the file
  (`db::probe_format`), never from a setting. An encrypted file without a key
  is `Error::Locked`; a wrong key is `VaultError::DbCorrupt`; a key on a clear
  file is `VaultError::NotEncrypted`. SQLCipher's own logging is switched off.
- `LibraryOpener { path, key }` is what jobs will carry (still `None` at every
  call site today).
- Copies stay encrypted: `pre-vN` backups and `portability::backup` use
  `VACUUM INTO`, which inherits the key (tested, as is `ATTACH` without a
  key). `backup` also copies the header next to the copy, with the extension
  replaced by `vault` (`copy.db` gets `copy.vault`), and refuses when the
  library has no readable header.
- A `pre-vN` backup of an encrypted library is opened with the same key; it
  has no header of its own because the key is the same.
- JSON exports are still in clear.
- Passphrase hashing is slow on purpose (about 0.1 to 0.5 s at the default
  cost); callers must run it off the UI thread. Memory held by Argon2 inside
  the crate is not zeroed by this code.

## Step 4: conversion and recovery (engine only)

`jizzcraft_core::vault_convert`: `enable`, `disable`, `change_passphrase`,
`recover` and `unlock_library`. Nothing in the application or the CLI calls
them yet, so no shipped build encrypts a library. Everything is slow (Argon2id,
a full copy): call it from a job, never from the UI thread.

Order of `enable` (`disable` mirrors it with `ATTACH ... KEY ''`):

1. `wal_checkpoint(TRUNCATE)`, `journal_mode = DELETE`, then the exclusive lock
   is held for the whole conversion (`locking_mode = EXCLUSIVE`). If another
   connection or process holds the library the answer is `Busy` and nothing
   changed.
2. `library.vault.new` written and synced.
3. `ATTACH 'library.db.enc-tmp' KEY x'..'` and `sqlcipher_export`, then
   `user_version`.
4. The copy is verified as a stranger would open it: key accepted,
   `cipher_integrity_check`, `quick_check`, row count of every table,
   `user_version`, FTS5 `integrity-check`. Any mismatch is `VerifyFailed`.
5. File and directory are synced; a `-wal` or `-journal` next to the target
   aborts.
6. **Commit point**: `rename(library.db.enc-tmp, library.db)`.
7. `library.vault.new` renamed to `library.vault`.
8. Clear `pre-vN` backups and clear `cache/thumbs`, `cache/previews` deleted.
   (Encrypted thumbnails are step 5; here caches are only removed.) Disable
   deletes `library.vault`, the encrypted `pre-vN` backups and
   `cache/vault-thumbs`.

`recover(db_path)` must run before every open. It decides from disk state:

| `library.db` | Leftovers | Action |
| --- | --- | --- |
| plain | `enc-tmp`, `plain-tmp`, `.vault.new`, unreadable `.vault` | delete |
| plain | `.vault.disabling` marker (interrupted disable, after commit) | delete `.vault`, encrypted `pre-vN`, `vault-thumbs` and the marker |
| plain | valid `.vault`, **no** marker | `HeaderOrphan`, nothing is deleted: the encrypted library was lost or replaced by a clear copy |
| absent | valid `.vault` or `.vault.new` | `HeaderOrphan`, nothing is deleted; `Db::open` refuses to create a new library |
| encrypted | `.vault.disabling` | delete the marker (the disable never committed) |
| encrypted | `enc-tmp`, `plain-tmp` | delete |
| encrypted | no `.vault`, valid `.vault.new` | rename `.new` to `.vault` |
| encrypted | no `.vault`, no usable `.new` | `HeaderMissing`, nothing is deleted |
| encrypted | `.vault` and a damaged `.new` | delete `.new` |
| encrypted | `.vault` and a valid `.new` | both kept; `unlock_library` tries the current header first and keeps the one that opens |
| encrypted | header present | delete clear `pre-vN` and clear caches |

`disable` writes `library.vault.disabling` (file and directory synced) before
the commit point and removes it as its last step. The marker is the only
authority for `recover` to delete the header, the encrypted backups and
`vault-thumbs` of a library found clear. Without it a clear library next to a
valid header is never "cleaned": the header is the only envelope of the key, and
the encrypted file may simply be restored.

Changing the passphrase rewrites only the header: the new one goes to
`library.vault.new` (synced), then an atomic rename. A copy of the old header
still opens with the old passphrase.

Limits and honest caveats:

- Until step 8 finishes (or `recover` runs) the old clear copy and the clear
  caches are still on disk, and deleting a file does not wipe flash or
  copy-on-write storage: use full-disk encryption if that matters.
- Another process that had the library open before the conversion but held no
  lock can keep writing to the old, unlinked file after the rename on
  Unix; on Windows the lock is dropped before the rename (a file cannot be
  renamed while open), which leaves a short window. Close other instances
  first.
- `recover` is not a lock: do not run it while a conversion is in progress in
  another process.
- Whole-file rollback to an older valid copy is still not detected.
- Tests inject a fail point after every step and a `kill -9` of a converting
  child process; they check that only "all clear with every row" or "all
  encrypted with every row" can result, including the FTS content.

## Step 7a: lock screen and keyed opening in the desktop app

`jizzcraft_ui::root::Root` is what eframe runs. It wraps the real app in a
state machine, `Gate`, with `Locked`, `Unlocking`, `Open` and `Locking`
(plus a `Failed` error screen):

- **Start-up.** `vault_convert::recover`, then the first 16 bytes decide. A
  library that is not encrypted opens exactly as before. An encrypted one
  shows the lock screen and nothing else runs: no quick check, no cache
  prune, no codec check, no `--import` or `--cmd` until the key opened the
  library. A missing, damaged or orphaned header is an error screen with a
  fixed translated sentence; no new library is ever created next to it.
- **Lock screen.** Neutral: the word "Locked", a masked field, "Unlock" and
  "Exit". No counts, names or brand; the window title is `Viewer` and the
  icon the neutral one, whatever the identity. It is translated in the six
  languages.
- **Unlocking.** The passphrase leaves the field in a `Zeroizing` buffer
  of fixed capacity (no reallocation while typing) and goes to a worker
  thread: Argon2id and the first database open never run on the UI thread.
  The window shows "Unlocking…". Only the derived database and thumbnail keys
  (behind `Arc`s) leave the worker; the data key is dropped there. A wrong
  passphrase gives the fixed sentence "Wrong passphrase." and wipes the
  buffer. After three wrong attempts in a row the field waits 1, 2, 4 … 30
  seconds; the counter lives in memory only and is not a defence against an
  attacker who has the files (Argon2id is).
- **Locking.** Command `app.vault.lock` (also `vault.lock`, menu "Lock
  library" under Privacy, shortcut `Ctrl+Shift+L`). The window is hidden at
  once and shows "Locking…". The app cancels its jobs through their flags
  (scans stop between rows, fingerprint and integrity checks, folder removal),
  saves the playback position and hands the player to its control thread
  (`Player::drop` sends Stop, nothing is set to Null from the UI). Only when
  every job holding a connection has ended (a ticket counter, released even on
  a panic) is the app dropped: database connection, textures, thumbnail
  service and previews go, and the last `Arc` to the key zeroes it. Then
  "Locked" appears. A file dialog left open inside a backup or import job
  therefore delays the end of the lock; the content is hidden meanwhile.
- **Every open goes through `LibraryOpener`**: start-up, scans, jobs,
  folder removal, the start-up quick check. Thumbnails and scrub previews use
  `with_keys`. There is no `Db::open(path)` without a key in `crates/ui`.
  The start-up cache prune is size-only and covers `vault-thumbs`.

Settings, conversion and auto-lock are step 7b, next section.

Limits, honestly:

- egui keeps text-layout caches and its own edit state; the typed passphrase
  is wiped from our buffer and the edit state is dropped after each attempt,
  but copies in the toolkit's memory, in the allocator or in swap are not
  tracked (RAM is out of scope, see the threat model).
- The Argon2 working memory is zeroed on drop (`argon2` built with its
  `zeroize` feature); the passphrase copies we hold are `Zeroizing`.
- The state machine and the unlocking logic are unit-tested without a window
  (`crates/ui/src/root.rs`, `crates/ui/tests/vault_open.rs`, and a test that
  builds the real app without a window and drops it, checking that the key is
  freed). The window itself, the visual aspect of the lock screen and a real
  lock with a playing video were not seen on screen in this step.
- No `xtask e2e` scenario was added: that harness drives loopback servers and
  the player and does not depend on the UI crate; the integration test above
  covers the same opening logic with a library encrypted by the same engine
  the CLI uses.

## Step 7b: settings, conversion as a job, auto-lock

Settings > Privacy (both editions):

- **Encrypt library.** A dialog asks the passphrase twice (12 characters
  minimum, counted as characters; an estimate of the strength is computed on
  this machine and shown, nothing is sent). A box has to be ticked: "If I
  forget this passphrase, my library (tags, notes, history, playlists) is lost
  permanently. No recovery exists." Notes say that data already written in
  clear can stay readable on the disk (use full-disk encryption too), that
  clear backups and clear thumbnails are deleted and rebuilt encrypted, and that
  JSON exports stay in clear.
- **Change passphrase** and **Turn off encryption** ask for the current
  passphrase again even though the vault is open (an unattended unlocked
  window is not enough to weaken the vault).
- **Automatic lock**: minutes of inactivity (default 15, `0` = never, at most
  1440). Any input is activity, and so is a video that is playing. When the
  time is up the app takes the path of `app.vault.lock`. Stored as
  `autolock_minutes` in the clear `config.json`.
- **The boss key also locks the library** (`boss_locks`, off by default).
- The settings command is `app.vault.settings {autolock_minutes?, boss_locks?}`.
  No command takes a passphrase.

The conversion is a state of the root envelope, `Converting`:

```text
Open(app) --confirm--> Quiesce(app) --jobs ended, app dropped--> Running(worker)
     ^                                                             |
     +--------- Open(app, library opened again, one toast) <-------+
```

1. The app refuses the request with a fixed sentence while any job holds the
   library ("Wait for running tasks to finish, then try again.").
2. The root cancels the app's jobs, waits for their tickets, and drops the app
   so the connection (and WAL) are closed before the engine takes its
   exclusive lock. While it runs the window shows only "Encrypting…" (or
   "Decrypting…", "Changing passphrase…"): no other job can start.
3. A worker thread runs `vault_convert::enable_with`, `disable_with` or
   `change_passphrase_with` (Argon2id included; the UI thread never hashes),
   then opens the library again: with the new key after an enable or a change,
   clear after a disable, and with the keys it already had when the
   conversion was refused (wrong passphrase, busy, cancelled).
4. **Cancel** is offered until the commit point. The engine checks the flag
   before staging, before the export, before verifying and a last time right
   before the rename, then sets `committed`; after that the button disappears
   and the conversion finishes. A cancel leaves the library and the disk exactly
   as they were (tested: no leftover file). If the flag is raised after the last
   check the conversion simply completes and the toast says so.
5. One toast at the end: "Library encrypted.", "Encryption turned off.",
   "Passphrase changed.", "Cancelled. Nothing was changed.", "Wrong passphrase.
   Nothing was changed.", "The library is in use by another program. Nothing
   was changed." or "The operation failed. Your library is intact." No name, no
   path, no engine error text.
6. If the library cannot be opened again the root shows the lock screen when the
   disk is encrypted (the passphrase may still work) and the error screen
   otherwise.

The view (selection, scroll, open item) is not restored after a conversion:
the app is rebuilt, as after an unlock. The `--import` and `--cmd` options are
not run again.

Limits, honestly:

- **Not seen on screen.** The dialogs, the progress screen and the Privacy page
  were compiled and exercised through state-machine and app-level tests
  without a window; no human looked at them or at a real conversion with a
  large library and a playing video yet (step 8).
- Closing the window during a conversion is held and a dialog asks first
  ("A conversion is in progress. Closing interrupts the operation. This is
  safe, but you will have to start it again."; *Close anyway* or *Keep
  waiting*). Confirming ends the process; the engine is crash-safe (`recover`
  finishes or undoes it at the next start) and the user restarts the
  operation. A forced kill (task manager, power cut) takes the same recovery
  path without the dialog. The logic (`Gate::on_close_request`) is
  unit-tested without a window; the dialog itself was not seen on screen.
- Conversion of a read-only library (made by a newer version) is refused.
- Another JizzCraft process holding the library makes the conversion `Busy`;
  nothing changes.
- The passphrase is typed in egui text fields: toolkit caches and RAM are
  outside the guarantees (see the threat model). The conversion buffers are
  `Zeroizing` and moved, never cloned, to the worker.
- Changing the passphrase rewrites the header only and does **not** rotate the
  data key: a copy of the old header still opens with the old passphrase.
  Turn the vault off and on again to rotate (see "Changing the passphrase does
  not rotate the key").
- A passphrase is at most `vault::MAX_PASSPHRASE_CHARS` = 1024 characters, in
  the dialogs and on the command line alike.

## Step 6: command line

```sh
jizzcraft-cli vault status                       # never asks, works locked
jizzcraft-cli vault enable                       # warning, passphrase twice
jizzcraft-cli vault enable --passphrase-stdin --i-understand-no-recovery < pass.txt
jizzcraft-cli vault change-passphrase --passphrase-fd 3 --new-passphrase-fd 4 3<old 4<new
jizzcraft-cli vault disable
jizzcraft-cli vault verify
jizzcraft-cli --passphrase-stdin stats < pass.txt  # any command on an encrypted library
```

- **Secret input.** A terminal (no echo, asked twice for a new passphrase),
  `--passphrase-fd N` or `--passphrase-stdin` (read to the end, exactly one
  trailing `\n` or `\r\n` removed, nothing else trimmed, 1024 characters at
  most, the limit of the app), and `--new-passphrase-fd N` for `enable` and
  `change-passphrase` (`enable` also accepts the primary source as the new
  passphrase). There is no argument and no environment variable for a secret.
  A new passphrase has at least 12 characters. Descriptors 0, 1 and 2 are
  refused for both flags (use `--passphrase-stdin`).
  `--passphrase-stdin` is also refused when stdin is a terminal for `enable`
  and `change-passphrase` ("use the interactive prompt instead"): it would be
  one silent entry with no confirmation. The prompt asks twice and asks again
  (three attempts) when the entries differ or are too short.
- **`enable`** prints the permanent-loss warning. Without a terminal it needs
  `--i-understand-no-recovery`; on a terminal you type `YES`.
- **Exit codes.** `3` wrong passphrase; `4` the library is encrypted and no
  passphrase can be asked (not a terminal, no descriptor); `5` header missing,
  damaged, unsupported or orphaned (`HeaderOrphan`); other failures are `1`.
- **Every command** opens the library through `LibraryOpener` after
  `vault_convert::recover`. In vault mode `thumbs` builds the cache through
  `ThumbService::with_keys`, and `cache.prune` (`run cache.prune`) checks
  `vault-thumbs` against keyed names with `prune_store` and `live_keys_for`.
- **Registry.** Only `vault.status` and `vault.lock` exist and neither takes a
  secret. Command arguments (`run`, `--cmd`) containing a field whose name has
  `passphrase` or `password`, or is `secret` or `dek`, are refused with a fixed
  message. In the one-shot CLI `vault.lock` answers `locked: false`: the
  process ends after the command.
- **`doctor --report`** prints `vault: on|off|locked`. An encrypted library
  without `--passphrase-fd`/`--passphrase-stdin` is `locked` and the report
  carries no schema version or counts.

### Canary test

`apps/jizzcraft-cli/tests/vault.rs` builds a library with a synthetic image
`zz-canary-7f3a.png`, a tag, a category, a marker note and a playlist with
unique names, scans it, builds thumbnails, runs `vault enable` with the
passphrase `CANARY-pass-1234`, keeps working (late tag, rescan, backup,
thumbnails, prune), then reads every file under the home directory (data,
cache, state, `-wal`, `-journal`, backups) and every captured stdout and
stderr for the names, the passphrase and the database key in hex and raw. A
positive control proves the scanner finds the name in the clear library.
`config/config.json` is scanned too: every key must be on an allow list (the
fields of the app's `Config`), and none of the synthetic names may appear in
it. Note that `panic_video` can hold a path the owner chose (a clear text
setting, out of the vault's scope, see the threat model): it is never a
library item, but it can name a file. The only exception is the media file
(outside the home). The test also reads the header and checks the production
cost parameters, 65536 KiB / 3 / 1. Output of commands that print data on purpose (`search`, `tag create`)
is only checked for the passphrase and the key.

The test also searches for the data key (DEK) and the two thumbnail sub-keys,
as raw bytes, hex and upper-case hex, through `#[doc(hidden)]`
`bytes_for_tests()` accessors on `VaultKey`, `DbKey` and `ThumbKeys` in core
(never called by product code). The test runs on every platform; only the
`--passphrase-fd` case is Unix-only.

Limits: it does not look at UTF-16 or compressed encodings of the names.
`jizzcraft-cli thumbs` builds image thumbnails only (no GStreamer in the CLI).

### `vault verify`

Unlocks, then runs `PRAGMA cipher_integrity_check` (the HMAC of every page,
exposed as `Db::cipher_integrity_check`), `quick_check` and a row count. Exit
`0` and `vault: ok (N items)` when all hold; `3` for a wrong passphrase. It is
read-only and can take a while on a big library.

## Step 8: performance, packaging, finishing

- **Performance.** `cargo xtask perf` measures every metric on a clear and an
  encrypted 10k library, the timed conversion in both directions and an
  unlock, with budgets in `perf/budgets.json`. Warm queries cost the same;
  cold first queries cost 30 to 70 % more (sub-millisecond to 13 ms), a cold
  scan +4 %, a 10k conversion about 0.14 s. Figures and method:
  `docs/perf.md`.
- **Packaging smoke test.** `packaging/smoke-install.sh` (the CI job
  `install-e2e` and the release workflow's `smoke-install`) installs the
  tarball, then on the installed `jizzcraft-cli`: scans a synthetic library,
  `vault enable --passphrase-stdin --i-understand-no-recovery`,
  `doctor --report` says `vault: on` and names no item, the same report
  without a passphrase says `vault: locked`, `vault verify` passes and a wrong
  passphrase exits with code 3 exactly, no name is left in clear under the
  home (a `grep` error is told apart from "nothing found"), the header says
  65536 / 3 / 1, no sensitive file or directory is open to group or others,
  `vault disable` gives `vault: off`. The passphrase is random per run and goes through
  `printf` into stdin: it is in no argument, no environment variable and no
  log, and the script never prints the output of a command that handles it.
- **Windows CI.** The `windows` job runs `cargo test --workspace`, which
  includes `vault_db`, `vault_convert` (with its `kill -9` test, `child.kill()`
  being portable) and the CLI `vault` tests. Only `--passphrase-fd` is
  `cfg(unix)`. The Linux-side cross-check
  `RUSTFLAGS='-D warnings' sh packaging/windows/cross-check.sh --workspace --all-targets`
  (zig as C toolchain) compiles all of it for `x86_64-pc-windows-gnu`; it
  proves compilation, not behaviour.

## Troubleshooting

**I forgot my passphrase.** The data is gone for good. There is no recovery,
no hint, no master key and no key file, by design. Your media files are
untouched: create a new library and import the folders again. Tags, ratings,
history, markers and playlists that lived only in the database are lost. If
you made a JSON export earlier, it is in clear and can be imported into the
new library.

**The header file `library.vault` is lost or damaged.** The header is the only
envelope of the data key; without it the database cannot be opened even with
the right passphrase (`jizzcraft-cli vault status` says `header missing` or
`header damaged`, exit code 5; the app shows an error screen and never creates
a new library next to it). Restore `library.vault` from a backup copy that was
taken together with the database. A copy of an *older* header still works with
the passphrase it had when it was made, so a copy taken before a passphrase
change opens with the old passphrase. Without any copy, the situation is the
same as a forgotten passphrase.

**`HeaderOrphan`.** A valid `library.vault` exists but the database next to it
is absent or in clear. Nothing is deleted: the application refuses to start
over it. Usual causes: the database file was restored or deleted by hand, or
an old clear copy was put back. Put the encrypted `library.db` back next to the
header (check the `pre-vN` backups in the same directory if it is gone), or, if
you really want to start over, move the header away yourself. If the clear
database is the one you want and the header is stale, remove `library.vault`
manually; the program will not do it without the `.vault.disabling` marker of
an interrupted disable.

**A correct backup is the database and its header together.** Copy
`library.db` **and** `library.vault` (same directory, same moment). The
in-app and CLI `backup` write `copy.db` and `copy.vault` side by side. A
database copy without its header is unreadable. Do not copy `-wal` or
`-journal` files by hand while the program runs; use `backup` instead, which
produces one consistent encrypted file.

**Restoring from `pre-vN` or `pre-remove`.** Before a schema migration the
program writes `library.pre-vN.db` next to the database, and before removing a
folder (or after a failed start-up integrity check) it writes
`library.pre-remove-<time>.db` (or `pre-check`). In vault mode they are encrypted with
the same key and need no header of their own. To restore: close the program,
keep the current `library.db` aside, copy the backup over `library.db` (keep
`library.vault` as it is; the backup keeps the file name you gave it, so
rename the copy to `library.db`), start again and enter the same passphrase. A clear
backup that predates the vault is deleted when the vault is turned on, so an
encrypted library has no clear backup to go back to.

**Windows and `--passphrase-stdin`.** `--passphrase-fd` is not available
on Windows; use `--passphrase-stdin`. In PowerShell:
`Get-Content pass.txt -Raw | jizzcraft-cli --passphrase-stdin vault verify`
(one trailing line ending is removed, nothing else is trimmed, so a file with
extra blank lines changes the passphrase). Piping with `echo` adds a line
ending that is removed again; a console code page that is not UTF-8 may change
non-ASCII characters, so prefer a UTF-8 file. Without stdin redirection the
CLI asks on the terminal with no echo.

**Wrong passphrase, but I am sure it is right.** Check the keyboard layout
and caps lock; the passphrase is NFC-normalised, never trimmed. The app waits
1, 2, 4 ... 30 s after three failures; the command line returns exit `3`
immediately. A key that the header accepts but SQLCipher refuses means the
database itself is damaged (run `vault verify` on a backup).

**A conversion was interrupted.** Start the program or any CLI command again:
`recover` finishes or undoes the operation and leaves either an all-clear or
an all-encrypted library. Then run the conversion again.

**Relation to full-disk encryption.** The vault protects the library
database and its thumbnail cache. It does not protect your media files, the
operating system's own traces (recent files, desktop search, thumbnails made by
other programs), swap, hibernation images, or deleted clear data on SSD and
copy-on-write storage. Full-disk encryption (LUKS, BitLocker, FileVault) covers
those and is complementary: use both for a laptop that can be lost. The vault
adds protection when the disk is mounted but the application is locked or
closed (a shared or unlocked machine, a copied backup, a cloud-synced folder).
