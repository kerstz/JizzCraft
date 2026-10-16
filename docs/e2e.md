# End-to-end scenarios

`tests/e2e` (crate `jizzcraft-e2e`, never published) drives the real stack:
a loopback HTTP server or BitTorrent swarm, then `jizzcraft-fetch`, then the
real GStreamer `Player`. Everything is synthetic and SFW: clips come from
`videotestsrc` / `audiotestsrc`, generated in the test with `gst-launch-1.0`.
Everything stays on `127.0.0.1`.

## Running

```sh
cargo xtask e2e                  # all scenarios, serial, plugins required
cargo xtask e2e s06 s09          # only the scenarios whose name matches
cargo xtask e2e --allow-skips    # a missing GStreamer element skips, with a reason
E2E_DEBUG=1 cargo xtask e2e s02  # print every request the test server sees
```

It runs `cargo test -p jizzcraft-e2e --features network -- --test-threads=1`
with `JIZZCRAFT_REQUIRE_PLUGINS=1`. The crate is empty without its `network`
feature, so `cargo test --workspace`, `cargo clippy --workspace` and the
`cargo tree` offline checks never see a network dependency from it.

Needs `gst-launch-1.0` and these elements: `playbin`, `appsink`, `vp8enc`,
`webmmux`, `opusenc`, `x264enc`, `h264parse`, `mpegtsmux`, `mp4mux`,
`avdec_h264`, `qtdemux`, `tsdemux`, `matroskademux`. (Ubuntu: `gstreamer1.0-
plugins-{base,good,bad,ugly}` and `gstreamer1.0-libav`.) A whole run takes
about three minutes in a debug build; CI runs it in its own `e2e` job, which is a required check.

## What every scenario checks

- time to first frame under 20 s;
- the position never goes backwards (500 ms of jitter allowed), seeks rebase;
- failures are fixed sentences from `errors::classify`, never a URL, host,
  port or path;
- streams write no file in the download folder;
- after everything is dropped, threads and descriptors return to within a few
  of where they started (`/proc/self/{task,fd}`, Linux only), and resident
  memory grew by less than 200 MiB.

## Scenarios

| Test | What it proves |
| --- | --- |
| `s01_range_http_plays_to_the_end` | `open_any` -> `StreamTicket::Live` -> `Player::open_stream` plays a ranged HTTP file to the end, with `Range` requests and all 180 frames. |
| `s02_seek_into_data_that_has_not_arrived` | A seek past the read-ahead while the server withholds that span waits (no error), then plays once it is released. |
| `s03_64_kbit_server_still_plays` | A 64 kbit/s server (about 15 s of transfer) plays: only stalls time out, there is no total timeout. |
| `s04_dropped_connection_mid_body_recovers` | Two body-carrying requests are cut halfway; the reader retries and the clip plays to the end. |
| `s05_expiring_url_fails_with_the_expired_sentence` | A signed address that starts answering 403 ends in "The link expired.", with nothing about the address. |
| `s05b_expiring_url_recovers_through_the_resolver` | The same expiry on an address from a resolver is renewed by asking the resolver again (unix only). |
| `s06_hls_ts_vod_plays_and_seeks_within_a_segment` | HLS with TS segments, through `open_any` -> `Timed` -> `Player::open_timed`; an accurate seek lands within one segment. |
| `s06b_live_hls_plays_from_the_edge_and_ends_cleanly` | A sliding HLS playlist (no `ENDLIST` until the end): starts near the live edge, `is_live`, no duration, seeking ignored, plays until `ENDLIST`, and the reload loop stops with the player. |
| `s07_hls_fmp4_with_a_map` | HLS fMP4 with `#EXT-X-MAP` (init segment first). |
| `s08_hls_aes_128` | AES-128 segments with the key fetched from the server and the media-sequence IV. |
| `s09_abr_goes_down_when_throttled_and_up_when_released` | With a short `AbrConfig`, a throttled top variant makes the player go down then up (`current_variant()` is seen as 1, 0, 1). |
| `s10_dash_segment_template_muxed` | DASH `SegmentTemplate` with a muxed representation. |
| `s10b_dash_separate_audio_and_video_tracks` | DASH with a video set and a separate AAC audio set (fragmented MP4): both tracks are fetched, combined into one stream for `Player::open_timed`, a seek works and playback reaches the end. |
| `s11_three_peer_swarm_plays_while_downloading` | Three loopback seeders: the first frame comes before the download is complete; player pause/resume, torrent pause/resume, and a seek into missing pieces. |
| `s12_magnet_with_no_peers_gives_a_sentence_soon` | A magnet nobody has ends in the "no peers" or "details in time" sentence within the configured metadata timeout, without the info hash. |
| `s13_restart_keeps_the_torrent_and_ask_does_not_resume_it` | After dropping and recreating the service, the torrent is back and paused; policy Ask does not wake it. |
| `s14_kill_switch_means_zero_connections` | With `require_proxy` and no proxy, none of `open_any`, `stream_url`, `play_http`, a resolver run, `search`, a torrent listing or a magnet opens a single connection (counted at the server) or starts the resolver. |
| `s15_disk_full_is_a_sentence_and_leaves_nothing_behind` | Linux only. The test re-executes itself in an unprivileged user+mount namespace (`unshare -Urm`) with a 4 MiB tmpfs. A ranged-less HTTP download that does not fit ends in "Disk full. Downloads paused." (no path, no address), the `.part` file is removed, and once space is freed the same address downloads byte for byte. A SQLite database on the same volume fails the write, passes `quick_check`, and writes again after the space returns. A torrent on the same volume, with the free-space query made to lie so that it runs into a real ENOSPC (the engine asserts on the partial write), ends paused with the typed "disk full" status, refuses to start while there is no room, and completes byte for byte once space is freed; the process never panics. Skips with a printed reason where user namespaces are unavailable; `JIZZCRAFT_REQUIRE_NAMESPACE=1` (CI) makes that a failure. |

