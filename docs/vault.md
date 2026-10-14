# Encrypted vault (issue #13)

**Status: in progress, not delivered.** Nothing is encrypted today. The library
database and the thumbnail cache are stored in clear, exactly as before. This
page describes the design being built so that the promise is written down
before the code exists, and records what each step has actually shipped.

| Step | Content | State |
| --- | --- | --- |
| 0 | This document, `SECURITY.md`, README wording | done |
| 1 | SQLite replaced by SQLCipher (same behaviour, nothing encrypted) | done |
| 2 | Key derivation and header file (pure crypto) | not started |
| 3 | Opening the database with a key | not started |
| 4 | Plain to encrypted conversion, crash recovery | not started |
| 5 | Encrypted thumbnails and previews | not started |
| 6 | CLI commands | not started |
| 7 | Settings UI, lock screen, auto-lock | not started |
| 8 | Perf in vault mode, packaging smoke tests | not started |

Until steps 2 to 7 are done there is no way to turn the vault on, and no part of
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

### Header file `library.vault` (version 1, 135 bytes, little-endian)

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
