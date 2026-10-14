# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) once 1.0 is out. Before that,
anything may change between minor versions.

## [Unreleased]

### Security
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

### Editions and packaging
- Two named editions of one code base. Offline is `jizzcraft` (no network
  code). Online is `jizzcraft-online` (cargo feature `online`, an alias of
  `network`); it provides, conflicts with and replaces `jizzcraft`. Same app
  ID, same binaries and data directories.
- Online release tarballs are now `jizzcraft-online-<version>-<arch>-linux.tar.xz`
  (was `jizzcraft-network-...`). The 0.1.0 `install.sh --with-network` looks
  for the old name and cannot fetch 0.2 or later tarballs: use the current
  `install.sh`. `--network` / `--with-network` stay accepted as deprecated
  aliases of `--edition online`.
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
