# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) once 1.0 is out. Before that,
anything may change between minor versions.

## [Unreleased]

### Documentation
- README and the getting-started guide now document the panic switch
  (`Ctrl+Shift+M`) and the vault lock key (`Ctrl+Shift+L`); a test keeps every
  `Ctrl+Shift+` binding documented.

### Fixed
- Player: the PLAYING / PAUSED state label no longer overlaps the transport
  buttons in narrow windows; it moves into the free space or hides. An error
  shows ERROR in place of the clock when there is no room for the label.
- Online edition: restarting a torrent that went to the error state over its
  own partial files now first checks that every existing file is still the
  one the session wrote (same inode, regular file, not longer than expected).
  A swapped file or a link makes the restart refuse instead of writing through.
- Online edition: an unreadable or corrupt `fetch.json` now fails closed. The
  "Block the network without a proxy" switch comes up on instead of silently
  reverting to off; the banner says how to go on.
- Online edition: an empty proxy in `fetch.json` (or blanks typed in the
  setting) now means "no proxy" everywhere; torrents no longer fail to start
  because of it. The proxy stays optional.

## [0.2.0] - 2026-10-15

Second alpha, Linux first. Highlights:

- **Search**: full-text search (SQLite FTS5) over names, tags, categories and
  marker notes, ignoring case and accents (schema v4).
- **Player**: local `.srt` / `.vtt` subtitles; separate audio and video tracks
  for DASH and HLS, with an "Audio" menu and an audio-language setting; live
  HLS streams.
- **Library**: grouped selection with bulk rate, favorite, hide and "Add to";
  removing an imported folder from the library, as a cancellable job with a
  backup first.
- **Encrypted vault (alpha)**: SQLCipher-encrypted library with passphrase,
  lock screen, auto-lock, encrypted thumbnails and a `vault` CLI. Off until
  you turn it on; no recovery.
- **Downloads**: a full disk pauses torrents with a clear status instead of
  hanging after an engine panic, and resumes once space is back.
- **Quality**: performance budgets (`cargo xtask perf`) and 18 end-to-end
  scenarios (`cargo xtask e2e`), a required CI check.

### Upgrading from 0.1.0
- A 0.1.0 library opens as is: the first start migrates it to schema v4 (the
  full-text index is built) after writing a `library.pre-v3.db` backup next to
  it. Ratings, favorites, hidden flags, tags and playlists are unchanged.
  Checked with a library created by the 0.1.0 binary. 0.1.0 cannot open a
  migrated library again ("library schema v4 is newer"); restore the backup to
  go back.

### Known limits
- Windows has never been validated on real hardware; the portable zip and
  installer are built by CI as experimental and may be absent from a release.
- macOS compiles but ships no app bundle and has never run on real hardware.
- The encrypted vault has never been seen on screen by the maintainers: the
  lock screen and Settings flow are covered by tests only. Treat it as alpha
  and keep a copy of anything you cannot lose.
- `config.json` (and `fetch.json`, the online state directory) are not
  encrypted: they stay in clear even with the vault on, so settings such as
  the interface language are readable. See `docs/vault.md`.
- Live DASH, DRM streams and TS or WebM separate audio are still refused.