### What s15 guarantees for torrents

librqbit 9.0.1 asserts (`file_ops.rs`, `debug_assert_eq!` on the written
length) when a write is partial, which is what a real ENOSPC produces. That
assertion only exists in builds with debug assertions (dev profile, tests);
in a release build the partial write is accepted silently and the piece fails
its hash check. Both leave the torrent alive but not moving. JizzCraft does
not patch librqbit; it guarantees, on the paths it controls:

- the application stays alive: engine tasks run under tokio (a panic ends
  that task only), every service call runs in its own task and answers with a
  typed error on a panic, and the player's reader catches an engine panic;
- the state is typed: free space is checked before adding, before resuming
  and every second while downloading (remaining bytes + 64 MiB margin); below
  that the torrent is paused with "Disk full. Downloads paused.". A torrent
  that is stuck after an engine panic (no byte for 20 s) is paused too, as
  "disk full" for a write fault, "error" otherwise;
- resuming works once there is room; without room it stays paused. A torrent
  that librqbit moved to its error state (a fatal ENOSPC) cannot be started
  from there (it refuses to "create" files that exist), so it is forgotten
  (files kept) and added again over its own partial files with the same file
  selection and peers; its id changes. This is the one place where a torrent
  is added with `overwrite: true`, and only after every existing file is shown
  to be the same inode it wrote (a regular file, never longer than the torrent
  says; a link or a replaced file makes the restart refuse with "files
  changed on disk; refused"). `security.rs::overwrite_true_is_always_gated`
  keeps it that way.

Not guaranteed: a disk that fills between two checks (another program
writing fast) can still hit ENOSPC, and then the engine's own panic or fatal
error happens first; the typed state follows within a second or twenty, not
instantly. Windows has no free-space query yet (`diskspace.rs`), so only the
runtime path (ENOSPC error, stuck watchdog after a write fault) applies there.
A quiet swarm without any panic is never paused by the watchdog.

## Adding one

Use `jizzcraft_e2e::http::Server` (a closure per request: `Reply::ok`,
`.bps()`, `.cut()`, `.hold()`), `fixtures::{webm, ts_segments, fmp4}` for
media, `drive::Pump` to run the player, and take a `proc::Baseline` before
opening anything. Start the test with `let _g = serial();` and
`if !ready(ELEMENTS) { return; }`.
