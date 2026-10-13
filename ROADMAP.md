# Roadmap

Today: local library, thumbnails, image viewer, GStreamer player, favorites,
ratings, hide, resume and history, markers, A-B loop, speed and frame step,
scrub previews, views and folder tree, triage, fingerprint relink, integrity
check, backup and metadata export, portable mode, age gate, boss key, discreet
mode, CLI.

## Next milestones (explicitly out of scope until the MVP is solid)

- **Player**: NV12/I420 caps with YUV→RGB in a wgpu shader (skips the CPU
  `videoconvert`); subtitle and audio-track pickers; seamless segment loops.
- **Library**: tags, perceptual-hash duplicate detection, file watching instead
  of manual rescan, per-folder removal from the library UI, a relink picker for
  moved library roots, a restore-from-backup flow.
- **Privacy**: encrypted vault for the library database and thumbnail cache.
- **Distribution**: Flatpak, AppImage, installers; Windows and macOS packaging.
- **Agents**: MCP server on top of the command registry; loopback control channel.
- **Assistance**: local, offline AI tagging.
- **Extensibility**: plugins.
- **Website**: project site and docs.

Not planned, ever: accounts, cloud sync, telemetry, downloaders, scrapers,
remote URLs. This repository stays offline: any networked companion would be a
separate project with its own binary, so the offline guarantee never weakens.
