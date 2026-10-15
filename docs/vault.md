# Encrypted vault (issue #13)

**Status: in progress, not delivered.** Nothing is encrypted today. The library
database and the thumbnail cache are stored in clear, exactly as before. This
page describes the design being built so that the promise is written down
before the code exists, and records what each step has actually shipped.

| Step | Content | State |
| --- | --- | --- |
| 0 | This document, `SECURITY.md`, README wording | done |
| 1 | SQLite replaced by SQLCipher (same behaviour, nothing encrypted) | done |
| 2 | Key derivation and header file (pure crypto, `core::vault`) | done |
| 3 | Opening the database with a key | done |
| 4 | Plain to encrypted conversion, crash recovery (engine only) | done |
| 5 | Encrypted thumbnails and previews | done |
| 6 | CLI commands | not started |
| 7 | Settings UI, lock screen, auto-lock | not started |
| 8 | Perf in vault mode, packaging smoke tests | not started |

Until steps 5 to 7 are done there is no way to turn the vault on, and no part of
your library is protected by it.

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
- **JSON exports** remain in clear (the UI will warn).

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
  stays openable with the old passphrase.
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
