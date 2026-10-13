# Roadmap

MVP (this repository today): local library, thumbnails, image viewer, GStreamer
player, favorites, age gate, boss key, discreet mode, CLI.

## Next milestones (explicitly out of scope until the MVP is solid)

- **Player**: NV12/I420 caps with YUV→RGB in a wgpu shader (skips the CPU
  `videoconvert`); subtitle and audio-track pickers; frame-accurate stepping.
- **Library**: tags, collections, duplicate detection, file watching instead of
  manual rescan, per-folder removal from the library UI.
- **Privacy**: encrypted vault for the library database and thumbnail cache.
- **Distribution**: Flatpak, AppImage, installers; Windows and macOS packaging.
- **Agents**: MCP server on top of the command registry; loopback control channel.
- **Assistance**: local, offline AI tagging.
- **Extensibility**: plugins.
- **Website**: project site and docs.

Not planned, ever: accounts, cloud sync, telemetry, downloaders, scrapers,
remote URLs.