### Fixed
- Torrents on a full disk (issue #21): librqbit panics on the partial write of
  a real ENOSPC and the torrent used to stay "downloading" forever. The fetch
  service now checks free space while torrents run (remaining bytes plus
  64 MiB, every second) and before a resume, pauses with the translated
  "Disk full. Downloads paused." status, detects a torrent left stuck by an
  engine panic, keeps engine panics out of the player thread and every
  service reply, and resumes once space is back (a torrent that librqbit
  left in its error state is added again over its own partial files). A disk-full torrent shows as
  paused, not as an error. s15 now covers the torrent case; the known limit
  is removed from `docs/e2e.md`, which lists what is and is not guaranteed.

### Tests
- e2e scenario s15: a full disk ends in a fixed sentence, leaves no partial file and recovers once space is freed; the e2e CI job is a required check (issue #18).

### Security
- Encrypted vault: a conversion that ends encrypted but with the key file not
  finalised (`FinishFailed`) now has its own sentence and returns to the lock
  screen; the app no longer says "Your library is intact" then. Cache and
  config directories are created 0700 and `config.json` 0600 on Unix.
- Encrypted vault, audit round 2, desktop app. The thumbnail and preview
  workers were detached: a decode in flight could write a clear thumbnail
  after the encryption's cache clean-up, and their keys outlived the "Locked"
  screen. Workers now check a stop flag before every cache write, the app
  waits for them (`Workers`, `quiesced()`) before it converts or shows
  "Locked", and the thumbnail keys are freed first. The inactivity lock no
  longer depends on drawing: `Gate::tick` runs before the app is drawn, in
  `App::logic` while the window is hidden, and from a timer thread; a boss-key
  or menu lock covers the frame it was asked in. egui's text edit history is
  purged on every locked transition and dialog close. The passphrase limit is
  1024 characters with a clear message instead of a silent cut; "Change
  passphrase" says it does not regenerate the data key. The window, its title
  and its app ID are neutral from the first frame when the library is
  encrypted. UI tests no longer touch the real `~/.cache/jizzcraft`.
- Encrypted vault, audit round 2 (issue #13). **Fixed a total-loss race:** a
  `recover` running in parallel with an `enable` could delete
  `library.vault.new` just after the commit rename, leaving an encrypted
  database without header. Conversions, passphrase changes, recovery and
  unlock now share an advisory lock (`library.lock`): a holder makes `recover`
  return `Busy` and delete nothing. `enable` rewrites the header from memory if
  its rename fails after the commit, and a failure past the commit point is a
  new `VaultError::FinishFailed` instead of "library intact". Sensitive files
  are 0600 and the data, cache and home directories 0700 on Unix (database,
  `-wal`, backups, header, exports, crash notes). The command line refuses
  `--passphrase-stdin` on a terminal for `vault enable` and
  `change-passphrase` and asks again on a typo; the passphrase limit is 1024
  characters everywhere (`vault::MAX_PASSPHRASE_CHARS`); descriptors 0, 1 and
  2 are refused for `--passphrase-fd`. Argon2 memory is zeroed
  (`argon2/zeroize`). A library opened with `--db` no longer makes a
  conversion empty the global cache. The canary now scans `config.json` with
  a key allow list; the smoke test requires exit code 3 for a wrong
  passphrase, the 65536/3/1 header and the permissions. `docs/vault.md`
  documents that changing the passphrase does not rotate the data key
  (remedy: disable, then enable).
- Encrypted vault, step 8 of issue #13: finishing. `cargo xtask perf` now
  measures the clear and the encrypted 10k library side by side (`vault_`
  budgets), the timed conversion in both directions and one unlock; measured
  overhead: warm queries 0 %, cold first queries +30 to +70 %, cold scan +4 %,
  a 10k conversion about 0.14 s (`docs/perf.md`). The package smoke test
  (`install-e2e`, release `smoke-install`) enables the vault on the installed
  CLI, checks `doctor --report` (`vault: on`, no name), `vault verify`, the
  locked state and `vault disable`, without ever printing a secret. Closing
  the window during a conversion is held behind a translated confirmation
  (the engine is crash-safe; the operation has to be started again). Exporting
  a playlist as `.m3u8` warns first while the vault is on, like the JSON
  export, in the six languages. `vault verify` also runs
  `cipher_integrity_check` (`Db::cipher_integrity_check`). The canary test
  now also looks for the data key and the thumbnail sub-keys, and the CLI
  vault tests run on Windows except the `--passphrase-fd` ones.
  `docs/vault.md` gains a Troubleshooting section and the progress table is
  complete.
- Encrypted vault, step 7b of issue #13: Settings, conversion and auto-lock in
  the desktop app. The vault is now a feature, delivered as an **alpha** and
  off until you turn it on. New Settings page "Privacy": *Encrypt library*
  (passphrase typed twice, 12 characters minimum, a local strength estimate,
  a mandatory "no recovery" box, notes that data written in clear may stay
  readable on disk and that clear backups and thumbnails are deleted),
  *Change passphrase*, *Turn off encryption* (both ask for the passphrase
  again even when the vault is open). The conversion runs as a job: the root
  closes the library (jobs cancelled and waited for), a worker does the
  Argon2id and the conversion, the library opens again with the key; Cancel
  works until the commit point (`ConvertOptions::cancel` / `committed`,
  `VaultError::Cancelled`); a request is refused with a fixed sentence while
  a job runs; one final toast without names or paths. Automatic lock after
  inactivity (`autolock_minutes`, default 15, 0 = off, a playing video counts
  as activity) through the same path as `app.vault.lock`, and the option
  "The boss key also locks the library" (`boss_locks`, off by default); both
  in the clear config, set by the new `app.vault.settings` command. The
  clear JSON metadata export asks for confirmation when the vault is on.
  Still no recovery, hint, OS keychain or "remember me". Six languages.
  Step 8 (performance in vault mode, packaging smoke tests) remains.
- Encrypted vault, step 7a of issue #13: lock screen and keyed opening in the
  desktop app. A new `Root` wraps the app with `Locked`, `Unlocking`, `Open`
  and `Locking` states (and an error screen for a missing, damaged or
  orphaned header, which never creates a new library). An encrypted library
  shows a neutral, translated lock screen (title `Viewer`, no counts or names);
  nothing else runs before the key opens it. Argon2id runs on a worker thread,
  a wrong passphrase gives a fixed sentence and wipes the buffer, with a soft
  delay after three failures. `app.vault.lock` (menu "Lock library",
  `Ctrl+Shift+L`) hides the app, cancels its jobs, stops the player through
  its control thread, waits for the jobs and drops the app so the key is
  zeroed. Every open in the UI (scans, jobs, folder removal, start-up check,
  thumbnails, scrub previews) goes through `LibraryOpener` and the keys. Core:
  `scan::import_folder_with`. Settings to enable or disable the vault,
  conversion as a job and auto-lock are step 7b; see `docs/vault.md`.
- Encrypted vault, step 6 of issue #13: the command line. `jizzcraft-cli vault
  status|enable|disable|change-passphrase|verify`. The passphrase is read from
  a terminal without echo (`rpassword`, CLI only), or from `--passphrase-fd N`
  / `--passphrase-stdin` (one trailing newline removed), with
  `--new-passphrase-fd` for enable and change; never from argv or an
  environment variable. `enable` shows the no-recovery warning, asks the
  passphrase twice on a terminal, wants at least 12 characters and, without a
  terminal, `--i-understand-no-recovery`. Exit codes: 3 wrong passphrase, 4
  locked and no way to ask, 5 header missing, damaged or orphaned. Every CLI
  command opens the library through `LibraryOpener` with the key; the new
  `thumbs` command builds the cache through `ThumbService::with_keys`, and
  `cache.prune` in vault mode prunes `vault-thumbs` against keyed names. The
  registry gains `vault.status` and `vault.lock`; command arguments with
  passphrase-like fields are refused. `doctor --report` prints `vault:
  on|off|locked` and no statistics when locked. A canary test scans every byte
  under the home directory and all captured output for a synthetic file name,
  tags, category, note, playlist, the passphrase and the database key after
  `vault enable`.

- Encrypted vault, step 0 and 1 of issue #13. **Nothing is encrypted yet**:
  the vault is not delivered and `SECURITY.md` and the README say so.
  `docs/vault.md` records the threat model, the `library.vault` header format,
  what is not protected and that a forgotten passphrase means permanent loss.
  The database engine is now SQLCipher 4.14 (`rusqlite` feature
  `bundled-sqlcipher-vendored-openssl`) with no change of behaviour: same
  schema, FTS5 and migrations, perf budgets unchanged. New build needs: C
  compiler, `make`, `perl` (NASM on Windows); `OPENSSL_NO_VENDOR=1` links the
  system OpenSSL. `openssl-sys` is now in the tree, restricted by
  `offline_deps` to `libsqlite3-sys`; `openssl` and `native-tls` stay banned.
  The embedded SQLite is 3.51.3, older than before: migrations must not use
  newer SQL. OpenSSL (Apache-2.0) and SQLCipher (BSD-3) are credited in
  `NOTICE` and `ATTRIBUTION.md`.

- Encrypted vault, steps 2 and 3 of issue #13 (engine only, **still
  nothing is encrypted**: no UI, CLI or conversion calls it yet).
  `jizzcraft_core::vault` holds the pure crypto: the 111-byte `library.vault`
  header (the earlier "135 bytes" in the docs was an arithmetic slip),
  Argon2id with bounded parameters, an XChaCha20-Poly1305 key envelope,
  BLAKE3 sub-keys, redacted zeroing key types and the thumbnail cipher, with a
  frozen test vector. `Db::open_with` / `open_read_only_with` open a library
  with a key (`Error::Locked` without one, `VaultError::DbCorrupt` with a wrong
  one), `LibraryOpener` carries path and key, and `pre-vN` backups and
  `portability::backup` stay encrypted (the backup also copies the header).
  Migration, FTS and crash tests now run in clear and encrypted mode.

- Encrypted vault, step 5 of issue #13 (engine only, still not wired to the
  app or CLI). `jizzcraft_media::thumbs::ThumbStore` caches thumbnails and
  scrub previews as XChaCha20-Poly1305 files under `cache/vault-thumbs/`
  with keyed names; nothing is written to `thumbs/` or `previews/` in vault
  mode. Pruning and `cache.clear` cover the encrypted cache.

- Encrypted vault, step 4 of issue #13 (engine only, **still nothing is
  encrypted** by any shipped path: no UI or CLI calls it yet).
  `jizzcraft_core::vault_convert` converts a library in place:
  `enable`, `disable`, `change_passphrase` (header rewrap only, atomic) and
  `recover`. The copy is exported with `sqlcipher_export`, verified (key,
  cipher integrity, quick check, row counts, `user_version`, FTS integrity)
  and synced before a single rename commits it; clear `pre-vN` backups and
  clear thumbnail caches are then deleted. `recover` restores a consistent
  all-clear or all-encrypted state from disk alone. Tests inject a failure
  after every step and `kill -9` a converting process. New fixed-message
  `VaultError` variants: `Busy`, `AlreadyEncrypted`, `ConvertFailed`,
  `VerifyFailed`. `disable` writes an intent marker
  (`library.vault.disabling`) before its commit point; `recover` deletes the
  header, encrypted backups and `vault-thumbs` of a clear library only when
  that marker exists. A valid header next to a clear or missing library
  without it is `VaultError::HeaderOrphan`: nothing is deleted and `Db::open`
  refuses to create a new library there.

### Tooling
- Performance budgets (issue #17, engine part). `cargo xtask perf` builds a
  synthetic 10k-file library and times scan (cold and rescan), database open,
  the v1 migration chain, grid pages under four sort keys, counts and
  full-text search, then fails if a budget in `perf/budgets.json` is exceeded.
  Budgets are loose (about 10x measured). A non-blocking `perf` CI job runs
  it. See `docs/perf.md`; RSS, time to first frame and UI frame cost are not
  covered yet.

### Streaming
- Audio track choice. New setting "Audio language" in Settings, General
  (saved as `audio_lang` in `config.json`): the interface language by default,
  a language code, or "Default track". It is passed to `StreamOpts::audio_lang`
  on every stream that opens. When a stream keeps several audio tracks, the
  player shows an "Audio" chip with one entry per track (language code, or
  "Track N"; never a name or an address) and `player.audio {index}` switches:
  the stream is reopened at the same position and keeps its paused state.
  Engine: `TimedSource::audio_tracks` / `current_audio` (default: none),
  `StreamOpts::audio_index`, `segmented::AudioPref`. Limits: the switch
  reopens the source (a short rebuffer, not a hot swap); live streams with
  separate audio are still refused; tracks of the same language are told
  apart by number only. Tests: language/persistence unit tests, track list
  and index selection on loopback DASH and HLS servers.
- Separate audio and video tracks (issue #8). A DASH manifest with its own
  audio adaptation set, or an HLS master whose variants use an
  `#EXT-X-MEDIA` audio group, now plays instead of being refused. Both
  tracks go through the same segmented reader (so the same address checks,
  proxy kill switch, retries, limits and cancellation), the two init
  segments are merged and the fragments interleaved by presentation time:
  the player still receives a single byte stream through `open_timed` and
  never a URL, and the guard still sees one `appsrc`. The audio language
  follows `StreamOpts::audio_lang`, then the default rendition, then the
  first. Quality choices and automatic quality only touch the video; seek
  moves both tracks. Limits: both tracks must be fragmented MP4 (TS or WebM
  audio, live HLS with an audio group and DRM are refused; the refusal
  sentence "separate tracks" was reworded and re-translated). Tests: pure
  fMP4 merge/retag unit tests, loopback DASH and HLS servers (order,
  language, seek, cancellation, refusals) and e2e scenario `s10b`.
- Live HLS streams (issue #7). A media playlist without `#EXT-X-ENDLIST` now
  plays instead of being refused: it starts three segments from the live
  edge, is reloaded every target duration, plays each segment once by media
  sequence, resumes after a hole, follows a numbering restart and
  `#EXT-X-DISCONTINUITY`, skips a missing segment, and ends cleanly on
  `ENDLIST` or after 12 hours (or with "The live stream ended." when the
  playlist is lost). The player shows a translated `LIVE` chip and no
  timeline; seeking stays off. Key systems (SAMPLE-AES, Widevine, ...) are
  still refused, including when they appear in a later reload. Live DASH is
  still refused with its own sentence ("Live DASH streams are not supported.",
  translated). Same address checks, proxy kill switch and cancellation as
  on-demand streams; bytes still reach the player through `open_timed` only.
  Tests: sliding-window loopback server (gap, discontinuity, restart,
  `ENDLIST`, stale, cancellation, DRM) and e2e scenario `s06b`.

### Player
- Local subtitles (issue #9). A `.srt` or `.vtt` next to the video with the
  same base name is picked up, with an optional language tag (`clip.fr.srt`,
  `clip.pt-BR.vtt`). Tolerant parsing (BOM, CRLF, UTF-16 with a BOM, UTF-8 with
  a Latin-1 fallback; HTML, WebVTT and `{\an8}` tags are dropped). Drawn by
  the UI as an overlay, not by GStreamer, at a size that follows the interface
  zoom. A `Subs` chip in the player bar opens a menu (track, off, timing);
  `V` cycles tracks then off, `Alt+←` / `Alt+→` shift timing by 0.1 s. New
  command `player.subs`. The track choice is remembered in the settings
  (`subs`); the timing is per session. Sidecars are only read: nothing is
  written, and no file name appears in the menu, logs or toasts. Streams have
  no sidecars.

### Library
- Multi-select polish and bulk actions (issue #11). A bulk bar appears under
  the grid while tiles are marked: rate 1 to 5 or clear the rating, favorite /
  unfavorite, hide / unhide, "Add to…" (playlist, tag or category), select all,
  clear. Ctrl+drag adds a rubber band to the marks and Shift+drag replaces
  them (only tiles on screen are reached; the band does not auto-scroll). The
  tile context menu gained Rate, Favorite and Hide, acting on the marks when
  the tile is marked. Bulk changes run in one transaction, report one count in
  one toast and are undone as a whole (`A` / `org.undo`). New core command
  `media.bulk {ids, favorite?, rating?, hidden?}` and UI command `org.bulk`.
  No schema change, nothing is written outside the database.
- Full-text search over names and labels (issue #10). The search box now
  also matches tag names, category names and marker notes, ignoring case and
  accents, with every word matched as a prefix (`cafe sunr` finds
  `Café_Sunrise.png`); a plain substring of the file name still matches as
  before. Backed by an SQLite FTS5 index (schema v4, backfilled at upgrade
  after the usual pre-migration backup) that triggers keep current on scan,
  tagging, renames, marker edits and folder removal. Private tags and
  categories are never indexed. New command `media.search {query, limit?,
  offset?}` (ranked, names first) and CLI `jizzcraft-cli search WORDS...`.
- Remove a folder from the library in the UI (issue #12): right-click an
  imported root in the sidebar, then confirm. Only database rows go (the
  folder, its media and their ratings, tags, markers and playlist entries);
  files on disk are never touched, and the dialog and the toast say so.
  New command `library.remove {path}`. Refused while a scan is running.
- Removing an imported folder is now a background job: a
  `<stem>.pre-remove-<ts>.db` backup is written first (3 kept, same key as the
  library), rows are dropped in bounded chunks, it can be cancelled
  (`library.remove.cancel`, Tools menu), and one toast reports counts only.
  The confirmation dialog never shows the folder name once discreet mode is on.
- The start-up integrity check now really writes a `<stem>.pre-check-<ts>.db`
  backup before saying "A backup was kept"; if it cannot, the toast says so.
- Interface: the age-gate paragraph, three hints and three download-panel
  sentences are translated in fr/es/de/it/pt, and a test
  (`crates/ui/tests/i18n_coverage.rs`) fails when a `tr("…")` literal is
  missing from a catalog.

## [0.1.0] - 2026-10-14

First public alpha.

Application ID is io.github.kerstz.JizzCraft.

### Library and player
- Local library of images and videos, indexed in place and never modified:
  search, ratings, hide, saved views, folder tree, inbox triage, markers,
  resume points and watch history.
- Moved or renamed files keep their data (content fingerprints).
- GStreamer player rendered through wgpu: seeking and scrubbing, speed, A-B
  loop, frame step, autoplay-next.
- Privacy: 18+ gate, boss key, discreet and share-safe modes, disguised launch,
  panic switch to a local video of your choice.
- One-handed key preset, six interface languages, playful messages (off in
  discreet modes).
- `jizzcraft-cli` and `--cmd`: the same named commands as the menus.

### Editions and packaging
- Two named editions of one code base. Offline is `jizzcraft` (no network
  code). Online is `jizzcraft-online` (cargo feature `online`, an alias of
  `network`); it provides, conflicts with and replaces `jizzcraft`. Same app
  ID, same binaries and data directories.
- Online release tarballs are now `jizzcraft-online-<version>-<arch>-linux.tar.xz`
  (was `jizzcraft-network-...`). `--network` / `--with-network` stay accepted
  as deprecated aliases of `--edition online`.
- `install.sh`: `--edition offline|online`, `--with-browser` / `--no-browser`
  (online only), `--purge-browser-data`. Re-running keeps the edition recorded
  in the install manifest, so an online user is never downgraded; otherwise it
  asks in a terminal and falls back to offline. Switching online to offline
  removes the online-only files.
- Optional built-in browser: separate `jizzcraft-browser-<version>-<arch>-linux.tar.xz`
  (only when the binary is built), `jizzcraft-browser` deb, Arch optdepends and
  a Fedora `-browser` subpackage placeholder. System packages come from
  `assets/browser-packages.tsv` and are installed only after asking.
- `cargo xtask dist --edition offline|online [--browser]` (`--network` kept as
  an alias).

### Linux install
- One-line `install.sh` (checksum, per-user, uninstall that touches only its
  own files), `jizzcraft-cli doctor` with per-distribution fix commands,
  `.deb`, man pages and shell completions, a single staging tree
  (`cargo xtask dist`).

### Network build (opt-in, `--features network`)
- Torznab indexers, BitTorrent (play while it downloads), plain HTTP.
- Streaming from sources you provide: seekable HTTP through `Range`, HLS and
  DASH on-demand (muxed, AES-128, DRM refused), declarative JSON sources,
  external resolvers, one input box, quality chip.
- `require_proxy` kill switch; keys only in `fetch.json` (mode 0600).

### Known gaps
- Live streams, separate audio and video tracks, subtitles and saving a stream
  while watching are not supported.
- Windows and macOS compile but have never run on real hardware; no macOS
  bundle is published.
- Arch, RPM, Flatpak and AppImage recipes are not built by CI yet.
