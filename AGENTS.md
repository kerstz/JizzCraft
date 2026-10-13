# AGENTS.md

Read this first. JizzCraft is a private, offline adult-media viewer written in
Rust (egui/eframe + wgpu, GStreamer, SQLite). It is a parody project.

## Layout (engine first)

```
crates/core    model, SQLite, scan, command registry. No UI, no GStreamer.
crates/media   thumbnails, codec probing, GStreamer player.
crates/ui      egui app: theme, grid, viewer, player, inspector.
apps/jizzcraft       desktop app          apps/jizzcraft-cli   CLI
xtask          fixtures, icons, screenshots
```

`core` must never depend on `media` or `ui`.

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
`media.query`, `media.favorite.toggle`, `view.filter`, `player.*`,
`app.boss_key`, …). Menus, shortcuts, the CLI and `--cmd NAME=JSON` all call it.
Add behaviour as a command first, UI second.

## Rules

- **Never** modify, move or delete user media. Thumbnails go to the XDG cache dir.
- **No network** of any kind. No remote URLs in the player. No telemetry.
- **No shell** built from filenames. Pass paths as argv or through the APIs.
- **No full paths or filenames in logs** unless `JIZZCRAFT_DEBUG=1`.
- Tests, fixtures and screenshots are **100 % SFW and synthetic**. Never commit
  or download real adult content.
- The UI thread never decodes, never waits on GStreamer, never sets a pipeline
  to Null. All pipeline control lives on the player's control thread.
- Do not use `ctx.request_repaint()` per video frame: egui doubles it. Use a
  non-zero delay (see `open_player`).
- Do not reuse code, assets, logos or wordmarks from ArtCraft / PhotoCraft.
