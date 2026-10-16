# Performance budgets

`cargo xtask perf` runs `crates/core/tests/perf.rs` in a release build and
prints one line per metric, failing when a budget in `perf/budgets.json` is
exceeded. Nothing is read from your library: the test builds 10 000 empty,
synthetically named `.mp4` files in a temp directory and a database beside
them. No network, no user data.

The `cold_*` rows (a fresh connection and its first query) exist in both modes.
Every metric runs twice: on a clear library and on an encrypted one (the
`vault_` rows, SQLCipher, a generated key), then the conversion of the clear
10k library to an encrypted one and back is timed, and so is one unlock.

| Metric | What it measures | Typical (dev laptop) |
| --- | --- | --- |
| `scan_cold_10k` | first scan of 10k files | 0.8 s |
| `scan_rescan_10k` | rescan, nothing changed | 50 ms |
| `db_open_10k` | open a current-schema library | < 1 ms |
| `migrate_v1_10k` | v1 database with 10k rows migrated to current | 65 ms |
| `query_*` | one 200-row grid page (name, size, added, random) | 0.2 to 10 ms |
| `count_all`, `query_name_filter` | count and name filter | < 1 ms |
| `fts_*` | full-text search: two words, prefix page, miss | < 1 ms |

## Vault mode

Measured on the development machine (release build, 3 consecutive runs of
`cargo xtask perf`; the three runs agree within a few percent). Times in
milliseconds.

| Metric | Clear | Vault | Overhead | Budget (vault) |
| --- | --- | --- | --- | --- |
| `scan_cold_10k` | 293 to 302 | 305 to 312 | +4 % | 3500 |
| `scan_rescan_10k` | 51.7 to 52.2 | 59.3 to 59.5 | +15 % | 1000 |
| `db_open_10k` | 0.30 | 0.36 | +20 % | 100 |
| `migrate_v1_10k` | 63.4 to 64.2 | 78.9 to 80.3 | +25 % | 1000 |
| `query_name_page` (warm) | 0.17 | 0.17 | 0 % | 100 |
| `query_size_desc_page` (warm) | 9.4 to 9.6 | 9.4 to 9.6 | 0 % | 250 |
| `fts_two_words` (warm) | 0.18 | 0.18 | 0 % | 100 |
| `cold_query_name_page` | 0.64 to 0.65 | 0.93 to 0.95 | +45 % | 100 |
| `cold_query_size_desc_page` | 10.1 to 10.2 | 13.1 to 13.4 | +30 % | 250 |
| `cold_fts_two_words` | 0.74 to 0.77 | 1.28 to 1.29 | +70 % | 100 |
| `vault_convert_enable_10k` | | 140 to 143 | | 1500 |
| `vault_convert_disable_10k` | | 117 to 120 | | 1500 |
| `vault_unlock` (Argon2id, default cost) | | 67 to 69 | | 1000 |

How to read it:

- **Warm queries cost nothing extra.** SQLite keeps decrypted pages in its page
  cache; once a page was read, the encryption is not in the path any more.
- **Cold reads cost 30 to 70 %.** A "cold" metric opens a fresh connection and
  runs its first query, which is what a user waits for right after unlocking.
  These are sub-millisecond to 13 ms operations: the absolute cost is small.
  Page decryption and HMAC verification (AES-256 and HMAC-SHA512 per 4 KiB
  page) are the difference.
- **Writes** (cold scan of 10k rows) are within 4 %; the extra work per page
  is small next to the file system and SQL work.
- **Conversion** of a 10k library takes about 0.14 s to encrypt and 0.12 s to
  decrypt, of which one Argon2id derivation (about 70 ms at 64 MiB, 3
  iterations) is half. It scales with the database size, not with the media
  count. Larger libraries were not measured; the work is linear in the
  database size and runs as a job with a progress screen and a Cancel button.
- The unlock figure is the Argon2id cost on this CPU; a slower machine is
  slower on purpose (it is the brute-force cost), always off the UI thread.

Not measured: thumbnail decryption (XChaCha20-Poly1305 over a few KiB per
tile; not on a GPU-bound path) and conversion of a very large library.

## How budgets are chosen

- Wall-clock budgets are about 10x the observed value, with a 100 ms floor for
  anything under 10 ms. They catch a lost index or an accidental O(n^2), not a
  10 % drift. Each timing is the best of several runs.
- Row counts are asserted exactly (added, unchanged, page lengths, search hits).
  Counting beats timing wherever a regression shows up as a count.
- Vault budgets follow the same rule against the vault numbers above (about
  10x, floor 100 ms), not as a ratio to the clear ones: a ratio of two noisy
  numbers flakes on shared runners, while a plain ceiling still catches a
  lost index or a full-table decryption.
- To change a budget, edit `perf/budgets.json` with the numbers from a run on
  CI, and say why in the commit.

## CI

The `perf` job in `.github/workflows/ci.yml` runs `cargo xtask perf`. It has
`continue-on-error: true` until shared runners have proven stable; remove it
then.

## Player budgets (local, needs a GPU)

`cargo xtask perf --player [FILE]` builds the release app, runs
`jizzcraft --bench FILE --seconds 10` (default
`fixtures/videos/bench-1080p30.mp4`, made by `cargo xtask fixtures`) and checks
the `BENCH_JSON` line against `perf/player_budgets.json`: time to first frame
(`ttff_ms`), UI frame cost p95 (`ui_cost_p95_ms`), peak RSS (`rss_peak_mb`) and
dropped frames (`dropped`). It exits non-zero when one is over. It needs
GStreamer, a GPU and a display, so it is not part of CI. The ceilings are first
guesses; tighten them with data from real machines.

## Not covered yet

Issue #17 asked for RSS per stream, time to first frame and UI frame cost at
1080p. They are now measured by `--player`, locally only (`docs/bench.md`).
