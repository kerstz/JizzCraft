<p align="center">
  <img src="assets/icons/hicolor/128x128/apps/org.jizzcraft.JizzCraft.png" width="96" alt="JizzCraft peach icon">
</p>

# JizzCraft

**The porn viewer you already know how to use.**

Your library, in a native app written entirely in Rust. Open source, offline, and yours.

![100% Rust](https://img.shields.io/badge/100%25-Rust-b7410e?style=flat-square)
![Linux](https://img.shields.io/badge/platform-Linux-2f7bf5?style=flat-square)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-3a3a3a?style=flat-square)
![Status](https://img.shields.io/badge/status-early%20alpha-d69e2e?style=flat-square)

> "100% Rust" is about the code in this repository. Video decoding is delegated to the system's GStreamer, which is C.

> [!NOTE]
> JizzCraft is a **parody project**. It is not affiliated with, endorsed by or connected to the ArtCraft team or the Crafting Apps. See [Parody disclaimer](#parody-disclaimer).

![JizzCraft library grid](docs/screenshots/library.png)

## Why JizzCraft

- 🎛️ **Familiar by design.** If you know a file browser, you already know JizzCraft: a top bar, a grid, an inspector. Open a folder, see your files.
- ⌨️ **Keyboard first.** Arrow keys move through the grid, `Enter` opens, `/` searches, `1`–`4` switch tabs. A left-hand preset keeps everything on `WASD` for one-handed use.
- ⚡ **Native and fast.** egui + wgpu, SQLite, a virtualized grid that never loads the whole library, GStreamer playback paced by the pipeline clock.
- 🔒 **Private by default.** No account, no telemetry, no network access, ever. A boss key, a discreet mode and a screen-share-safe mode are built in.
- 🤖 **Agent-ready.** One registry of named commands drives the menus, the shortcuts and a JSON CLI.

## Features

### A library that stays out of your way

Import a folder (native dialog), and JizzCraft indexes images and videos recursively. It never follows symlinks, never touches your files, and keeps a file that went missing flagged instead of silently dropping it. Search by filename, filter All / Images / Videos / Favorites, sort by name, date added, size or duration, and rescan on demand. No ads, no account, nothing to log into.

![Empty state](docs/screenshots/empty.png)

### Thumbnails, cached outside your folders

Images are downscaled to about 320 px in a background pool. Videos go through a dedicated GStreamer pipeline (`uridecodebin → videoconvert → videoscale → appsink`), paused and seeked to about 10 %. Everything lands in the XDG cache dir, keyed by a hash of path + size + mtime.

### Image viewer

Fit, 100 %, wheel zoom around the cursor, pan, `←`/`→` for previous and next, `Esc` to go back. EXIF orientation is honoured.

![Image viewer](docs/screenshots/viewer.png)

### Video player

`playbin` with an `appsink` video sink, one persistent wgpu texture updated in place, coalesced scrub seeks (key-unit while dragging, accurate on release), pixel-aspect-ratio and orientation handling, last frame held at the end with a replay button, readable errors that name the missing GStreamer plugin.

![Video player](docs/screenshots/player.png)

### Discreet by design

`Ctrl+Shift+H` is the boss key: pause, mute, switch to discreet mode, minimize. Discreet mode retitles the window "Viewer", swaps the icon, hides the brand and file names, and blurs thumbnails. "Share-safe" blurs every thumbnail until hovered.

![Discreet mode](docs/screenshots/discreet.png)

## Everything in the box

| Area | What you get |
| --- | --- |
| Library | recursive indexing, symlinks skipped, hidden dirs skipped, Unicode names, duplicate imports are no-ops, missing files flagged, permission errors counted not fatal |
| Grid | virtualized (only visible rows are rendered, only a window of rows is loaded from SQLite), type badge, duration or resolution, favorite star |
| Search | case-insensitive Unicode filename substring |
| Images | PNG, JPEG, WebP, GIF, BMP |
| Videos | whatever your GStreamer can decode (MP4/H.264, WebM/VP8/VP9, MKV, …) |
| Inspector | name, path, type, size, resolution, duration, codec, added date |
| Shortcuts | `Ctrl+O` import · `Ctrl+R` rescan · `Space` play/pause · `←`/`→` seek ±5 s or prev/next image · `↑`/`↓` volume · `M` mute · `F` fullscreen · `S` favorite · `Esc` back |
| Privacy | 18+ age gate, boss key, discreet mode, share-safe mode |

## Built for agents

Menus, shortcuts, the CLI and `--cmd` all call the same command registry.

```sh
jizzcraft-cli scan ~/Videos --json
jizzcraft-cli query --type video --name holiday --json
jizzcraft-cli fav 42 --json
jizzcraft-cli query --fav --json      # same favorites the app shows
jizzcraft-cli stats --json
jizzcraft-cli commands                # list registered commands

# drive the desktop app
jizzcraft --import ~/Videos --cmd 'view.filter={"tab":"Videos"}' --cmd 'media.open={"index":0}'
```

See [`AGENTS.md`](AGENTS.md) for the conventions agents (and humans) should follow.

## Under the hood

```
crates/core    model, SQLite (versioned schema + migrations), scan, command registry
crates/media   thumbnails, codec probing, GStreamer player
crates/ui      egui app: Craft theme, grid, viewer, player, inspector
apps/jizzcraft       desktop app (eframe + wgpu)
apps/jizzcraft-cli   CLI on the same registry
xtask          fixtures, icons, README screenshots
```

- **Engine first.** `core` has no UI and no GStreamer dependency.
- **The UI thread never touches the pipeline.** A control thread builds, drives and tears down GStreamer; the UI reads atomics and takes the newest ref-counted `gst::Sample` from a one-slot mailbox.
- **Pacing by the clock.** `appsink` runs with `max-buffers=1 drop=true sync=true`.
- **No async runtime.** `std::thread` and channels.
- **Performance, measured.** `jizzcraft --bench <file> --seconds 30 --bench-gst-play`. Results in [`docs/bench.md`](docs/bench.md): no dropped frames on 1080p30, 1080p60 and 4K30 H.264, **but 3 to 6 times the CPU of `gst-play-1.0`** because the MVP does RGBA conversion on the CPU. A YUV→RGB shader is on the [roadmap](ROADMAP.md).

## Get started

### Dependencies (Arch / CachyOS)

```sh
sudo pacman -S --needed rust gstreamer gst-plugins-base gst-plugins-good \
  gst-plugins-bad gst-plugins-ugly gst-libav vulkan-icd-loader \
  xdg-desktop-portal xdg-desktop-portal-gtk
# plus a Vulkan driver for your GPU, e.g. vulkan-radeon, vulkan-intel or nvidia-utils
```

### Build and run

```sh
git clone <this repository> && cd JizzCraft
cargo run --release -p jizzcraft
```

### Try it on safe, synthetic media

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
cargo xtask screenshots       # render README images through the app itself
```

## Supported formats

| Kind | Formats |
| --- | --- |
| Images | PNG, JPEG, WebP, GIF (first frame), BMP. AVIF is indexed but only displayed if the `image` crate is built with AVIF support (it is not, today). |
| Videos | extensions `mp4 mkv webm mov avi m4v wmv flv mpg mpeg ts ogv`; actual playback depends on the GStreamer plugins you have installed. |

## Troubleshooting

**"missing GStreamer plugin: …"** — install the plugin sets listed above (`gst-plugins-good/bad/ugly`, `gst-libav`). The error names what is missing. Check with `gst-inspect-1.0 avdec_h264`.

**Black window or wgpu error at startup** — you need a working Vulkan driver (`vulkaninfo` should list your GPU). Install the right `vulkan-*` package for your hardware.

**Wayland** — JizzCraft runs natively on Wayland. If your compositor misbehaves, try X11 with `WINIT_UNIX_BACKEND=x11 jizzcraft`. The folder chooser uses the XDG desktop portal: make sure `xdg-desktop-portal` and a backend for your desktop are running.

**Playback stutters on 4K** — see the [benchmark notes](docs/bench.md); software colour conversion is the bottleneck in this MVP.

**Need to see file names in logs** — they are redacted by default. Run with `JIZZCRAFT_DEBUG=1` to log full paths and the chosen decoder.

## Status

> [!IMPORTANT]
> JizzCraft is **early alpha**. Here is what is true today.
>
> **Verified**: library scan/search/favorites/persistence (automated tests and CLI round trips), thumbnails for images and videos (tests + screenshots), image viewer and player rendering (screenshots), player state machine — play, pause, seek, coalesced scrubbing, EOS and replay, error reporting (integration tests on a real GStreamer pipeline), playback benchmarks on one machine (H.264, VA-API, AMD GPU, Wayland).
>
> **Only checked through commands and screenshots, not by hand**: mouse wheel zoom, drag-panning, timeline dragging, keyboard shortcuts, fullscreen, boss-key minimize behaviour under different compositors, the native folder dialog.
>
> **Known gaps**: CPU use on high-resolution video is several times `gst-play-1.0` (CPU colour conversion); no subtitles, no audio-track selection; no file watching (rescan is manual); no way yet to remove a folder from the library in the UI; AVIF images are not decoded; tested on one Linux machine only.

## Privacy guarantees

- No accounts, telemetry, analytics, cloud sync, ads or network requests. The player only accepts local file paths.
- No scraping, no downloaders, no external feeds.
- Your media is never modified, moved or deleted. Thumbnails are written to the XDG cache directory only.
- No shell commands are ever built from file names. Paths are passed as arguments to APIs.
- Full paths and file names are not logged unless `JIZZCRAFT_DEBUG=1`.
- The 18+ age gate, discreet mode and share-safe mode are conveniences, not security boundaries: the library database and cache are not encrypted (an encrypted vault is on the roadmap).

## Parody disclaimer

JizzCraft started as a joke imitating the "Crafting Apps" family (PhotoCraft, FilmCraft, LightCraft…). It is a parody and is **not affiliated with, endorsed by or connected to the ArtCraft team**. It reuses README conventions only: no code, assets, logos or wordmarks from those projects. App ID: `org.jizzcraft.JizzCraft`.

## License and credits

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

Fonts (Archivo, Inter, JetBrains Mono, all OFL) and other credits are listed in [`ATTRIBUTION.md`](ATTRIBUTION.md). The peach icon was drawn for this project.
