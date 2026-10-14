<p align="center">
  <img src="assets/icons/hicolor/128x128/apps/org.jizzcraft.JizzCraft.png" width="96" alt="JizzCraft icon">
</p>

<h1 align="center">JizzCraft</h1>

<p align="center"><strong>A private, offline viewer and library manager for local adult media.</strong></p>

<p align="center">Native Linux desktop application written in Rust. No account, no telemetry, no network connections.</p>

<p align="center">
  <a href="https://github.com/kerstz/JizzCraft/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/kerstz/JizzCraft/ci.yml?branch=main&style=flat-square&label=CI" alt="CI status"></a>
  <img src="https://img.shields.io/badge/language-Rust-b7410e?style=flat-square" alt="Language: Rust">
  <img src="https://img.shields.io/badge/platform-Linux-2f7bf5?style=flat-square" alt="Platform: Linux">
  <img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-3a3a3a?style=flat-square" alt="License: MIT OR Apache-2.0">
  <img src="https://img.shields.io/badge/status-early%20alpha-d69e2e?style=flat-square" alt="Status: early alpha">
</p>

> [!WARNING]
> **18+ only.** JizzCraft is intended for adults managing media they own or are entitled to keep. It ships no media, downloads nothing and opens no network connections. All test data, fixtures and screenshots in this repository are synthetic and safe for work.

