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

## Conventions

README structure and the "engine first, agent-ready" layout follow the
conventions of PhotoCraft by storytold (https://github.com/storytold/photocraft).
No code, assets, logos or wordmarks from ArtCraft / Crafting Apps are used.
JizzCraft is a parody and is not affiliated with or endorsed by that team.

## Libraries

eframe/egui/wgpu, gstreamer-rs, rusqlite (bundled SQLite), image, rfd,
directories, walkdir, blake3, clap, serde. See `Cargo.lock` for exact versions
and each crate's license (all permissive: MIT / Apache-2.0 / Unlicense-style).
