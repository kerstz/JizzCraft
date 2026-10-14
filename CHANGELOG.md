# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) once 1.0 is out. Before that,
anything may change between minor versions.

## [0.1.0]

First public alpha.

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
