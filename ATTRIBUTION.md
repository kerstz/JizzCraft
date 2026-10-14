# Attribution

## Fonts (bundled in `assets/fonts/`)

| Font | Use | License | Source |
| --- | --- | --- | --- |
| Archivo (Medium) | wordmark, headings, hero headline | SIL Open Font License 1.1 (`Archivo-OFL.txt`) | https://github.com/Omnibus-Type/Archivo |
| Inter (Regular, Medium) | body text, filenames | SIL Open Font License 1.1 (`Inter-OFL.txt`) | https://github.com/rsms/inter |
| JetBrains Mono (Regular, Bold) | labels, tabs, buttons, numbers | SIL Open Font License 1.1 (`JetBrainsMono-OFL.txt`) | https://github.com/JetBrains/JetBrainsMono |

Fonts are used unmodified. egui additionally ships its own default fonts
(Ubuntu-Light, Hack, NotoEmoji, emoji-icon-font) as glyph fallbacks.

## Icons

The peach icon (`assets/icons/jizzcraft.svg`) and the neutral "Viewer" icon
(`assets/icons/viewer.svg`) were drawn for this project and are released under
the project license.

## Inspiration

The project was inspired by the r/linuxmemes thread "Not another one"
(https://www.reddit.com/r/linuxmemes/comments/1x454bj/not_another_one/).
No text, code or media from the thread is included.

## Conventions

README structure and the "engine first, agent-ready" layout follow the
conventions of PhotoCraft by storytold (https://github.com/storytold/photocraft).
The interface imitates the visual style of that family of apps (flat surfaces,
hairlines, typography, numbered labels) and is implemented from scratch: no
code, assets, logos or wordmarks from ArtCraft / Crafting Apps are used.
JizzCraft is an independent project and is not affiliated with or endorsed by that team.

## Libraries

eframe/egui/wgpu, gstreamer-rs, rusqlite (bundled SQLite), image, rfd,
directories, walkdir, blake3, clap, serde. See `Cargo.lock` for exact versions.

Licences of the Rust dependencies (checked with `cargo metadata`): almost all
are permissive (MIT, Apache-2.0, BSD, ISC, Zlib, BSL-1.0, Unicode, CC0 or
Unlicense, usually as "MIT OR Apache-2.0"). Two exceptions, both compatible
with this project's licence when used unmodified:

- `option-ext` (pulled in by `directories`) is MPL-2.0, a file-level copyleft.
- `epaint_default_fonts` (egui) bundles fonts under the SIL Open Font License
  1.1 and the Ubuntu Font Licence 1.0.

Where a crate offers a choice that includes a copyleft licence (`self_cell`:
Apache-2.0 OR GPL-2.0; `r-efi`: MIT OR Apache-2.0 OR LGPL-2.1+), the permissive
option applies.

System libraries, linked dynamically and not shipped in this repository:
GStreamer and its plugins (LGPL-2.1+, some plugins under other licences), and
the Vulkan loader and GPU drivers. SQLite is public domain and compiled in by
`rusqlite`.
