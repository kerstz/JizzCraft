# AGENTS.md

Read this first. JizzCraft is a private, offline adult-media viewer written in
Rust (egui/eframe + wgpu, GStreamer, SQLite).

## Layout (engine first)

```
crates/core    model, SQLite, scan, command registry. No UI, no GStreamer.
crates/media   thumbnails, codec probing, GStreamer player, network guard.
crates/fetch   Torznab indexers + BitTorrent (librqbit). Network deps only
               behind the off-by-default `network` feature.
crates/ui      egui app: theme, grid, viewer, player, inspector.
apps/jizzcraft       desktop app          apps/jizzcraft-cli   CLI
xtask          fixtures, icons, screenshots
```

`core` must never depend on `media`, `ui` or `fetch`; `media` never on `fetch`;
`ui` and the apps only optionally, through the `network` feature.

## Commands

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask fixtures                      # SFW images + videos into ./fixtures
cargo run --release -p jizzcraft          # desktop app
cargo run -p jizzcraft-cli -- stats --json
./target/release/jizzcraft --bench fixtures/videos/bench-1080p30.mp4 --seconds 30
./target/release/jizzcraft --import fixtures --screenshot out.png --assume-adult
```

## Command registry

One registry of named commands (`library.import`, `library.rescan`,
`media.query`, `media.favorite.toggle`, `view.filter`, `view.sort`, `view.select.move`, `player.*`,
`app.boss_key`, `app.left_hand.toggle`, …). Menus, shortcuts, the CLI and `--cmd NAME=JSON` all call it.
Add behaviour as a command first, UI second.

## Rules for user data

- **Schema is append-only.** New columns and tables go in a new entry of
  `MIGRATIONS`; Rust backfills are keyed by target version in `post_migrate`.
  Never edit an old entry (a test freezes the v1 SQL).
- **Filter values reach SQL only as bound parameters.** Sort keys and filter
  keys are whitelisted (`SortKey`, `ALLOWED_KEYS`).
- **Anything slow is a job** with its own connection, a cancel flag and one
  toast with counts. Never names in toasts or logs.
- **Key bindings live in `crates/ui/src/keys.rs` only.** The docs window is
  generated from that table. Digits and left-hand letters match physical keys.
- **User data is database-only.** Ratings, hidden flags, history and markers
  must not write sidecar files or touch media. `engine_never_touches_media`
  hashes a tree before and after every command; keep it passing.

## Rules

- **Never** modify, move or delete user media. Thumbnails go to the XDG cache dir.
- **The default build has no network of any kind**, proven by `offline_deps`
  (`cargo tree`) and `tests/network.rs` (zero connections). Network code lives
  only in `crates/fetch`, behind the off-by-default `network` feature
  (`cargo build -p jizzcraft --features network`). No telemetry in any build.
  The player never takes a URL in any build. It gets a `file://` URI from
  `jizzcraft_media::file_uri`, or, for `Player::open_stream` only, bytes read
  in-process from a local reader and pushed into an `appsrc` (no URL, no
  socket). Only that pipeline may contain an `appsrc` (`guard::watch_with`);
  streamed bytes are sniffed with `guard::looks_like_manifest` first, and
  `tests/network.rs::stream_player_never_connects` must stay at zero connections.
  GStreamer only gets `file://` URIs from `jizzcraft_media::file_uri`; call
  `jizzcraft_media::init()` before building any pipeline (it removes
  network-capable elements from the process registry) and pass every new
  pipeline to `guard::watch`. `tests/network.rs` must stay at zero connections.
- **Network build rules.** Indexer keys live only in `fetch.json` (mode 0600),
  never in the database, backups, exports, logs, errors or toasts. No indexer,
  tracker or site is hard-coded: the user supplies them. No `tracing`
  subscriber (librqbit logs torrent names). `fetch.remove` never deletes files,
  torrents never overwrite files, unsafe torrent paths are refused. Toasts
  carry counts only. Nothing connects before the user acts. Schema changes go
  in `MIGRATIONS` regardless of features.
- **No shell** built from filenames. Pass paths as argv or through the APIs.
- **No full paths or filenames in logs** unless `JIZZCRAFT_DEBUG=1`.
- Tests, fixtures and screenshots are **100 % SFW and synthetic**. Never commit
  or download real adult content.
- The UI thread never decodes, never waits on GStreamer, never sets a pipeline
  to Null. All pipeline control lives on the player's control thread.
- Do not use `ctx.request_repaint()` per video frame: egui doubles it. Use a
  non-zero delay (see `open_player`).
- Do not reuse code, assets, logos or wordmarks from ArtCraft / PhotoCraft.

## Visual language

Craft look (tokens in `crates/ui/src/theme.rs`, widgets in `widgets.rs`): flat
surfaces, 0 px radius, hairlines in translucent warm white, Archivo for display,
Inter for body, JetBrains Mono for tracked uppercase labels and numbers, inverted
white primary button, own app colour `APP` (peach). Tiles and cells share 1 px
hairlines. Do not add background grids or rulers.
