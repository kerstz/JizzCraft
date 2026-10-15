# JizzCraft on Windows

Status, honestly: the code compiles for `x86_64-pc-windows-gnu` from Linux (zig
as C toolchain) and the CI `windows` job builds, lints, tests and takes a
screenshot with the software renderer. Nobody has run the result on a real
Windows machine yet. Treat the first release as experimental.

## Install

- **Installer** (`JizzCraft-<version>-windows-x86_64-setup.exe`): per user, no
  administrator rights, installs under `%LOCALAPPDATA%\Programs\JizzCraft`.
  Optional neutral *Viewer* shortcut. Uninstall removes the program only.
- **Portable zip**: unpack anywhere. Add an empty `jizzcraft-data` folder next
  to the exe (or start with `--portable`) to keep everything beside it.

Both carry their own GStreamer. They do not look at a GStreamer you installed
yourself, and set their environment before anything starts so the two cannot
be mixed.

## Where things live

| What | Folder |
| --- | --- |
| Library database | `%LOCALAPPDATA%\jizzcraft\JizzCraft\data` (an older library in `%APPDATA%` is still found) |
| Settings, `fetch.json` | `%APPDATA%\jizzcraft\JizzCraft\config` |
| Thumbnails and the GStreamer registry | `%LOCALAPPDATA%\jizzcraft\JizzCraft\cache` |

`fetch.json` holds keys: it inherits the per-user ACL of `%APPDATA%`, which is
the Windows equivalent of mode 0600.

## Things worth knowing

- **Long paths**: the exe is long-path aware; Windows also needs
  `LongPathsEnabled=1` (Group Policy or registry).
- **SmartScreen**: builds are unsigned. "More info" then "Run anyway".
- **Firewall**: only the network build listens, and only if you turn incoming
  peers on. The prompt appears when it first binds.
- **Cloud placeholders** (OneDrive "online only"): indexed by name and size and
  never read, so scanning does not download them.
- **Hidden and system entries** (`$RECYCLE.BIN`, `System Volume Information`)
  are skipped, like dot-folders.
- **Disguise**: window title, taskbar ID and icon are neutral. The file name
  `jizzcraft.exe`, Recent files from the file dialog and the Start menu entry
  (unless you chose the Viewer shortcut) still say what it is.
- **Boss key**: leaving fullscreen and minimizing are separated by one frame on
  purpose; Windows can otherwise restore the fullscreen window.

## Checking the port from Linux

```sh
rustup target add x86_64-pc-windows-gnu
# zig (any recent) as the C toolchain; nasm and perl for the vendored OpenSSL
# (SQLCipher), which every build now compiles
packaging/windows/cross-check.sh -p jizzcraft-core -p jizzcraft-fetch -p jizzcraft-media
```

This proves compilation, not behaviour. The CI `windows` job is the authority.

## Building the installer

`packaging\windows\build.ps1` builds the portable tree and, when Inno Setup's
`ISCC.exe` is found, the installer. The GStreamer MSVC runtime and devel
packages and `pkgconfiglite` are the prerequisites (the CI job shows how).

**NASM and Perl are required by default**, not only for the network build: the
library is encrypted with SQLCipher, and every build compiles its vendored
OpenSSL, which needs NASM (assembly) and Perl (its configure step; Strawberry
Perl, or the one bundled with Git for Windows, works). Install NASM
(`choco install nasm`) and put it on `PATH` before `cargo build`; the CI job
uses `ilammy/setup-nasm`.
