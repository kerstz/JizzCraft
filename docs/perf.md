# Performance budgets

`cargo xtask perf` runs `crates/core/tests/perf.rs` in a release build and
prints one line per metric, failing when a budget in `perf/budgets.json` is
exceeded. Nothing is read from your library: the test builds 10 000 empty,
synthetically named `.mp4` files in a temp directory and a database beside
them. No network, no user data.

| Metric | What it measures | Typical (dev laptop) |
| --- | --- | --- |
| `scan_cold_10k` | first scan of 10k files | 0.8 s |
| `scan_rescan_10k` | rescan, nothing changed | 50 ms |
| `db_open_10k` | open a current-schema library | < 1 ms |
| `migrate_v1_10k` | v1 database with 10k rows migrated to current | 65 ms |
| `query_*` | one 200-row grid page (name, size, added, random) | 0.2 to 10 ms |
| `count_all`, `query_name_filter` | count and name filter | < 1 ms |
| `fts_*` | full-text search: two words, prefix page, miss | < 1 ms |

## How budgets are chosen

- Wall-clock budgets are about 10x the observed value, with a 100 ms floor for
  anything under 10 ms. They catch a lost index or an accidental O(n^2), not a
  10 % drift. Each timing is the best of several runs.
- Row counts are asserted exactly (added, unchanged, page lengths, search hits).
  Counting beats timing wherever a regression shows up as a count.
- To change a budget, edit `perf/budgets.json` with the numbers from a run on
  CI, and say why in the commit.

## CI

The `perf` job in `.github/workflows/ci.yml` runs `cargo xtask perf`. It has
`continue-on-error: true` until shared runners have proven stable; remove it
then.

## Not covered yet

Issue #17 also lists RSS per stream, time to first frame and UI frame cost at
1080p. Those need GStreamer and a GPU, so they stay manual for now
(`docs/bench.md`, `jizzcraft --bench`).
