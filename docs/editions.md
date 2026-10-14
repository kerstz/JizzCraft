# Editions

JizzCraft is one code base built two ways.

| | Offline | Online |
| --- | --- | --- |
| Build | default | `--features online` (alias of `network`) |
| Network code | none; a test fails if a network crate enters the tree | opt-in, nothing connects before you act |
| Adds | | indexers, torrents, streaming from your own sources, resolvers, "Send to JizzCraft", optional browser |
| Package | `jizzcraft` | `jizzcraft-online` (replaces `jizzcraft`) |
| Tarball | `jizzcraft-<v>-<arch>-linux.tar.xz` | `jizzcraft-online-<v>-<arch>-linux.tar.xz` |
| Browser | n/a | separate optional package `jizzcraft-browser` |

Same application ID, same binaries, same data folders: you do not run both at once.
Library, settings and thumbnails are shared, so switching loses nothing.

## Choose and switch

```sh
install.sh --edition offline          # or online
install.sh --edition online --with-browser
install.sh --edition offline          # back; online-only files are removed, your data is not touched
```

Re-running the installer without flags keeps the edition recorded in the install
manifest, so an Online user is never downgraded by an update.

`jizzcraft-cli --version` and the first lines of `jizzcraft-cli doctor` say which
edition runs. `Settings → General` shows it too.

## Branches

`main` is where both editions are developed. `edition/offline` and
`edition/online` are release channels: a workflow fast-forwards them to each
released commit. They are not separate code lines, so nothing diverges.

## The browser

Optional, Online only, chosen at install time and switchable in Settings. See
[browser.md](browser.md). It is a separate process that never links into the app.
