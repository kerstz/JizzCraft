# Interop

JizzCraft reads other tools' files and keeps your data in your own database.
It is never a server and it never phones a service. The rules are in
[AGENTS.md](../AGENTS.md#interop); this page says what that means in practice.

## What is read and written

Filled in as each piece lands.

| What | Direction | Where |
| --- | --- | --- |
| Content hash `oshash` (OpenSubtitles-style, 16 hex digits) | stored in the library database, computed from the first and last 64 KiB of a file by `library.fingerprint`; read-only on the file | `jizzcraft-cli lookup --oshash HEX`, command `media.by_hash` |
| Metadata export (`jizzcraft-meta`, version 3) | written to a file you choose; items carry `fp` and `oshash` | `meta.export` |
| Title, release date, studio, performers, tag aliases, external ids | stored in the library database; set by hand or imported; exported in `jizzcraft-meta` version 3 (private ones only with `--private`) | `media.info*`, `studio.*`, `person.*`, `tag.alias.*`; see [organise.md](organise.md#title-studio-performers-and-aliases) |
| Kodi-style `.nfo` files (Kodi, Jellyfin, Emby, Plex) | read only, never written; applied to the library database | `meta.import_nfo`, CLI `import-nfo`, Tools menu "Import from other apps…" |
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
- **Performer attributes beyond a name**: no birth dates, gender, body
  attributes or images are stored.

## Remote libraries

Mount the share with your operating system (NFS, SMB, SFTP via the file
manager), then add the mount point as a library folder. JizzCraft only sees
files.

## Licences

The formats are reimplemented from their specifications and from observed
files. No AGPL or GPL code is copied.
