# Roadmap

Today: local library, thumbnails, image viewer, GStreamer player, favorites,
ratings, hide, resume and history, markers, A-B loop, speed and frame step,
scrub previews, views and folder tree, triage, fingerprint relink, integrity
check, backup and metadata export, portable mode, age gate, boss key, discreet
mode, CLI, network guard for GStreamer.

## Next milestones (explicitly out of scope until the MVP is solid)

- **Reachability**: buttons and shortcuts for what is only a command today
  (triage, random pick, slideshow, frame step, adding and jumping between
  markers); automatic fingerprinting of items that carry user data.
- **Player**: NV12/I420 caps with YUV→RGB in a wgpu shader (skips the CPU
  `videoconvert`); subtitle and audio-track pickers; seamless segment loops.
- **Library**: tags, perceptual-hash duplicate detection, file watching instead
  of manual rescan, per-folder removal from the library UI, a relink picker for
  moved library roots, a restore-from-backup flow.
- **Privacy**: encrypted vault for the library database and thumbnail cache.
- **Hardening**: thumbnail, preview and probe decoding in a sandboxed worker
  process (GStreamer parses every imported video today).
- **Distribution**: Flatpak, AppImage, installers; Windows and macOS packaging.
- **Agents**: MCP server (stdio) on top of the command registry; a local control
  channel over a Unix socket, never TCP.
- **Assistance**: local, offline tagging (classification only; nothing is
  generated or altered).
- **Extensibility**: plugins.
- **Website**: project site and docs.

Not planned, ever: accounts, cloud sync, telemetry, downloaders, scrapers,
remote URLs, media generation. This repository stays offline: any networked
companion would be a separate project with its own binary, so the offline
guarantee never weakens.
