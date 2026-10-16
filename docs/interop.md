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
