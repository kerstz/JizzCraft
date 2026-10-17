# Getting started

For people who want to install JizzCraft and use it, without knowing Rust or
much about Linux. Two minutes to read, a few more to install. If you would
rather judge how it is built, go to [internals.md](internals.md) instead.

No account, no cloud, no telemetry: the only thing that leaves your machine is
your dignity, and only if you leave the window in fullscreen.

- [What it is, in one paragraph](#what-it-is)
- [Install (Linux)](#install)
- [First launch, step by step](#first-launch)
- [Everyday keys](#everyday-keys)
- [FAQ](#faq)
- [Words used in the app](#words-used-in-the-app)

> **18+ only.** JizzCraft is for adults managing media they own or are entitled
> to keep. It ships no media, no sites and no catalogs.

## What it is

JizzCraft is a desktop app that shows the pictures and videos already on your
computer as a searchable library, plays them, and lets you rate, favorite,
hide and sort them. It only **reads** your files: it never moves, renames or
changes them. Everything it remembers (ratings, what you watched, where you
stopped) is kept in one file on your own disk. There is no account, no
advertising, and the default version contains no internet code at all.

It is **alpha** software (version 0.3.0): it works and is tested, but it will
still have rough edges. Linux is the only system it has really been used on.

## Install

Open a terminal (on most Linux desktops: search for "Terminal" in the
application menu), paste this line and press Enter:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/kerstz/JizzCraft/releases/latest/download/install.sh | sh
```

What you should see, in order:

1. **`Edition [1/2]:`** Press Enter to keep **Offline** (the default: your
   own files only, no internet code). The other choice, *Online*, is
   explained in [the FAQ](#offline-or-online).
2. **`Downloading jizzcraft-0.3.0-…-linux.tar.xz`.** Its checksum is verified;
   if it does not match, the script stops and installs nothing.
3. **`GStreamer packages needed:`** followed by the exact command (GStreamer is
   the part of Linux that decodes video), then **`Run it now? [y/N]`**. Type
   `y` and Enter to let it install them; your distribution will ask for your
   password (`sudo`). Plain Enter means no: the command is skipped and you can
   run it yourself later. The script never runs `sudo` without this question.
4. **`Installed JizzCraft 0.3.0 to /home/you/.local`.** Possibly followed by a
   line saying that `~/.local/bin` is not on your PATH, with the exact command
   to fix it for your shell. Copy and run it once.
5. **A health check** (`jizzcraft-cli doctor`): one line per piece of video
   support, and the command that installs what is missing on your
   distribution.
6. **`Start it from your application menu, or run: jizzcraft`.**

Everything is installed for your user only, under `~/.local`. To update later,
run the same line again.

Other systems: a `.deb` package for Debian, Ubuntu and Mint is on the
[release page](https://github.com/kerstz/JizzCraft/releases) (install it with
`sudo apt install ./jizzcraft_*.deb`). Windows is experimental and has never
been run on a real machine ([windows.md](windows.md)); macOS has to be built
from source. The full table is in the [README](../README.md#install).

## First launch

1. **Age check.** The first window asks you to confirm you are 18 or older.
   Click **I am 18 or older** (or **Exit**). It is asked once.
2. **A short tour.** Every step can be skipped (`Esc` skips them all, `Enter`
   goes to the next one), and you can reopen it later from the menu:
   - *Welcome*: what the app does and does not do.
   - *Your folders*: click **Add a folder** and pick the folder that holds
     your pictures or videos. You can add more later.
   - *Privacy*: switches for discreet mode, share-safe mode, playful messages
     and the "Viewer" disguise (all explained [below](#words-used-in-the-app)).
     All of them can be changed later.
   - *Can this machine play things?*: the same health check as the installer,
     inside the app.
3. **Your library fills up.** The app lists the files and makes small preview
   pictures (thumbnails) in the background. Nothing is copied next to your
   files: thumbnails go to a separate cache folder.
4. **Look around.** Use the arrow keys to move, `Enter` to open, `Esc` to go
   back. Type `/` to search by name, tag or category. The tabs at the top
   (keys `1` to `4`) switch between everything, images, videos and favorites.
5. **Rate and sort.** Press `F` to mark a favorite, `Alt+1` to `Alt+5` to give
   one to five stars (`Alt+0` clears), or click the stars in the panel on the
   right. New imports land in the *Inbox*; press `V` to go through them one
   at a time (keep, hide, favorite, undo). Hiding only hides: nothing is
   deleted.
6. **Try the boss key.** Press `Ctrl+Shift+H`: playback pauses, the sound is
   muted, discreet mode turns on and the window is minimized. When you come
   back, discreet mode is still on; click the **Discreet** button at the top
   of the window to turn it off.

Want to try it before pointing it at your own folders? People who build from
source can generate harmless test pictures and videos with
`cargo xtask fixtures` (see [internals.md](internals.md#build-test-and-run-ci-locally)).

## Everyday keys

| Key | What it does |
| --- | --- |
| Arrows, `Enter` | Move, open |
| `Esc` | Back (or clear the search) |
| `/` | Search |
| `F` | Favorite |
| `Alt+1` … `Alt+5`, `Alt+0` | Rate one to five stars, clear the rating |
| `V` | Go through new imports one by one |
| `R` | Open a random item |
| `Space` | Play / pause (in the player) |
| `Ctrl+O` | Add a folder |
| `Ctrl+R` | Look for new or removed files in your folders |
| `Ctrl+,` | Settings |
| `Ctrl+Shift+H` | Boss key |
| `Ctrl+Shift+M` | Panic switch: jump full screen to your chosen video |
| `Ctrl+Shift+L` | Lock the encrypted library (only if you turned the vault on) |
| `?` | The full list of keys |

Prefer one hand? Press `?` and click *Left-hand keys*: everything moves
around `WASD` (the [README](../README.md#one-hand-is-enough) has the table).

## FAQ

### Where is my data?

On your disk, in three folders (Linux):

| What | Where |
| --- | --- |
| The library (ratings, history, tags, markers…) | `~/.local/share/jizzcraft/library.db` |
| Settings | `~/.config/jizzcraft/config.json` |
| Thumbnails and previews | `~/.cache/jizzcraft/` |

Crash notes, if any, go to `~/.local/state/jizzcraft/`; they never contain a
file name unless you turned on `JIZZCRAFT_DEBUG=1`. On Windows the folders are
listed in [windows.md](windows.md#where-things-live). To keep everything in
one folder you choose (for example on a USB stick), start the app with
`--home /path/to/folder` or `--portable`.

These files are **not encrypted** unless you turn on the encrypted vault
(Settings > Privacy > *Encrypt library*; alpha, and **there is no way to
recover a forgotten passphrase**). Read [vault.md](vault.md) first.

### Does it change my files?

No. It reads them to list them, make thumbnails and play them. It never
moves, renames or deletes a file, never writes tags into them and never puts
extra files next to them. Your ratings and history live only in the library
file above. An automated test checks this on every change to the code
(details in [internals.md](internals.md#guarantees-checked-by-tests)).

### Does it connect to the internet?

The **Offline** edition (the default) contains no internet code at all; a test
fails if any gets in. The only connections involved are the ones *you* make:
the install script downloads the app from GitHub, and your package manager
downloads GStreamer. The app never checks for updates, has no account and
sends no statistics.

If your media sit on a network share (NFS, SMB, sshfs), the app reads them
through your system like any other file.

### Offline or Online?

- **Offline**: a private library and player for files already on your
  computer. Start here.
- **Online**: the same app plus optional tools to play or download from
  sources **you** set up yourself (indexers, torrents, streaming addresses).
  Nothing is preset and nothing connects before you act, but those tools do
  reveal your IP address to the services you use. See [editions.md](editions.md)
  and [streaming.md](streaming.md).

You can switch at any time with `install.sh --edition offline|online`; your
library is kept.

### How do I uninstall it?

Run the installer again with `--uninstall`:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/kerstz/JizzCraft/releases/latest/download/install.sh | sh -s -- --uninstall
```

It removes exactly the files it installed and nothing else. Your library,
settings and thumbnails are kept, in case you come back; the script prints
their three folders. To erase those too, delete them yourself:

```sh
rm -r ~/.local/share/jizzcraft ~/.config/jizzcraft ~/.cache/jizzcraft ~/.local/state/jizzcraft
```

Your media files are not in any of these folders and are never touched.
If you installed the `.deb`, use `sudo apt remove jizzcraft` instead.

### A video does not play. What now?

1. Run `jizzcraft-cli doctor` in a terminal. It lists which video formats your
   system can decode and prints the one command that installs what is
   missing. Run that command, then restart JizzCraft.
2. An error that names a **missing plugin** means the same thing: a GStreamer
   package is missing (usually `gst-libav` or `gst-plugins-bad/ugly`;
   [troubleshooting.md](troubleshooting.md) has the package names per
   distribution).
3. "**refused: this file refers to network media**" means the file is not a
   video but a streaming playlist pointing to the internet. The Offline
   edition refuses those on purpose.
4. A **black window** or a graphics error at start-up: your graphics driver
   (Vulkan) is missing. Try `jizzcraft --renderer gl`, and see
   [troubleshooting.md](troubleshooting.md#the-window-does-not-open).
5. Still stuck? Open a [bug report](https://github.com/kerstz/JizzCraft/issues/new/choose)
   and paste the output of `jizzcraft-cli doctor --report`: it contains no
   file names, paths or addresses. Never attach your own media or
   screenshots of your library.

### Is it safe to show my screen?

Use discreet mode (neutral window title "Viewer", hidden names, blurred
thumbnails) or share-safe mode (everything blurred until you hover it). They
reduce what a glance can see; they are conveniences, not a lock. For a real
lock, the encrypted vault is the tool, with the limits explained in
[vault.md](vault.md#threat-model).

## Words used in the app

| Word | Meaning |
| --- | --- |
| **Library** | The list of everything JizzCraft knows about, built from the folders you added. |
| **Import / add a folder** | Tell JizzCraft to list a folder. The files stay where they are. |
| **Rescan** | Look again for new, moved or removed files (it does not happen on its own). |
| **Inbox** | Items you have not reviewed yet. |
| **Triage** | Going through the Inbox one item at a time: keep, hide, favorite. |
| **Hide** | Remove from the normal views. Reversible; the file is untouched. |
| **Missing** | A file the library knew about that is no longer on disk. It is flagged, not forgotten. |
| **Marker** | A bookmark at a moment in a video (`K` adds one). |
| **Discreet mode** | Neutral window title, hidden names, blurred thumbnails. |
| **Share-safe mode** | Everything blurred until you point at it. |
| **Boss key** | `Ctrl+Shift+H`: pause, mute, discreet mode, minimize. |
| **Panic switch** | `Ctrl+Shift+M`: opens the video you picked (Library menu, Privacy, *Panic video*) full screen with sound, turns discreet mode on and leaves no history or resume point. With no video picked, or if the file is gone, it does the boss key instead. |
| **"Viewer" disguise** | Start the app under a neutral name and icon. |
| **Edition** | Offline (no internet code) or Online (optional tools you configure). |
| **GStreamer** | The part of your system that decodes video. JizzCraft uses it to play files. |
| **Codec / plugin** | The piece of GStreamer that understands one video format (H.264, HEVC, AV1…). |
| **Doctor** | `jizzcraft-cli doctor`: a health check that says what is missing and how to fix it. |
| **Vault** | Optional encryption of the library and thumbnails with a passphrase. Alpha, no recovery. |
| **Terminal** | The window where you type commands. In most Linux terminals, paste with `Ctrl+Shift+V`. |

More questions: [troubleshooting.md](troubleshooting.md), the
[README](../README.md), or the [docs index](README.md).