> [!NOTE]
> JizzCraft is an independent parody project. It is not affiliated with or endorsed by the ArtCraft team or Crafting Apps. See [Parody disclaimer](#parody-disclaimer).

![Library grid](docs/screenshots/library.png)

## Features

- **Library**: recursive indexing of local folders into SQLite; search by file name; tabs for images, videos and favorites; sorting by name, date added, size, duration, rating, last played or play count, or a stable shuffle; manual rescan; missing files are flagged rather than dropped.
- **Organisation**: 0–5 star ratings, a reversible *hide* (a database flag), built-in and saved views, a folder tree with item counts, and an inbox triage mode (keep, hide, favorite, undo; nothing is ever deleted).
- **Moved files keep their data**: content fingerprints let a renamed or moved file keep its favorite, rating, history and markers.
- **Thumbnails and scrub previews**: generated in the background and stored in the XDG cache directory, never next to your files.
- **Image viewer**: fit, 100 %, cursor-anchored zoom, pan, previous/next, EXIF orientation.
- **Video player**: GStreamer playback rendered through wgpu, with seeking and scrubbing, volume, fullscreen, playback speed, A-B loop, frame step, markers, resume points, watch history and autoplay-next.
- **Maintenance**: integrity check (decode test, optional checksum against silent corruption), backup, metadata export/import, cache clearing, portable mode.
- **Privacy controls**: 18+ age gate, boss key, discreet mode, screen-share-safe mode; watch history can be paused, cleared or hidden.
- **Command registry and CLI**: menus, shortcuts, a JSON command-line tool and `--cmd` startup commands all call the same named commands.
- **Local only**: no accounts, telemetry, update checks or remote URLs; GStreamer's network-capable elements are disabled inside the application.

The interface follows familiar file-browser conventions: a top bar with tabs and search, a sidebar with views and folders, a thumbnail grid, and an inspector panel.

## Details

### Library

Folders are imported through the native folder dialog (XDG desktop portal) and indexed recursively. Symbolic links and hidden files or directories inside imported folders are skipped. Files are read, never modified, moved or deleted; a file that disappears is flagged as missing until it comes back. Re-importing a folder is a no-op, permission errors are counted rather than fatal, and a directory that cannot be read does not mark its files missing. The grid is virtualized: only visible rows are rendered and only a window of rows is loaded from SQLite.

### Organisation

- **Ratings** with `Alt+0`–`Alt+5` or the stars in the inspector. **Hide** with `X` in the image viewer or the inspector button; hidden items leave the library and appear in the *Hidden* view.
- **Built-in views**: Continue, Recently played, Top rated, Unrated, Inbox, Hidden, Damaged, Missing, With markers. Any filter can be stored with *Save view*. The sidebar also shows the folder tree with item counts.
- **Inbox triage** reviews newly imported items one at a time: `→` keep, `↓` hide, `↑` favorite, `←` undo, `Esc` leave. It currently has no button: start it with `jizzcraft --cmd triage.start`.
- **Moved and renamed files**: run *Library → Fingerprint files* (or `jizzcraft-cli fingerprint`) and the next rescan recognises a moved or renamed file by its size and the first and last 64 KiB of its content. By default only items that carry user data are fingerprinted.
- **Shuffle** is a stable order that never repeats or skips items while scrolling. A random pick (`view.random`) and a slideshow (`view.slideshow`) are available as commands.

![Inbox triage](docs/screenshots/triage.png)

### Thumbnails and scrub previews

Images are downscaled to about 320 px in a background thread pool. Videos go through a dedicated GStreamer pipeline (`uridecodebin → videoconvert → videoscale → appsink`), paused and seeked to about 10 % of their duration. Hovering the player timeline shows scrub previews, decoded on demand by a separate pipeline. Everything is cached in the XDG cache directory, keyed by a hash of path, size and modification time; discreet and share-safe modes use blurred variants. Previews can be turned off with `"no_previews": true` in `config.json`.

### Image viewer

Fit to window, 100 %, mouse-wheel zoom anchored on the cursor, drag to pan, previous/next, `Esc` to return to the library. EXIF orientation is applied.

![Image viewer](docs/screenshots/viewer.png)

### Video player

`playbin` with an `appsink` video sink, uploaded into a single persistent wgpu texture. Scrubbing coalesces seeks (key-unit while dragging, accurate on release). Pixel aspect ratio and orientation are honoured. The last frame is held at the end of the stream with a replay control. Playback errors name the missing GStreamer element or plugin.

- **Speed**: the player-bar chip cycles 0.5×–2×; the `player.speed` command accepts 0.25×–4×. Pitch is preserved when `scaletempo` (gst-plugins-good) is installed.
- **A-B loop**: the player-bar chip sets the start, then the end, then clears the loop.
- **Markers**: shown as ticks on the timeline and listed in the inspector (click to seek, × to remove). Adding markers and jumping between them are commands for now (`markers.add`, `player.marker.jump`), as is frame stepping (`player.frame`).
- **Resume and history**: playback resumes where you stopped (for videos of a minute or more). History is aggregate (play count, last played, resume point), never an event log.
- **Autoplay-next**: the *Auto* chip plays the next item after a three-second countdown; `Esc` cancels it and switches autoplay off.

![Video player](docs/screenshots/player.png)

### Privacy controls

`Ctrl+Shift+H` (boss key) pauses, mutes, switches to discreet mode and minimizes the window. Discreet mode retitles the window "Viewer", replaces the icon, hides branding, file names, folder names and history views, and blurs thumbnails. Share-safe mode blurs every thumbnail until it is hovered. An age confirmation is shown on first launch. Watch history can be paused, hidden in the interface or cleared from the *Library* menu.

![Discreet mode](docs/screenshots/discreet.png)

These controls are conveniences, not security boundaries; see [Privacy](#privacy).

### Maintenance and portability

The *Library* menu runs each job in the background and reports counts only, never file names:

- **Integrity check**: a quick decode test, or a full decode with an optional whole-file checksum that flags files whose content changed while their size and date did not. Results appear in the *Damaged* view.
- **Backup** of the library database (`VACUUM INTO`, never overwrites an existing file).
- **Metadata export/import** of favorites, ratings, hidden flags, markers and saved views, keyed by fingerprint. File names and watch history are only exported on request.
- **Cache clearing** for thumbnails and previews.

`--portable` keeps the database, configuration and cache in `jizzcraft-data/` next to the executable; `--home DIR` or `JIZZCRAFT_HOME` uses another directory.

## Keyboard shortcuts

Press `?` in the library to display the shortcuts in the application. The left-hand preset is enabled from that window ("Left-hand keys") or with `--cmd app.left_hand.toggle`, and is saved in the configuration. Digits and left-hand letters match the physical key, so they work on AZERTY and other layouts.

| Action | Default | Left-hand preset |
| --- | --- | --- |
| **Anywhere** | | |
| Import a folder | `Ctrl+O` | `Ctrl+O` |
| Rescan the library | `Ctrl+R` | `Ctrl+R` |
| Boss key | `Ctrl+Shift+H` | `Ctrl+Shift+H` |
| Rate 1–5 stars / clear the rating | `Alt+1`–`Alt+5` / `Alt+0` | `Alt+1`–`Alt+5` / `Alt+0` |
| **Library** | | |
| Move selection | `←` `↑` `↓` `→` | `W` `A` `S` `D` or arrows |
| Open selected item | `Enter` | `E`, `Space` or `Enter` |
| First / last item | `Home` / `End` | `Home` / `End` |
| Library / Images / Videos / Favorites | `1` – `4` | `1` – `4` |
| Focus search | `/` | `/` |
| Clear search | `Esc` | `Q` or `Esc` |
| Toggle favorite | `S` | `F` |
| Shortcut reference | `?` | `?` |
| **Image viewer** | | |
| Previous / next item | `←` / `→` | `A` / `D` or arrows |
| Fit / 100 % | `0` / `1` | `Z` or `0` / `1` |
| Zoom in / out | `+` (or `=`) / `-` | `W` / `S`, or `+` / `-` |
| Toggle favorite | `S` | `F` |
| Hide / unhide | `X` | `X` |
| Back to library | `Esc` | `Q` or `Esc` |
| **Video player** | | |
| Play / pause | `Space` | `Space` |
| Back / forward 5 s | `←` / `→` | `A` / `D` or arrows |
| Volume up / down | `↑` / `↓` | `W` / `S` or arrows |
| Mute | `M` | `X` or `M` |
| Fullscreen | `F` | `G` |
| Toggle favorite | `S` | `F` |
| Previous / next item | `Page Up` / `Page Down` | `Shift+A` / `Shift+D`, or `Page Up` / `Page Down` |
| Restart | `Home` | `Home` |
| Leave fullscreen, then back | `Esc` | `Q` or `Esc` |
| **Triage** (on top of the viewer or player) | | |
| Keep / hide / favorite / undo | `→` / `↓` or `X` / `↑` or `S` / `←` | `D` / `S` or `X` / `W` / `A`, or arrows |
| Seek back / forward 5 s in a video | `Shift+←` / `Shift+→` | `Shift+←` / `Shift+→` |
| Leave triage | `Esc` | `Q` or `Esc` |

Mouse: double-click opens an item; in the image viewer the wheel zooms and dragging pans; in the player the timeline can be dragged and shows previews on hover. Shuffle, *Save view*, sorting, the *Library* menu and the player-bar chips (speed, A-B loop, autoplay) are on-screen controls. Triage, random pick, slideshow, frame step, markers and the sidebar toggle do not have a shortcut yet; they are reachable as commands (see below).

## Command-line interface and automation

All user actions are named commands in a single registry (`library.*`, `media.*`, `views.*`, `history.*`, `markers.*`, `view.*`, `player.*`, `app.*`, …). Menus, shortcuts, the CLI and `--cmd` all dispatch through it. `jizzcraft-cli commands` lists the library commands; `jizzcraft-cli run NAME 'JSON'` calls any of them.

```sh
jizzcraft-cli scan ~/Videos --json
jizzcraft-cli query --type video --name holiday --json
jizzcraft-cli fav 42 --json
jizzcraft-cli rate 42 5
jizzcraft-cli query --min-rating 4 --sort rating --desc --json
jizzcraft-cli query --view inbox --json            # built-in or saved views
jizzcraft-cli stats --json
jizzcraft-cli fingerprint --all                    # enables move detection
jizzcraft-cli check --full --checksum              # integrity check
jizzcraft-cli export meta.json                     # names and history only with --names / --history
jizzcraft-cli import meta.json --dry-run
jizzcraft-cli run history.list '{"limit":10}'

# Desktop application: run commands after start-up
jizzcraft --import ~/Videos --cmd 'view.filter={"tab":"Videos"}' --cmd 'media.open={"index":0}'
jizzcraft --cmd triage.start
jizzcraft --cmd 'view.slideshow={"action":"start","interval_s":5}'

# Render a screenshot of the application on synthetic media
jizzcraft --import fixtures --screenshot out.png --assume-adult

# Playback benchmark, compared with gst-play-1.0
jizzcraft --bench fixtures/videos/bench-1080p30.mp4 --seconds 30 --bench-gst-play
```

Conventions for contributors and coding agents are in [`AGENTS.md`](AGENTS.md).

## Architecture

```
crates/core          model, SQLite (versioned schema and migrations), scanning, command registry
crates/media         thumbnails, previews, codec probing, GStreamer player, network guard
crates/ui            egui application: theme, grid, sidebar, viewer, player, inspector
apps/jizzcraft       desktop application (eframe + wgpu)
apps/jizzcraft-cli   command-line front end on the same registry
xtask                synthetic fixtures, icon rasterization, README screenshots
```

- `core` has no dependency on the UI or on GStreamer.
- The UI thread never controls the pipeline. A dedicated control thread builds, drives and tears down GStreamer; the UI reads atomics and takes the newest ref-counted `gst::Sample` from a one-slot mailbox.
- Frame pacing follows the pipeline clock (`appsink` with `max-buffers=1 drop=true sync=true`).
- No async runtime: `std::thread` and channels. Slow work runs as background jobs with their own database connection.
- User data (ratings, hidden flags, history, markers) lives in the database only; the schema is versioned and append-only.
- **Network guard.** Pipelines only receive `file://` URIs. At start-up, GStreamer elements that can reach the network (HTTP, RTSP, UDP and other remote sources, HLS, DASH, Smooth Streaming and SDP demuxers) are removed from, or ranked out of, the application's own plugin registry, and every pipeline is watched for anything that slips through. A streaming playlist renamed to `.mp4` is therefore refused rather than fetched. The system-wide GStreamer registry is not modified. `crates/media/tests/network.rs` checks that such files make zero connections to a local listener.
- Video decoding is performed by the system GStreamer libraries and SQLite is compiled in through `rusqlite` (both C); all code in this repository is Rust.

### Performance

Measured with `jizzcraft --bench <file> --seconds 30 --bench-gst-play`; full results and method in [`docs/bench.md`](docs/bench.md). On the test machine, 1080p30, 1080p60 and 4K30 H.264 play with no dropped frames, but JizzCraft uses **3 to 6 times the CPU of `gst-play-1.0`** because colour conversion to RGBA is currently done on the CPU. Moving YUV→RGB conversion to a wgpu shader is on the [roadmap](ROADMAP.md).

## Installation

Requirements: Rust 1.95 or newer (edition 2024; the minimum required by egui 0.36, developed with 1.98; [rustup](https://rustup.rs) recommended), GStreamer with development headers, and a GPU driver. Vulkan is the tested backend; wgpu's OpenGL ES fallback is built in but untested.

### Arch Linux / CachyOS

```sh
sudo pacman -S --needed rust gstreamer gst-plugins-base gst-plugins-good \
  gst-plugins-bad gst-plugins-ugly gst-libav vulkan-icd-loader \
  xdg-desktop-portal xdg-desktop-portal-gtk
# plus the Vulkan driver for your GPU: vulkan-radeon, vulkan-intel or nvidia-utils
```

### Debian / Ubuntu

```sh
sudo apt install build-essential pkg-config \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
  libvulkan1 mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk
```

### Fedora

```sh
sudo dnf install gcc pkgconf-pkg-config \
  gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-base-tools \
  gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-plugins-ugly-free \
  vulkan-loader mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk
# H.264 and other patent-encumbered codecs require RPM Fusion:
# gstreamer1-libav, gstreamer1-plugins-bad-freeworld, gstreamer1-plugins-ugly
```

Only Arch Linux / CachyOS has been used interactively. The CI job builds and tests on Ubuntu 24.04 with the GStreamer development headers and the base and good plugins; the full Debian/Ubuntu runtime list above and the Fedora list have not been tested.

### Build and run

```sh
git clone https://github.com/kerstz/JizzCraft.git
cd JizzCraft
cargo run --release -p jizzcraft
```

### Synthetic test media

```sh
cargo xtask fixtures          # gradients, patterns and short test videos in ./fixtures
cargo run --release -p jizzcraft -- --import fixtures
```

### Development

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask icons             # rasterize the SVG icons with resvg
cargo xtask screenshots       # regenerate README screenshots through the application
```

Media tests need `gst-launch-1.0` and the GStreamer base and good plugins; when elements are missing they print `skipped: …` instead of failing.

## Supported formats

| Kind | Formats |
| --- | --- |
| Images | PNG, JPEG, WebP, GIF (first frame), BMP. AVIF files are indexed but not decoded: the `image` crate is built without AVIF support. |
| Videos | Extensions `mp4 mkv webm mov avi m4v wmv flv mpg mpeg ts ogv`. Playback depends on the installed GStreamer plugins. |

## Troubleshooting

**"missing GStreamer plugin: …"**: install the plugin sets listed above (good, bad, ugly, libav). The message names the missing element; check with `gst-inspect-1.0 <element>`, e.g. `gst-inspect-1.0 avdec_h264`.

**"refused: this file refers to network media…"**: the file is a streaming playlist (HLS, DASH, Smooth Streaming) or an SDP description rather than a video. JizzCraft does not follow the addresses inside it, by design; installing plugins will not change that.

**Black window or wgpu error at startup**: check your Vulkan driver (`vulkaninfo` should list your GPU). Vulkan is the only backend that has been tested.

**Wayland**: JizzCraft runs natively on Wayland. To force X11 (XWayland), run `WAYLAND_DISPLAY= jizzcraft`. The folder dialog uses the XDG desktop portal; `xdg-desktop-portal` and a backend for your desktop must be running.

**High CPU or stutter on 4K**: expected in the current version; see [`docs/bench.md`](docs/bench.md).

**File names in logs**: paths and file names are redacted by default. `JIZZCRAFT_DEBUG=1` logs full paths and the selected decoder. Redact such logs before sharing them.

## Project status

> [!IMPORTANT]
> JizzCraft is **early alpha**.
>
> **Covered by automated tests**: library scanning, search, favorites and persistence; schema migration; ratings, hiding, inbox, history settings and markers; filters, saved views, the folder tree and the stable shuffle; fingerprint relinking and merging; backup and metadata export/import; checksum-based corruption detection; a test that hashes a media tree before and after every command to prove nothing is written to it; image and video thumbnails, scrub previews and integrity checks; the player state machine on a real GStreamer pipeline (play, pause, seek, coalesced scrubbing, end of stream and replay, speed, frame step, A-B loop, error reporting); the network guard; keymap invariants. The CLI examples above have been run end to end. Rendering of the library, viewer, player, triage and discreet mode has been checked through generated screenshots. Playback was benchmarked on one machine (H.264, VA-API, AMD GPU, Wayland).
>
> **Not yet verified interactively**: mouse-wheel zoom, drag-panning, timeline dragging, keyboard shortcuts (including the left-hand preset), fullscreen, boss-key minimization across compositors, the native folder dialog, the sidebar, triage, the player-bar chips, autoplay-next, the slideshow.
>
> **Known limitations**: CPU usage on high-resolution video is several times that of `gst-play-1.0`; no subtitles or audio-track selection; no file watching (rescan is manual); folders cannot yet be removed from the library in the UI; triage, random pick, slideshow, frame step and markers have no button or shortcut yet; fingerprints are computed on request only; AVIF is not decoded; GStreamer parses every imported video for its thumbnail, without a sandbox; tested on a single Linux machine; no Flatpak or AppImage packages.

Planned work is listed in [`ROADMAP.md`](ROADMAP.md).

## Privacy

- No accounts, telemetry, analytics, cloud sync, advertising or network connections. The player accepts local file paths only, and GStreamer's network-capable elements are disabled inside the application (see [Architecture](#architecture)). Files stored on network mounts (NFS, SMB, sshfs) are read through your operating system like any other file.
- No scraping, downloading or external feeds.
- Media files are never modified, moved or deleted. Thumbnails and previews are written only to the XDG cache directory (or the portable home).
- Ratings, hidden flags, history and markers live only in the library database: no sidecar files. Exports leave out file names and history unless asked.
- History is aggregate (count, last played, resume point) and can be paused, hidden or cleared; clearing also truncates SQLite's write-ahead log (best effort). Fingerprints read at most 128 KiB per file; integrity checks and checksums run only on request and are read-only.
- No shell command is ever built from a file name; paths are passed to APIs as arguments.
- Full paths and file names are not logged unless `JIZZCRAFT_DEBUG=1` is set.
- The age gate, discreet mode and share-safe mode do not protect data at rest: the library database and thumbnail cache are not encrypted and use your normal file permissions. An encrypted vault is on the roadmap.
- `--bench-gst-play` starts the external `gst-play-1.0` program, which the network guard does not cover.

## Contributing

Contributions are welcome. Please read [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`AGENTS.md`](AGENTS.md). In short: no network code, no real adult content in the repository or in issues (use `cargo xtask fixtures`), no site-specific code, and new behaviour is added to the command registry before it is exposed in the UI.

## Security

Please report vulnerabilities privately as described in [`SECURITY.md`](SECURITY.md), not in public issues.

## Inspiration

The idea comes from a joke thread on r/linuxmemes, ["Not another one"](https://www.reddit.com/r/linuxmemes/comments/1x454bj/not_another_one/), about yet another vibe-coded Rust app ending in "Craft". The thread's remarks on accessibility, one-handed use and repetitive filtering shaped the keyboard-first design and the left-hand key preset. No content from the thread is reproduced here, and the project is not affiliated with its author or with the subreddit.

## Parody disclaimer

The name "JizzCraft", the structure of this README and the look of the interface parody the "Craft" family of apps by Crafting Apps (ArtCraft and its siblings). JizzCraft is an independent project and is not affiliated with, endorsed by or connected to the ArtCraft team or Crafting Apps. It contains no code, assets, logos or wordmarks from those projects. Application ID: `org.jizzcraft.JizzCraft`.

## License and credits

Licensed under either the [MIT License](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option. Unless you explicitly state otherwise, any contribution you submit is dual-licensed as above, without additional terms.

Bundled fonts (Archivo, Inter, JetBrains Mono; SIL Open Font License 1.1), the licences of third-party libraries and other credits are listed in [`ATTRIBUTION.md`](ATTRIBUTION.md). The application icon was created for this project.
