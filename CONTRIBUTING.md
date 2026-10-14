# Contributing to JizzCraft

Thanks for helping. Read [`AGENTS.md`](AGENTS.md) first: it is the short rule
book for humans and coding agents alike.

## The rules that matter

- **The default build has no network code.** No HTTP clients, no remote URLs
  in the player, no telemetry, no update checks. Networking exists only in the
  opt-in `network` build (`--features network`), where every connection is
  started by an explicit user action and nothing is bundled. The default
  dependency tree is checked to contain no network crate. GStreamer pipelines
  only get `file://` URIs, start after `jizzcraft_media::init()` and are passed
  to `guard::watch`; `crates/media/tests/network.rs` must keep reporting zero
  connections.
- **No real adult content, anywhere in the repo.** Tests, fixtures and
  screenshots are 100 % SFW and synthetic (`cargo xtask fixtures` generates
  gradients, patterns and test-pattern videos). Do not attach real media,
  screenshots of your library or file names to issues or pull requests either.
- **No site-specific code.** No scrapers, site modules, indexer or source
  presets, or links to adult websites. The network build ships the mechanism
  only; users bring their own sources.
- **Command registry first.** New behaviour is a named command in the registry
  (`library.*`, `media.*`, `view.*`, `player.*`, `app.*`) before it gets a
  button or a shortcut. Menus, shortcuts, the CLI and `--cmd` all go through it.
- **Never modify, move or delete user media.** Thumbnails go to the XDG cache.
  User data (ratings, history, markers…) lives in the database only.
- **Schema is append-only.** New columns and tables are a new migration; old
  migrations are never edited.
- **No shell built from file names**, and no full paths in logs unless
  `JIZZCRAFT_DEBUG=1`.
- `crates/core` never depends on `media` or `ui`. The UI thread never touches
  the GStreamer pipeline. Key bindings live in `crates/ui/src/keys.rs` only.
- Do not copy code, assets, logos or wordmarks from ArtCraft / PhotoCraft or
  any other Crafting App.

## Content policy

No real adult content in the repository, in issues or in pull requests: fixtures
and screenshots are synthetic (`cargo xtask fixtures`). Pull requests that add
source, site or indexer presets, catalogs or scrapers will be closed. See
[`LEGAL.md`](LEGAL.md) and the [code of conduct](CODE_OF_CONDUCT.md).

## Before you open a pull request

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three commands on Ubuntu with GStreamer base and good
plugins. Media tests need `gst-launch-1.0`, `vp8enc`, `webmmux` and the
playback elements; when they are missing the tests print `skipped: …` and
return, so check the output if you run them on a minimal system.

Keep pull requests focused (a PR titled "misc fixes" gets reviewed with misc
enthusiasm), describe what you checked by hand versus what is covered by
tests, and update the README status block if you change what is verified.
Unless you explicitly state otherwise, any contribution you submit is
dual-licensed under MIT OR Apache-2.0, without additional terms.
