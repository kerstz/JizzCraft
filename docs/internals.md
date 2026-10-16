# Internals

For people who want to judge how JizzCraft is built before trusting it with
anything. Every claim here points at the file that enforces it; if a linked
test goes red, the claim is wrong. The rule book for contributors is
[AGENTS.md](../AGENTS.md); this page is the map. If you only want to install
and use the app, [getting-started.md](getting-started.md) is shorter.

- [Workspace and dependency rules](#workspace-and-dependency-rules)
- [Runtime model](#runtime-model)
- [Guarantees checked by tests](#guarantees-checked-by-tests)
- [Command registry and JSON CLI](#command-registry-and-json-cli)
- [`cargo xtask`](#cargo-xtask)
- [Threat model and known limits](#threat-model-and-known-limits)
- [Performance budgets](#performance-budgets)
- [Build, test and run CI locally](#build-test-and-run-ci-locally)
- [Contributing](#contributing)

## Workspace and dependency rules

| Crate | Role | Workspace dependencies |
| --- | --- | --- |
| [`crates/core`](../crates/core) | model, SQLite schema and migrations, scan, search, vault, command registry. No UI, no GStreamer. | none |
| [`crates/media`](../crates/media) | thumbnails, probing, GStreamer player, scrub previews, integrity checks, the network guard, `doctor` | `core` |
| [`crates/fetch`](../crates/fetch) | indexers, BitTorrent, HLS/DASH, resolvers (Online edition). Empty of network code unless its `network` feature is on. | `core`, `bridge` |
| [`crates/bridge`](../crates/bridge) | line protocol to the optional browser helper (framing and vetting only, no network code) | none |
| [`crates/ui`](../crates/ui) | egui app: theme, grid, viewer, player view, settings, onboarding, i18n, key table | `core`, `media`; `fetch` and `bridge` only with `network` |
| [`apps/jizzcraft`](../apps/jizzcraft) | desktop binary (eframe + wgpu) | `ui`, `core`, `media`; `fetch` only with `network` |
| [`apps/jizzcraft-cli`](../apps/jizzcraft-cli) | CLI on the same registry | `core`, `media`, `ui`; `fetch` only with `network` |
| [`apps/jizzcraft-browser`](../apps/jizzcraft-browser) | optional WebKitGTK helper, a separate process ([browser.md](browser.md)) | `bridge` |
| [`tests/e2e`](../tests/e2e) | end-to-end scenarios, empty without its `network` feature ([e2e.md](e2e.md)) | |
| [`xtask`](../xtask) | fixtures, icons, screenshots, packaging, e2e, perf | |

Rules (from [AGENTS.md](../AGENTS.md)), and what holds them:

- `core` never depends on `media`, `ui` or `fetch`; `media` never on `fetch`.
  Held by the manifests above (Cargo forbids the cycles that would follow).
- The default build of `jizzcraft` and `jizzcraft-cli` does not link
  `jizzcraft-fetch`: `default_apps_do_not_link_fetch` in
  [`crates/core/tests/offline_deps.rs`](../crates/core/tests/offline_deps.rs).
- No app or library crate depends on a browser engine (`gtk4`, `webkit6`,
  `wry`, …) in any feature set: `browser_engine_stays_in_the_helper`, same file.
- `openssl-sys` is allowed for SQLCipher only, with `libsqlite3-sys` as its
  only direct dependent: `only_sqlcipher_pulls_openssl_sys`, same file.
- Network code lives only in `crates/fetch`, behind the off-by-default
  `network` feature (alias `online` on the apps and `ui`).

## Runtime model

- **The UI thread never touches the pipeline.** A control thread in
  [`crates/media/src/player.rs`](../crates/media/src/player.rs) builds, drives
  and tears down GStreamer; the UI takes the newest frame from a one-slot
  mailbox and uploads it into one persistent wgpu texture.
- **No async runtime in the app, library or player:** `std::thread` and
  channels. `tokio` exists only inside `crates/fetch` in the network build
  (librqbit needs it), and `tokio` is on the banned list of the default build.
- **Anything slow is a job** with its own connection, a cancel flag and one
  toast with counts, never names.
- **Local by construction.** `jizzcraft_media::init()` removes
  network-capable elements (HTTP/RTSP/UDP sources, HLS/DASH/Smooth
  Streaming/SDP demuxers) from the process's own plugin registry, and every
  pipeline is passed to a watcher
  ([`crates/media/src/guard.rs`](../crates/media/src/guard.rs)). Pipelines only
  get `file://` URIs. In the network build, remote bytes reach the player
  through an `appsrc` fed in-process by `crates/fetch`; manifests are parsed
  in `fetch` only and the player keeps refusing manifest bytes.
- **Schema is append-only.** New tables and columns are a new entry in
  `MIGRATIONS`; old entries are frozen.
- **SQL injection surface.** Filter values reach SQL only as bound
  parameters; sort keys and filter keys are whitelisted (`SortKey`,
  `ALLOWED_KEYS`).

## Guarantees checked by tests

| Guarantee | Enforced by |
| --- | --- |
| The default build has no network crate (HTTP, TLS, async runtime, BitTorrent, browser engines) | `default_build_has_no_network_crates` in [`crates/core/tests/offline_deps.rs`](../crates/core/tests/offline_deps.rs), which runs `cargo tree`; `network_build_links_librqbit` is the positive control proving the check can see such crates |
| The player, stream player, thumbnails, probes, scrub previews and integrity checks open zero connections, even on HLS/DASH playlists disguised as videos | [`crates/media/tests/network.rs`](../crates/media/tests/network.rs) (`player_never_connects`, `stream_player_never_connects`, `timed_player_never_connects`, `thumbnails_and_probe_never_connect`, `integrity_checks_never_connect`, `scrub_previews_never_connect`): a local listener counts connections |
| The first-run wizard cannot start a network session | `wizard_never_starts_a_network_session` in [`crates/ui/src/app/onboarding.rs`](../crates/ui/src/app/onboarding.rs) |
| The engine never modifies, moves or deletes media | `engine_never_touches_media` in [`crates/core/tests/portability.rs`](../crates/core/tests/portability.rs): hashes a tree before and after every command |
| Content hashes: `oshash` matches independent references, `fp1` is unchanged | [`crates/core/tests/hashes.rs`](../crates/core/tests/hashes.rs) |
| Private performers and studios are never indexed or exported by default | `private_people_and_studios_are_not_indexed` and `export_import_round_trip_and_privacy` in [`crates/core/tests/meta.rs`](../crates/core/tests/meta.rs) |
| NFO import is read-only, bounded, refuses DOCTYPE, and a dry run writes nothing | [`crates/core/tests/interop_nfo.rs`](../crates/core/tests/interop_nfo.rs) (`hostile_input_is_refused`, `dry_run_changes_nothing_and_reports_exactly_what_apply_does`, `interop_sources_never_print`) and `engine_never_touches_media` |
| Stash import matches by content only, never opens a path from the export, and leaves a dry run's lazy hashes out | [`crates/core/tests/interop_stash.rs`](../crates/core/tests/interop_stash.rs) (`paths_in_the_export_are_never_opened`, `dry_run_changes_nothing_and_matches_apply`, `hostile_folders`) |
| NFO export never writes inside or above a library, never overwrites, and round-trips through the importer | [`crates/core/tests/interop_nfo_export.rs`](../crates/core/tests/interop_nfo_export.rs) (`destinations_inside_above_or_through_a_library_are_refused`, `a_symlink_leading_into_a_library_is_refused`, `round_trip_through_a_fresh_library_and_a_stable_second_export`) and `engine_never_touches_media` |
| M3U and metadata exports never write inside a library folder | `m3u8_refuses_a_destination_inside_a_library` in [`crates/core/tests/organize.rs`](../crates/core/tests/organize.rs), `meta_export_refuses_inside_a_library` in [`crates/core/tests/portability.rs`](../crates/core/tests/portability.rs) and `engine_never_touches_media` |
| Funscripts are parsed forgivingly, bounded, and only displayed | [`crates/core/tests/funscript.rs`](../crates/core/tests/funscript.rs) (`size_and_count_are_capped`, `bad_entries_are_dropped_and_positions_clamped`) and `engine_never_touches_media` |
| Visual fingerprints: the metric tree equals a linear scan, frames never reach the disk or the cache, hashing never touches media | [`crates/core/tests/phash.rs`](../crates/core/tests/phash.rs) (`bk_tree_matches_a_linear_scan`), [`crates/media/tests/phash.rs`](../crates/media/tests/phash.rs) (`library_phash_stores_hashes_without_touching_files_or_cache`) |
| Old migrations are never edited | `migrations_are_frozen` in [`crates/core/tests/migrations.rs`](../crates/core/tests/migrations.rs) |
| No listening socket or server crate in any build | `default_build_has_no_network_crates` (its list also bans DLNA, WebDAV, mDNS and HTTP server crates); [interop.md](interop.md) |
| Paths become stable opaque tags before reaching logs | `redacted_paths_are_stable_opaque_tags` in [`crates/core/src/privacy.rs`](../crates/core/src/privacy.rs) |
| Vault: nothing readable in clear after `vault enable` | `canary_nothing_readable_after_enable` in [`apps/jizzcraft-cli/tests/vault.rs`](../apps/jizzcraft-cli/tests/vault.rs) |
| Key table: no duplicate chords, exact modifiers, AZERTY digits and left-hand letters by physical key | tests at the end of [`crates/ui/src/keys.rs`](../crates/ui/src/keys.rs) |
| Every `tr()` string exists in every language; every error sentence is translated | [`crates/ui/tests/i18n_coverage.rs`](../crates/ui/tests/i18n_coverage.rs), [`crates/ui/tests/error_sentences.rs`](../crates/ui/tests/error_sentences.rs) |
| Text tokens meet WCAG AA contrast | [`crates/ui/tests/a11y.rs`](../crates/ui/tests/a11y.rs) |
| Online: the kill switch means zero connections across every entry point | `s14_kill_switch_means_zero_connections` in [`tests/e2e/tests/scenarios.rs`](../tests/e2e/tests/scenarios.rs); [`crates/fetch/tests/security.rs`](../crates/fetch/tests/security.rs); [`crates/fetch/tests/socks5_mock.rs`](../crates/fetch/tests/socks5_mock.rs) |
| Online: indexer and source keys never reach errors, debug output or listings | `secrets_never_reach_errors_debug_or_listings` in [`crates/fetch/tests/security.rs`](../crates/fetch/tests/security.rs) |
| Online: streaming, seeking, ABR, live HLS, DASH, torrents, disk full, all against loopback servers and the real player | 18 scenarios in [`tests/e2e/tests/scenarios.rs`](../tests/e2e/tests/scenarios.rs), described in [e2e.md](e2e.md) |

What tests do **not** cover is listed just as plainly in
[audit-untested.md](audit-untested.md) (line coverage, uncovered areas, and
why), and the README status block says which features were only checked
through commands and screenshots.

## Command registry and JSON CLI

One registry ([`crates/core/src/commands.rs`](../crates/core/src/commands.rs))
of named commands, each a `fn(&mut C, serde_json::Value) -> Result<Value>`.
The registry is generic over its context, so `core`, `media`, `fetch` and the
UI each register their own commands (`library.*`, `media.*`, `view.*`,
`player.*`, `app.*`, `fetch.*`). Menus, key bindings, the CLI and
`jizzcraft --cmd NAME=JSON` all call it; arguments that carry a secret field
are refused before any handler runs. New behaviour is added as a command
first, UI second.

```sh
jizzcraft-cli commands --json                     # [{"name": …, "help": …}, …]
jizzcraft-cli run history.list '{"limit":10}'     # any registered command
jizzcraft-cli query --view inbox --json           # built-in and saved views
jizzcraft --import fixtures --cmd 'view.filter={"tab":"Videos"}' --cmd 'media.open={"index":0}'
```

With `--json`, a failure prints `{"error": …, "code": …}` and the process
exits with that code. Shell completions and a man page:
`jizzcraft-cli gen completions <shell>`, `jizzcraft-cli gen man`. The CLI is
tested end to end on the real binary with a private home in
[`apps/jizzcraft-cli/tests/cli.rs`](../apps/jizzcraft-cli/tests/cli.rs).
The README's [Built for agents](../README.md#built-for-agents) section has
more examples.

## `cargo xtask`

`cargo xtask` is an alias for `cargo run --quiet -p xtask --`
([`.cargo/config.toml`](../.cargo/config.toml)). Everything it generates is
synthetic and safe for work.

| Command | What it does |
| --- | --- |
| `cargo xtask fixtures [DIR]` | gradients, patterns and test-pattern videos into `./fixtures` (needs `gst-launch-1.0`) |
| `cargo xtask screenshots` | README screenshots, rendered by the app itself (`--screenshot`) from fixtures |
| `cargo xtask icons` | app icons |
| `cargo xtask dist [--edition offline\|online] [--browser] [--prefix P] [--destdir D] [--no-build] [--tarball]` | stage an install tree or a release tarball |
| `cargo xtask check-packaging` | the packaging files agree with each other |
| `cargo xtask e2e [--allow-skips] [FILTER…]` | the end-to-end scenarios ([e2e.md](e2e.md)) |
| `cargo xtask perf` | performance budgets ([perf.md](perf.md)) |

## Threat model and known limits

What counts as a vulnerability, and what does not, is in
[SECURITY.md](../SECURITY.md). In short:

- **In scope:** any connection from the default build (including through
  GStreamer); in the network build, a connection before the user acts, an
  extra listening port, keys or queries leaking into logs, toasts, the
  database or backups, or a torrent writing outside its folder; modifying
  user media; a shell command built from a file name; full paths in logs
  without `JIZZCRAFT_DEBUG=1`; SQL injection through filters or saved views;
  an exploitable-looking crash on a crafted file.
- **Not security boundaries:** the age gate, the boss key, discreet mode and
  share-safe mode.
- **At rest:** the library and thumbnails are in clear unless the vault is on.
  The vault (alpha, no recovery) protects the database, its backups and the
  thumbnails against someone holding the disk while the app is closed or
  locked; not media files, a running unlocked session, RAM and swap,
  `config.json`, `fetch.json` or JSON exports. Full threat model:
  [vault.md](vault.md#threat-model).
- **Untrusted input:** every imported video is parsed by the system's
  GStreamer plugins without a sandbox (sandboxed decoding is on the
  [roadmap](../ROADMAP.md)). Video decoding and SQLCipher's OpenSSL are C,
  not Rust.
- **Network build:** your IP is visible to the indexers, peers and sites you
  use. Sources and resolvers are user-supplied and untrusted in what they
  return; loopback and LAN targets (including DNS rebinding) are refused, and
  the `require_proxy` kill switch is audited across entry points. Details:
  [SECURITY.md](../SECURITY.md#things-to-know), [streaming.md](streaming.md),
  [browser.md](browser.md).
- **Not covered by the guard:** `--bench-gst-play` runs the external
  `gst-play-1.0`. Files on network mounts are read through the OS like any
  other file.
- **Platforms:** Linux is the only platform anyone has run. Windows and macOS
  compile and are built in CI, never run on real hardware
  ([windows.md](windows.md)).
- **Playback cost:** 3 to 6 times the CPU of `gst-play-1.0` because colour
  conversion runs on the CPU ([bench.md](bench.md)).

## Performance budgets

`cargo xtask perf` runs [`crates/core/tests/perf.rs`](../crates/core/tests/perf.rs)
in a release build on a synthetic library of 10 000 files and fails when a
budget in [`perf/budgets.json`](../perf/budgets.json) is exceeded. Budgets
are about 10x the observed value (floor 100 ms), so they catch a lost index or
an accidental O(n²), not a 10 % drift; row counts are asserted exactly. Every
metric also runs on an encrypted library. Measured values and the reasoning:
[perf.md](perf.md). The CI `perf` job is not blocking yet
(`continue-on-error`) until shared runners prove stable.

Playback (frame pacing, CPU, RSS) needs a GPU and is measured by hand with
`jizzcraft --bench`: [bench.md](bench.md).

## Build, test and run CI locally

Rust 1.95 or newer, GStreamer with development headers, a C compiler with
`make` and `perl` (SQLCipher's vendored OpenSSL), and a Vulkan driver (the
tested backend).

```sh
# Arch / CachyOS
sudo pacman -S --needed base-devel perl rust gstreamer gst-plugins-base gst-plugins-good \
  gst-plugins-bad gst-plugins-ugly gst-libav vulkan-icd-loader \
  xdg-desktop-portal xdg-desktop-portal-gtk
# plus a Vulkan driver for your GPU, e.g. vulkan-radeon, vulkan-intel or nvidia-utils

# Debian / Ubuntu (the CI job uses this list)
sudo apt install build-essential pkg-config libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev gstreamer1.0-tools gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
  gstreamer1.0-libav libvulkan1 mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk

# Fedora (untested; H.264 needs RPM Fusion's gstreamer1-libav)
sudo dnf install gcc make perl pkgconf-pkg-config gstreamer1-devel gstreamer1-plugins-base-devel \
  gstreamer1-plugins-base-tools gstreamer1-plugins-good gstreamer1-plugins-bad-free \
  gstreamer1-plugins-ugly-free vulkan-loader mesa-vulkan-drivers \
  xdg-desktop-portal xdg-desktop-portal-gtk
```

```sh
git clone https://github.com/kerstz/JizzCraft && cd JizzCraft
cargo xtask fixtures                                   # synthetic test media in ./fixtures
cargo run --release -p jizzcraft -- --import fixtures  # try it without your own files
```

The CI jobs in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), and
how to run each one on your machine:

| CI job | Run locally | Required |
| --- | --- | --- |
| `check` | `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo fetch --locked && cargo test --workspace` | yes |
| `network` | `cargo clippy --workspace --all-targets --features jizzcraft/network,jizzcraft-cli/network -- -D warnings` and `cargo test --workspace --features jizzcraft/network,jizzcraft-cli/network` | yes |
| `e2e` | `JIZZCRAFT_REQUIRE_NAMESPACE=1 cargo xtask e2e` (about three minutes; s15 needs unprivileged user namespaces) | yes |
| `deny` | `cargo deny check advisories licenses bans sources` and `cargo audit` (CI ignores one advisory, see the workflow) | yes |
| `msrv` | `cargo +1.95 check --workspace --all-targets` | yes |
| `packaging` | `shellcheck -s sh packaging/install.sh packaging/appimage/build-appimage.sh`, `desktop-file-validate assets/*.desktop`, `appstreamcli validate --no-net assets/io.github.kerstz.JizzCraft.metainfo.xml`, `cargo xtask check-packaging` | yes |
| `install-e2e` | `cargo xtask dist --destdir target/stage --prefix /usr --tarball` then `sh packaging/smoke-install.sh dist/jizzcraft-*-linux.tar.xz` | yes |
| `perf` | `cargo xtask perf` | no |
| `windows` | cross-check from Linux: [windows.md](windows.md#checking-the-port-from-linux) | no (experimental) |

CI sets `JIZZCRAFT_REQUIRE_PLUGINS=1`, so a media test whose GStreamer element
is missing fails instead of printing `skipped: …`. Set it locally too if you
want the same strictness. Coverage: see
[audit-untested.md](audit-untested.md#reproduce).

## Contributing

Start with [CONTRIBUTING.md](../CONTRIBUTING.md) and [AGENTS.md](../AGENTS.md).
Features are proposed and voted on in the open
([feature-process.md](feature-process.md)). Hard rules in one breath: no real
adult content anywhere (fixtures are synthetic), no site, indexer or source
presets, no network code in the Offline edition, never write to user media,
schema append-only, command first and UI second. Vulnerabilities go through a
private advisory ([SECURITY.md](../SECURITY.md)).
