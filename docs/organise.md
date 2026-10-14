# Organise your library

Three ways to group items. All of them are rows in the library database:
creating, renaming, reordering or deleting one never writes, moves or deletes
a media file, and never writes a sidecar file.

| | Shape | Order | An item can be in |
| --- | --- | --- | --- |
| Playlist | list | by hand (or sorted once on demand) | several, once each |
| Category | tree, depth up to 4 | by position, then name | several |
| Tag | flat | by position, then name | several |

Names are unique within their kind, ignoring case.

## Playlists

A playlist is an ordered list without duplicates. Reorder by moving items
before another item, or sort it once by name, date added, duration or random;
the result is stored, so the order is then yours to edit. Deleting a playlist
removes the list only.

```sh
jizzcraft-cli playlist list
jizzcraft-cli playlist create Evening
jizzcraft-cli playlist add 1 12 7 31            # append media ids
jizzcraft-cli playlist move 1 31 --before 12    # 31 now sits before 12
jizzcraft-cli playlist sort 1 --by duration --desc
jizzcraft-cli playlist items 1
jizzcraft-cli playlist export-m3u 1 ~/evening.m3u8 --relative
```

`export-m3u` writes an `.m3u8` of absolute (or `--relative`) paths. It refuses
to overwrite an existing file. Item names are left out unless you pass
`--include-names`. Files flagged missing are skipped and counted.

## Categories and tags

```sh
jizzcraft-cli category create Travel
jizzcraft-cli category create Italy --parent 1
jizzcraft-cli category move 2               # no --parent: back to the top
jizzcraft-cli category assign 2 12 7
jizzcraft-cli tag create favourite-lighting
jizzcraft-cli tag assign 4 12 7             # --off removes
jizzcraft-cli tag toggle 4 12
jizzcraft-cli tag merge 4 5                 # 4 folds into 5
```

Deleting a category moves its children up one level. Deleting or merging a
tag leaves the items in the library. Filtering by a category includes the
whole subtree.

## Querying

```sh
jizzcraft-cli query --playlist 1 --sort position
jizzcraft-cli query --tag 4 --tag 5          # has both (AND)
jizzcraft-cli query --any-tag 4 --any-tag 5  # has at least one (OR)
jizzcraft-cli query --tag 4 --not-tag 6      # NOT
jizzcraft-cli query --category 1             # Travel and everything below
jizzcraft-cli query --untagged
jizzcraft-cli query --uncategorized
```

`--sort position` is only valid together with `--playlist`. These filters
combine with every other `query` option and with saved views. Add `--json`
to any command to get the registry command's JSON unchanged; `run` reaches the
same commands by name (`playlist.*`, `tag.*`, `category.*`, `labels.of`).

## When files move

Memberships follow the item, not the path. Renaming or moving a file is picked
up by the next rescan as the same item (by fingerprint), and keeps its
playlists, tags and categories. When a file vanishes it is flagged missing and
stays in its playlists, greyed out, so it comes back in place if the file
returns. `relink` merges a vanished entry into its new location and carries
its playlist position and labels with it.

## Discreet mode and privacy

Playlist, tag and category names can say a lot. Anything can be flagged
private. In discreet mode the desktop app masks these names (and counts)
in menus, the sidebar and the inspector. The CLI shows what you ask for, so
do not run it on a shared screen. Errors only repeat ids and what you typed,
never stored names; toasts and logs carry counts only.

## Export and import

`jizzcraft-cli export FILE` writes favorites, ratings, markers, tags,
categories and playlists keyed by file fingerprint, so they can be imported
onto the same files at another path or on another machine
(`jizzcraft-cli import FILE --dry-run` first). By default:

- names of files are not included (`--names` adds them);
- history is not included (`--history` adds it);
- anything flagged private is not included (`--private` adds it; a category
  is private when any ancestor is).

Treat an export that uses `--names` or `--private` like the library itself.
