# Troubleshooting

Run `jizzcraft --doctor` (or `jizzcraft-cli doctor`, add `--json` for scripts).
Exit status: 0 fine, 1 something required is missing, 2 only codecs or
streaming parts are missing. Notes marked WARN never change the status.
`jizzcraft-cli doctor --report` prints a redacted report you can paste into an
issue: versions, OS, distro family, GStreamer plugin summary, schema version,
item and folder counts and a few config flags. It never holds a path, name,
URL or key.

## Checks

| Check | What it means | Fix |
| --- | --- | --- |
| required: playbin, appsink, videoconvert | GStreamer base is missing | Install `gst-plugins-base` (`gstreamer1.0-plugins-base`, `gstreamer1-plugins-base`). |
| required: MP4 / Matroska demuxer | `gst-plugins-good` is missing | Install `gst-plugins-good` (`gstreamer1.0-plugins-good`). |
| codec: H.264, HEVC, AAC | No libav decoder | Install `gst-libav` (`gstreamer1.0-libav`, `gstreamer1-plugin-libav`). Flatpak: `org.freedesktop.Platform.codecs-extra`. |
| codec: AV1, VP9, Opus | Decoder plugin missing | AV1 is in `gst-plugins-bad`; VP9 in good; Opus in base. |
| streaming: MPEG-TS demuxer (`tsdemux`) | HLS streams cannot play | Install `gst-plugins-bad` (`gstreamer1.0-plugins-bad`). |
| streaming: H.264 parser (`h264parse`) | HLS/DASH video cannot be parsed | Same package: `gst-plugins-bad`. |
| streaming: AAC parser (`aacparse`), fMP4 demuxer (`qtdemux`) | HLS audio, DASH segments | Install `gst-plugins-good`. |
| hardware: VA-API | Hardware decoding unavailable (optional, shown as `--`) | Install `gst-plugin-va` / `gstreamer1.0-plugins-bad` and your GPU's VA driver. |
| folders: data, config, cache | The folder is not writable | Check permissions and free space, or choose another with `--home DIR` or `JIZZCRAFT_HOME`. |
| WARN filesystem | The data or cache folder is on NFS, CIFS/SMB, 9p or sshfs. SQLite WAL is unsafe there | Keep the library on a local disk (`--home /local/dir`). Media folders on a network share are fine. |
| WARN disk | Less than 1 GiB free where the library or cache lives | Free space, or move the cache with `XDG_CACHE_HOME`. |
| WARN clock | The clock is before this build's date, or differs from the filesystem's by more than a day | Set the time (NTP). A wrong clock breaks file dates and HTTPS in the network build. |
| renderer | Lists the GPU adapters wgpu can use (desktop app only) | None found: install Vulkan drivers, or run `jizzcraft --renderer gl`. |

## The window does not open

The app shows a dialog and exits with status 1 when no graphics adapter can be
created. Try `--renderer vulkan` or `--renderer gl` (or `JIZZCRAFT_RENDERER`),
install your GPU's Vulkan driver (`mesa-vulkan-drivers`, `vulkan-icd-loader`),
and run `jizzcraft --doctor` to see the adapters.

## After a crash

A redacted `crash-<unixts>.txt` (thread, source line, version; no message
unless `JIZZCRAFT_DEBUG=1`) is written to the state folder. The next launch
prints one line on stderr and marks it seen. Core dumps are disabled on Linux.
