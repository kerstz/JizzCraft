# Audit: code that was not covered by tests

Measured with `cargo llvm-cov --workspace --summary-only --ignore-filename-regex 'xtask|/tests/|/lang/'`
(default features), plus `cargo llvm-cov -p jizzcraft-fetch --features network` for the network build.
Line coverage went from **43.7 %** to **46.2 %** across the workspace in this pass (the
network crate separately: 62 % → see below). Numbers are lines, integration tests included.

## What the audit found

| Finding | Severity | Status |
| --- | --- | --- |
| `fetch.indexers.add` (registry command) ignored the "name defaults to the host" rule that the UI path applies: an empty name stayed empty. Found by the new `crates/fetch/tests/commands.rs`. | Low (cosmetic, CLI) | Fixed |
| `fetch.*` registry commands had **0 %** coverage. They carry the "never reveal a key or a download link" rule. | Medium | Covered: indexer CRUD without key leak, search results show magnets only and `has_link` for links, plain errors, download folder import |
| Both `main.rs` files at **0 %** (argument parsing, mapping to commands, exit codes). | Medium | CLI covered end to end by `apps/jizzcraft-cli/tests/cli.rs` (real binary, private home). The desktop `main.rs` is still untested (see below) |
| `paths`, `privacy::redact`, `Error` messages, saved `Config` (load/save/corrupt/old file), size/duration/date formatting, fetch config defaults: untested. | Low–medium | Covered |
| `media::service` (thumbnail queue) at **0 %**. | Medium | Covered: results, per-item errors without paths, cache reuse, nothing written beside originals |

## Still not covered, and why

| Area | Coverage | Risk | Notes |
| --- | --- | --- | --- |
| `crates/ui/src/app.rs`, `app/sidebar.rs`, `app/tools.rs`, `player_view.rs`, `widgets.rs`, `theme.rs` (egui drawing and event handling) | 0–7 % | Medium | Needs a headless egui harness. The `--screenshot` path renders the whole app once and is used by hand; a golden-image smoke test per tab/language would be the next step. Registered commands behind the UI are covered through the registry and CLI. |
| `apps/jizzcraft/src/main.rs` | 0 % | Low | Flag parsing and the magnet hand-off. Extract the parser into a function and unit-test it. |
| `crates/media/src/integrity.rs` | 47 % | Medium | Decode-integrity paths for truncated/corrupt containers need more synthetic broken clips. |
| `crates/ui/src/bench.rs` | 0 % | Low | Developer benchmark. Nobody benchmarks the benchmark. |
| `crates/fetch/src/service.rs` | ~74 % | Medium | Torrent paths are covered by the loopback swarm tests. Not covered: SOCKS5 proxy, listening for incoming peers, rate limits, disk-full handling, Prowlarr indexer listing over HTTP. |
| `crates/ui/src/app/downloads.rs` | 0 % | Medium | The Play flow state machine (list → pick → add → open) is only exercised by hand. Extract it from egui and test the transitions with fake receivers. |
| Windows / macOS code paths | — | High | `paths`, the single-instance lock and the external-player lookup have platform branches that no test here runs. CI on those platforms is required before a release. |

## Untested assumptions worth a manual check

- `File::try_lock` (single instance) semantics on Windows and network filesystems.
- Renaming an open `.part` file on Windows (relies on std's default share mode).
- GStreamer plugin availability on Windows/macOS bundles: the player reports a plain
  missing-codec message, but the packaged plugin set has not been exercised.

## Reproduce

Coverage goes up when you add tests; it has never once gone up from staring
at the report.

```sh
cargo install cargo-llvm-cov --locked
cargo llvm-cov --workspace --summary-only --ignore-filename-regex 'xtask|/tests/|/lang/'
cargo llvm-cov -p jizzcraft-fetch --features network --summary-only
```
