# Roadmap

Today: local library, thumbnails, image viewer, GStreamer player, favorites,
ratings, hide, resume and history, markers, A-B loop, speed and frame step,
scrub previews, views and folder tree, triage, fingerprint relink, integrity
check, backup and metadata export, portable mode, age gate, boss key, discreet
mode, CLI, network guard for GStreamer. Opt-in `network` build: BitTorrent
client (librqbit), user-configured Torznab indexers (Jackett, Prowlarr), and
streaming from sources you configure: direct video addresses, HLS and DASH on
demand, resolvers, a quality chip and a buffered-ahead band on the timeline.

Also shipped since: live HLS, separate fragmented-MP4 audio and video tracks
with an audio-track chip and language preference, local `.srt` / `.vtt`
subtitles, tags and categories with full-text search (FTS5), multi-select and
bulk actions, folder removal from the library UI, and keys for triage, random
pick, slideshow, markers and frame step.

Not done yet: live DASH and low-latency HLS, audio in TS or WebM, subtitles
for streams and embedded subtitles, saving a stream as a feature, the
encrypted vault (issue #13: format, key wrap, database opening and conversion
engine are written, nothing in the app calls them yet), and validation on real
Windows and macOS machines (issues #14, #15).

## Next milestones (explicitly out of scope until the MVP is solid)

- **Reachability**: on-screen buttons for what is a key or a command today
  (triage, random pick, slideshow); automatic fingerprinting of items that
  carry user data.
- **Player**: NV12/I420 caps with YUV→RGB in a wgpu shader (skips the CPU
  `videoconvert`); embedded subtitles and an audio-track picker for local
  files; seamless segment loops.
- **Library**: file watching instead
  of manual rescan, a relink picker for moved library roots, a
  restore-from-backup flow.
- **Privacy**: encrypted vault for the library database and thumbnail cache
  (issue #13, in progress, not delivered).
- **Hardening**: thumbnail, preview and probe decoding in a sandboxed worker
  process (GStreamer parses every imported video today).
- **Distribution**: published AUR packages (#16), Flatpak, AppImage; Windows installer and macOS bundle that embeds GStreamer (#15); real-hardware validation on Windows (#14).
- **Quality**: an end-to-end harness with loopback servers and a swarm (#18); performance budgets as CI checks (#17).
- **Agents**: MCP server (stdio) on top of the command registry; a local control
  channel over a Unix socket, never TCP.
- **Assistance**: local, offline tagging (classification only; nothing is
  generated or altered).
- **Extensibility**: plugins. (Not plugins for the plugin system. We know where
  that road goes.)
- **Website**: project site and docs. Until then the README is doing its best.

Not planned, ever: accounts, cloud sync, telemetry, bundled indexers, per-site
scrapers or site rules, remote URLs in the player, media generation. The default build stays
offline and is tested to link no network crate; networking exists only in the
opt-in `network` build, with no bundled sites.

Proposals, votes and status live on GitHub: see [docs/feature-process.md](docs/feature-process.md).
