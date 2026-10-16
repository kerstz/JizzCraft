# Interop

JizzCraft reads other tools' files and keeps your data in your own database.
It is never a server and it never phones a service. The rules are in
[AGENTS.md](../AGENTS.md#interop); this page says what that means in practice.

## What is read and written

| What | Direction | Where |
| --- | --- | --- |
| Content hash `oshash` (OpenSubtitles-style, 16 hex digits) | stored in the library database, computed from the first and last 64 KiB of a file by `library.fingerprint`; read-only on the file | `jizzcraft-cli lookup --oshash HEX`, command `media.by_hash` |
| Metadata export (`jizzcraft-meta`, version 3) | written to a file you choose; items carry `fp` and `oshash` | `meta.export` |
| Title, release date, studio, performers, tag aliases, external ids | stored in the library database; set by hand or imported; exported in `jizzcraft-meta` version 3 (private ones only with `--private`) | `media.info*`, `studio.*`, `person.*`, `tag.alias.*`; see [organise.md](organise.md#title-studio-performers-and-aliases) |
| Kodi-style `.nfo` files (Kodi, Jellyfin, Emby, Plex) | read only, never written; applied to the library database | `meta.import_nfo`, CLI `import-nfo`, Tools menu "Import from other apps…" |
| Stash JSON export folders | read only, never written; applied to the library database | `meta.import_stash`, CLI `import-stash`, same Tools window |
| Kodi-style `.nfo` files, written | new files in a folder you choose, never inside or above a library folder, never overwriting, mode 0600; plain text even when the library is encrypted (the app warns first) | `meta.export_nfo`, CLI `export-nfo`, Tools menu "Export NFO files to a folder…" |
| Funscript sidecars (`<name>.funscript` next to a video) | read only, on a worker thread; shown as a decorative heatmap band above the timeline and as "Script: N actions" in the inspector; never stored, never sent anywhere | `media.funscript`, CLI `funscript`, Settings toggle, command `player.heatmap.toggle` |
| Visual fingerprint (perceptual hash, 64 bits) of videos | computed by `library.phash` from 25 frames decoded in memory (never written to disk); stored in the library database (schema v7); compared only by Hamming distance | `library.phash`, `media.duplicates`, `media.phash.distance`, CLI `phash` and `duplicates`, Tools menu "Find possible duplicates…" |
| Metadata import | reads versions 1 to 3; matches by `fp`, then by `oshash` and size | `meta.import` |

### The oshash

`oshash = size + sum of the little-endian 64-bit words of the first 64 KiB
and the last 64 KiB`, wrapping at 2^64. A file under 64 KiB uses
`size / 8 * 8` bytes at each end. A file of 8 bytes or less has none. It is
the same value other tools show for the same file, which lets you line up
your library with theirs. It is a quick fingerprint, not a proof: two
different files can share it, which is why imports compare the size too and
refuse to guess between several candidates.

`library.fingerprint` computes both hashes in one read and also fills the
oshash of files fingerprinted before it existed. A file that changes on disk
loses both hashes at the next scan.

## NFO files

`jizzcraft-cli import-nfo [DIR] [--apply] [--overwrite] [--ratings] [--history]`
reads Kodi-style `.nfo` files (root `movie`, `musicvideo` or `episodedetails`)
below DIR, or below every library folder when DIR is omitted, and applies them
to the matching library items. Without `--apply` it only counts: a preview is
the same code as the import, rolled back, so its numbers are exact. The
desktop app has the same thing under Tools, "Import from other apps…", with a
Preview button; Import stays disabled until a preview of the same folder and
options has run.

Which item a file belongs to, first hit wins:

1. a `uniqueid` of type `jizzcraft-fp1` (our fingerprint) or `oshash`, which
   works for files outside any library folder;
2. the video next to it with the same name apart from the extension (case is
   ignored). Two videos with that name (`a.mp4` and `a.mkv`) are ambiguous and
   skipped;
3. a `movie.nfo` beside exactly one video.

What is read and where it goes:

| NFO element | Becomes |
| --- | --- |
| `title` | the title |
| `premiered`, else `year` (as `YYYY-01-01`) | the release date |
| first `studio` | the studio (others are ignored) |
| `actor/name`, `actor/order` | performers, in that order |
| `genre`, `tag` | tags; a name matching a tag alias reuses that tag |
| `uniqueid type=...` | external ids (our own `jizzcraft-fp1` is not stored) |
| `userrating` (0 to 10) | stars, `round(v / 2)` clamped to 0 to 5; only with `--ratings` and only onto unrated items unless `--overwrite` |
| `playcount`, `lastplayed`, `resume/position` | play count (larger wins), last played (later wins, midnight UTC of that day), resume point (only if empty); only with `--history` |

Everything else (`plot`, `outline`, `set`, `thumb`, `fanart`, `trailer`,
`fileinfo`, `ratings`, `runtime`, `dateadded`...) is counted as ignored. The
policy is "fill": a title, date or studio you already have is kept unless you
pass `--overwrite`; performers and tags are added; favorite and hidden are
never changed.

The reader is deliberately narrow. A file with a DOCTYPE is refused (so no
entity can expand and no external entity is read); only the five predefined
entities and numeric references are understood; files over 1 MiB, nested more
than 16 deep, or with an absurd number of elements, tags, actors or ids are
refused. A file that holds only a web address is counted and the address is
never fetched. Symbolic links are not followed, hidden folders are skipped,
and at most 100 000 files are read. The report holds counts only. Stopping an
import from the app rolls everything back.

## Stash exports

`jizzcraft-cli import-stash DIR [--apply] [--overwrite] [--ratings] [--history]`
reads the folder Stash writes with Settings, Tasks, Export: `scenes/` (required,
otherwise the folder is refused), `files/`, and optionally `performers/`,
`studios/` and `tags/`. Like the NFO import it is a dry run unless `--apply`,
bounded, and counts-only; the Tools window has a "Stash export folder" source.

Items are matched by content, never by path (Stash's paths are those of
another machine, and a path in a file is never opened):

- the file's `size` and `oshash`: exactly one library video matches, several
  are ambiguous and skipped. A library file without an oshash gets one computed
  on the spot (a read-only look at 128 KiB; a preview forgets it again);
- when the export has no `oshash`: the same `size`, a duration within half a
  second, and the same file name apart from case, exactly one.

A scene applies to each of its matched files. Mapping:

| Stash | Becomes |
| --- | --- |
| `title`, `date` | title, release date |
| `studio` | studio |
| `performers` (+ `performers/<name>.json`) | performers in listed order, with disambiguation, aliases and favorite (favorite only on a performer this import creates) |
| `tags` (+ `tags/<name>.json`) | tags, with their aliases; parents are flattened and counted as ignored |
| `markers` | markers: `seconds` rounded to milliseconds, labelled with the title or the primary tag; end times are dropped |
| `stash_ids[0]` | external id `stashbox` (the endpoint address is dropped) |
| file `phash` | external id `phash.stash`, 16 hex digits of the 64 bits |
| `rating` | stars, only with `--ratings`: 1 to 5 is stars, above 5 is the 1 to 100 scale and becomes `ceil(v / 20)` |
| `play_history`, `play_count`, `resume_time` | play count (larger wins), last played (later wins), resume point (only if empty); only with `--history` |

`md5`, `o_history`, `organized`, `code`, `details`, `director`, `urls`,
`groups`, `custom_fields` and covers are counted as ignored. Performer birth
dates, genders, body attributes and images are never read into the library. A
`phash` is read as an exact 64-bit integer (a JSON number is never passed
through a float) or as a string of 16 hex digits, else decimal.

Limits: at most 500 000 JSON files in total (more is refused), 256 KiB per
file, 16 MiB per scene, 4 MiB per performer or studio, 1 MiB per tag file;
anything over is counted as rejected.

## NFO export

`jizzcraft-cli export-nfo DEST [--private] [--history] [--no-ratings] [--lock] [--date-added]` writes
one `<movie>` file per video that has something to say, as
`DEST/<library folder name>/<relative folder>/<video name>.nfo`. `DEST` must be
an empty folder or a new folder inside an existing one, and it is refused when
it is, is inside, or contains a library folder (symbolic links are resolved,
and the check is repeated after each folder is created). Files are created
with `create_new`, so nothing is ever overwritten, and with mode 0600 on Unix.
Copying them next to your media is your step; JizzCraft never writes there.

| Library | NFO element |
| --- | --- |
| title, release date | `title`, `premiered` |
| studio | `studio` |
| performers (listed order) | `actor` with `name` and `order` |
| tags (categories are not exported) | `tag` |
| stars | `userrating` = stars x 2, unless `--no-ratings` |
| play count, last played, resume point | `playcount` (plus `watched` `true` when above 0), `lastplayed`, `resume/position`, only with `--history` |
| date the item entered the library | `dateadded`, UTC `YYYY-MM-DD HH:MM:SS`, only with `--date-added` |
| nothing (a switch) | `lockdata` `true`, only with `--lock` |
| external ids | `uniqueid type="<scheme>"` (`phash.stash` is left out) |
| fingerprint, oshash | `uniqueid type="jizzcraft-fp1"` and `type="oshash"`, so the importer finds the item again wherever the files go |

Private performers, studios and tags are left out unless `--private`. Only
videos are exported. A video with no exported field gets no file (`lockdata`
and `dateadded` alone never make one); two videos
that differ only by extension (`a.mp4`, `a.mkv`) would share `a.nfo`, so the
first one wins. Writing the same library twice into two empty folders gives
byte-identical trees.

On import, `watched` `true` counts as one play when there is no `playcount`
and the item has none yet (only with `--history`); `lockdata` and `dateadded`
are counted as ignored.

### Handing your library to a media server

JizzCraft never talks to a server. To give Jellyfin, Emby, Plex or Kodi your
titles, performers, tags and ratings:

1. Export to a folder outside your libraries: `jizzcraft-cli export-nfo
   ~/nfo-out --lock --date-added` (or Tools, "Export NFO files for a media
   server…", which writes `lockdata`).
2. Copy the files next to your media yourself, never replacing anything:
   `rsync -a --ignore-existing DEST/<label>/ <library>/` where `<label>` is the
   folder named after your library folder.
3. Refresh the library in the server.

Notes per server:

- **Jellyfin**: a local NFO is local metadata and wins over online providers
  such as TMDb. Turn the online metadata fetchers off in the library settings,
  or the server may still look titles up. Play state (`playcount`, `watched`,
  `lastplayed`) is imported for the one user chosen in the NFO settings only;
  there is no import of user ratings or resume points. `lockdata` makes the
  server keep the file's values, which also blocks edits made inside Jellyfin,
  so it is opt-in.
- **Plex**: needs Plex Media Server 1.43.1 or later and the "Plex NFO Movie"
  agent, picked in the library's advanced settings.
- **Emby**: reads Kodi-style NFO (Medium confidence; not checked against an
  official page).

Subtitle sidecars next to a video are read with Jellyfin's naming. After the
video name come up to four dot-separated parts, in any order: at most one
language tag and any of these flags (case is ignored):

| Part | Meaning |
| --- | --- |
| `default` | the default track |
| `forced`, `foreign` | forced: only translates foreign speech; never picked ahead of a full track in the same language |
| `sdh`, `cc` | for the hearing impaired |
| `hi` | for the hearing impaired, only when another language tag is present (`name.en.hi.srt`) |

A lone `.hi.` (`name.hi.srt`) is the language Hindi, a deliberate choice:
that is what it was before flags existed, and ISO 639-1 says the same. A name
with two language tags (`name.en.fr.srt`) or more than four parts is skipped.
The track menu shows "EN · Forced" or "EN · SDH".

## Funscripts

A `.funscript` with the same name as the video (case ignored) in the same
folder is read for display only. Per-axis variants such as `name.roll.funscript`
are ignored. Only the `actions` list (`at` in milliseconds, `pos` 0 to 100) is
used; `inverted`, `range` and every other field are ignored. Entries with a
negative or non-integer time, or a non-numeric position, are dropped; positions
are clamped to 0..100; the list is sorted, de-duplicated by time and cut at
200 000 actions. Files over 8 MiB are ignored.

The timeline band shows the mean stroke speed per cell (position units per
second, 400 and above reads as full intensity) in a single hue. It is hidden
in discreet mode and can be turned off in Settings. Nothing is ever sent to a
device and nothing is written next to the video.

## Duplicates

`library.phash` decodes 25 frames of each video (from 5 % to 95 % of its
length, 160 px wide), pastes them into a 5 x 5 sprite and computes a DCT
perceptual hash of it: the layout is the one Stash uses, so values imported
from a Stash export (`phash.stash`, see above) are compared with yours. The
bits are not guaranteed to be identical to Stash's (a different decoder gives
slightly different pixels), so two hashes are only ever compared by Hamming
distance, never by equality. Static test patterns hash poorly (a 5 x 5 tiling
of one picture has almost no low-frequency detail); real footage is fine.

A video that cannot be decoded is remembered as "tried" and not retried until
you ask for everything again (`--all`) or the recipe changes. A changed file
loses its hash on the next scan. Frames never touch the disk.

`media.duplicates` links videos whose hashes are within the chosen distance
(exact 0, high 4, medium 8, low 10; at most 16), transitively, optionally
refusing pairs whose durations differ by more than `duration_diff_ms`. Groups
hold at most 50 items. The window only reviews: it can open an item or put a
group in a new playlist. There is no delete action anywhere.

## Refused, and why

- **Serving your library** (DLNA, UPnP, WebDAV, Jellyfin, Plex or Subsonic
  APIs, mDNS): a listening socket is a way in. The default build has no
  network of any kind and the Online build only connects when you act.
- **Scrapers and site presets, Stash YAML or Python scrapers**: no site is
  hard-coded and no foreign code runs.
- **Looking up hashes on a remote service, or contributing fingerprints**:
  your library would leak to a third party. There is no default endpoint.
- **Driving devices or cloud sync**: funscripts, when supported, are only
  displayed.
- **Actor images, `.strm` files, a DLNA source or renderer, Kodi JSON-RPC, the
  Plex API (it needs a plex.tv token), rename plans or scripts**: artwork,
  remote streams and drives of other programs are not ours to write or call.
- **Performer attributes beyond a name**: no birth dates, gender, body
  attributes or images are stored.

## Export guards

M3U playlists (`playlist.export_m3u`), metadata exports (`meta.export`) and
NFO files never land inside a library folder: the destination folder is
resolved (symbolic links included) and refused when it is, or is inside, a
library folder, with "destination must be outside every library folder".
Backups are not affected. If a library folder is your home folder (or a
parent of where you want to write), almost every destination is inside it and
the export is refused; the error says why. Pick a folder outside it, or add a
narrower library folder.

## Remote libraries

Mount the share with your operating system (NFS, SMB, SFTP via the file
manager), then add the mount point as a library folder. JizzCraft only sees
files.

## Licences

The formats are reimplemented from their specifications and from observed
files. No AGPL or GPL code is copied.
