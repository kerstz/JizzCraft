# Media servers and the adult-media ecosystem: what to integrate with, and what to learn

Research date: 2026-10-16 (all sources accessed that day unless stated). Scope: Jellyfin, Plex, Emby, Kodi, Stash
(GraphQL, plugins, scrapers, stash-box/StashDB), Whisparr, ThePornDB, adult metadata plugins, NFO, funscript,
oshash/phash, tag and performer/studio models, DLNA/UPnP, WebDAV, Subsonic-like APIs. This report is descriptive
and SFW. It is not legal advice. No code was changed. No Stash, Jellyfin, Plex or Emby server was run; every
claim comes from documentation, schemas and source code read on the access date (see "Method limits").

Repository facts about JizzCraft come from `AGENTS.md`, `ROADMAP.md`, `docs/streaming.md`,
`crates/core/src/{db,fingerprint,portability,subtitles}.rs` at commit `a083af2` (v0.2.0, project paused).

How to read confidence levels:

- **High (85-100 %)**: read in a primary source (official docs, schema, source code), and either confirmed by a
  second independent source or directly observable.
- **Medium (60-84 %)**: one primary source, or several secondary ones that agree.
- **Low (<60 %)**: one secondary source or an inference. Treated as **UNVERIFIED**.

Claim types: **[F]** verified fact, **[E]** expert/project opinion, **[S]** speculation or inference, **[R]** rumor
(none used as a basis).

---

## TL;DR

1. **The Kodi NFO format is the lingua franca.** Kodi reads it natively, Jellyfin gives it priority over online
   providers, Emby builds on it, and Plex shipped an NFO agent (PMS 1.43.1+) that is "compliant with the Kodi/XBMC
   NFO format". One read-only NFO importer therefore covers four ecosystems and fits every JizzCraft rule. [F, High]
2. **Stash is the only tool whose data model matches JizzCraft's audience** (scenes, performers, studios, tags,
   markers, oshash/md5/phash fingerprints, funscript). Its **JSON export** (a folder of JSON files) is a stable,
   offline, documented hand-off format. Importing it needs no network and no change to AGENTS.md. [F, High]
3. **oshash is nearly free for JizzCraft.** Its existing `fp1` fingerprint already reads the first and last 64 KiB
   of each file; oshash is a 64-bit little-endian word sum over the same two windows plus the file size. Adding it
   in the same pass makes JizzCraft data joinable with Stash, ThePornDB and StashDB without any network. [F, High]
4. **phash cannot be bit-compatible in practice.** Stash computes it from 25 ffmpeg frames of 160 px width tiled
   into a 5x5 sprite, then a DCT perceptual hash through `goimagehash` (bilinear resize to 64x64). JizzCraft
   decodes with GStreamer. Import Stash phashes and compare by Hamming distance (Stash's own UI levels are
   0/4/8/10); do not promise equal values. [F, High for the algorithm; S for cross-decoder drift]
5. **Do not build servers.** DLNA/UPnP, WebDAV and Jellyfin/Subsonic-compatible APIs all need an inbound listener,
   have weak or no authentication models for a LAN, and would leak the existence of an adult library. Jellyfin
   itself removed DLNA from its core in 10.9. They are incompatible with "default build has no network". [F/E, High]
6. **Never copy the third-party-cloud and site-scraper patterns.** Stash's interactive support uploads the user's
   funscript (as CSV) to `handyfeeling.com` from the browser; Stash's 709 community YAML scrapers and 167 Python
   scripts are per-site presets under AGPL-3.0. JizzCraft's "no presets, no telemetry" rules rule both out. [F, High]
7. Remote-identification (stash-box, ThePornDB) is **not recommended now**: it ships library hashes to a third
   party, StashDB access is invite-gated and its terms of use were not found, and the legal footing is unclear.
   Keep it as a deferred, per-click, user-supplied-endpoint idea. [F for access model; terms UNVERIFIED]

---

## Executive summary

JizzCraft is a single-user, offline viewer. The five mature media systems studied are multi-user servers. The overlap
is therefore not "act like a server" but "speak the same files": NFO sidecars, Stash JSON, funscript, oshash.

What the ecosystem has that JizzCraft lacks, in order of value to its users:

1. **A scene-level metadata model**: title, date, code, studio, performers, details, external IDs. JizzCraft stores
   only the file name, tags, categories, playlists and user state. This is the prerequisite for every import.
2. **Content identity beyond the file**: oshash/md5/phash fingerprints. JizzCraft has `fp1` and a blake3 checksum,
   both private to the app.
3. **Perceptual duplicate detection** (already on JizzCraft's ROADMAP).
4. **Funscript** pairing and timeline display.
5. **Standard-format import/export** (NFO, Stash JSON).

What the ecosystem does that JizzCraft should refuse by design: network servers, plugin runtimes that execute
foreign code, vendor-cloud device sync, online account systems, and remote metadata lookups on by default.

Legal and privacy findings that shape every option: licence incompatibility (Stash, community scrapers, Jellyfin,
Kodi, Whisparr, OpenFunscripter are AGPL/GPL while JizzCraft is MIT OR Apache-2.0, so formats may be re-implemented
but code and rule files must not be copied); GDPR Article 9 treats data about a person's sex life as special-category
data, with the household exemption available only for purely personal use; performer data is data about third
parties; every remote lookup reveals a library fingerprint to the operator.

---

## 1. Feature matrix: what these tools do that JizzCraft lacks

Legend: Y = yes, N = no, P = via plugin/add-on, n.v. = not verified in this research, D = deliberately excluded by
AGENTS.md. "JC" = JizzCraft v0.2.0.

| Capability | Stash | Jellyfin | Plex | Emby | Kodi | Whisparr | JC today |
|---|---|---|---|---|---|---|---|
| Scene metadata (title, date, code, plot) | Y (Scene type) [S3] | Y (BaseItemDto) [S9] | Y [S10] | Y [S11] | Y (NFO) [S8] | Y [S12] | N (file name only) |
| Performer entity | Y, rich (aliases, gender enum, dates, URLs, rating, favorite) [S3] | Y (`People`) [S9] | Y [S10] | Y [S11] | Y (`actor`) [S8] | Y (v3 import lists by performer) [S12] | N |
| Studio entity with parent | Y (`parent_studio`, aliases) [S3] | flat `Studios` [S9] | studio field [S10] | n.v. | `studio` tag [S8] | Y (v2 focuses on studios) [S12] | N |
| Tags | Y, hierarchy (parents/children), aliases, sort name [S3] | flat tags and genres [S9] | n.v. | n.v. | `tag`, `genre` [S8] | n.v. | Y: flat tags + category tree (depth 4), private flag, FTS5 [S22] |
| Content fingerprints | oshash, md5, phash [S3][S4] | N | N | N | N | n.v. | `fp1` (blake3 head/tail) + blake3 checksum; no oshash/phash |
| Perceptual duplicate finder | Y, levels 0/4/8/10 [S5] | N | N | N | N | N | N (ROADMAP item) |
| Remote identification DB | Y: stash-box/StashDB, ThePornDB [S3][S6] | P: ThePornDB, PhoenixAdult [S13] | P (legacy bundles, being removed) [S10][S13] | P (same plugins) [S13] | P (third-party scrapers) | Y: StashDB import lists, TPDB migration in source [S12] | D |
| Rule-based scrapers | Y: YAML (xpath/json/script), 709 community files [S1][S23] | P (C# per site) [S13] | P (Python bundles) [S13] | P | P | n.v. | D (declarative streaming sources only) [S22] |
| NFO read | N (own JSON) | Y, priority over online [S9] | Y, NFO agent PMS >= 1.43.1 [S10] | Y [S11] | Y, native [S8] | writes Kodi metadata (README claim) [S12] | N |
| NFO write | N | Y, opt-in saver, writes beside media [S9] | N stated | Y, per library, off by default [S11] | Y, "Export library" [S8] | Y (README claim) [S12] | N, and D if beside media |
| Funscript / interactive | Y: same-basename `.funscript`, Handy [S5] | N | N | N | N | N | N |
| Plugins | Y, experimental: tasks, post-hooks, UI JS/CSS [S2] | Y, .NET catalog [S9] | New: HTTP metadata providers from PMS 1.43.0 [S10] | Y [S11] | Y, Python add-ons [S8] | N | D (ROADMAP: "not plugins for the plugin system") |
| Remote API | GraphQL `/graphql`, `ApiKey` header [S3][S5] | REST, OpenAPI 12.2.0, 294 paths [S9] | PMS HTTP API, `X-Plex-Token` from plex.tv [S10] | REST at `/emby/` [S11] | n.v. (JSON-RPC not checked) | REST [S12] | CLI + command registry; no network API [S22] |
| DLNA/UPnP server | n.v. | P: first-party plugin since 10.9 [S9][S16] | n.v. | n.v. | n.v. | N | D |
| Accounts / multi-user | single login + API key [S5] | Y | Y, requires plex.tv [S10] | Y | profiles (n.v.) | Y | D |
| Markers | Y: start, end, primary tag, tags [S3] | chapters [S9] | n.v. | n.v. | n.v. | N | Y: point markers with label [S22] |
| Watch state | play_count, play_history, o_counter, resume [S3] | `UserItemDataDto`: Played, PlayCount, PlaybackPositionTicks, IsFavorite, Rating [S9] | `viewCount`, `viewOffset`, `userRating` [S10] | n.v. | `playcount`, `lastplayed`, `resume`, `userrating` [S8] | n.v. | rating 0-5, play_count, last_played, resume, history toggle [S22] |
| Encrypted library / vault | N found | N found | N found | N found | N found | N found | Y (SQLCipher vault) [S22] |
| Age gate, boss key, discreet mode | tripwire for internet exposure only [S5] | N | N | N | N | N | Y |
| Offline guarantee, tested | N (version check, scraping) | N (plugin catalog) | N (plex.tv) | N | N | N | Y [S22] |

Confidence: High for rows citing a schema or source file (Stash, Jellyfin OpenAPI, Kodi wiki); Medium for rows
citing README text (Whisparr) or secondary text; "N found" means not found in the documents reviewed, not proof of
absence.

**Tag and taxonomy models worth knowing** (High):

- Stash `Tag`: `name`, `sort_name`, `description`, `aliases[]`, `parents[]`, `children[]` (several parents are
  allowed), `ignore_auto_tag`, `favorite`, `stash_ids[]`. [S3]
- stash-box `Tag`: `name`, `description`, `aliases[]`, `category` -> `TagCategory{name, group, description}` with
  `group` in `PEOPLE | SCENE | ACTION`. [S6]
- Calibre: user-defined columns, hierarchical items written with dots (`History.Military.WWII`), searched with
  prefix (`=.`) and component (`=..`) operators, plus saved searches and virtual libraries. [S20]
- JizzCraft: single-parent category tree (depth <= 4) plus flat tags; no aliases. The lesson is **aliases**: they
  make imports merge instead of duplicating (`blow job` and `blowjob`), and `label_merge` already exists. [S22]

**Performer model (Stash)**: `name`, `disambiguation`, `alias_list`, `gender` (`MALE | FEMALE | TRANSGENDER_MALE |
TRANSGENDER_FEMALE | INTERSEX | NON_BINARY`), `birthdate`, `death_date`, `country`, `ethnicity`, `eye_color`,
`hair_color`, `height_cm`, `weight`, `measurements`, `tattoos`, `piercings`, `urls`, `favorite`, `rating100`,
`stash_ids`, `custom_fields`. [S3] Recommendation: store far less (see section 2.7).

---

## 2. Integration options, ranked

Scoring: **Value** (to a JizzCraft user), **Effort** (S under 2 days, M under 1 week, L longer, XL project), **Risk**
(technical, legal, privacy), **AGENTS.md** (compatible / conditional / incompatible). Ranked by value over
effort and risk. Every import is proposed as opt-in, dry-run first (the same pattern as `portability::import_meta`),
bounded in size, DB-only, and covered by the existing `engine_never_touches_media` tree-hash test.

| Rank | Option | Value | Effort | Risk | AGENTS.md |
|---|---|---|---|---|---|
| 1 | oshash column, computed with `fp1` | High | S | Low | Compatible |
| 2 | Kodi-format NFO import (read-only) | High | M | Low | Compatible |
| 3 | Stash JSON export import (offline) | High | M | Low | Compatible |
| 4 | Funscript sidecar, read-only display | Medium | S-M | Low | Compatible |
| 5 | JizzCraft-native phash + import of Stash phash values | High | L | Medium | Compatible |
| 6 | Kodi NFO export to a mirror folder outside the library | Medium | S-M | Low-Med | Compatible only outside media trees |
| 7 | Stash GraphQL live import (network build) | Medium | M | Medium | Conditional (see 2.8) |
| 8 | Jellyfin/Emby API import (network build) | Low-Med | M | Medium | Conditional |
| 9 | Plex API import (network build) | Low | M | High | Conditional |
| 10 | User-supplied scraper rules (JizzCraft-native JSON) | Medium | L | High | Conditional, deferred |
| 11 | stash-box / ThePornDB identify by hash | Medium | M | High | Conditional, deferred |
| 12 | Mount remote libraries by OS (WebDAV/SMB) as plain folders | Medium | 0 (docs) | Low | Compatible |
| 13 | Direct read of other apps' SQLite databases | Low | M | High | Not recommended |
| 14 | DLNA/UPnP, WebDAV server, Jellyfin/Subsonic-compatible server | Low | L-XL | High | Incompatible |
| 15 | Device output (Handy cloud) | Low | M | High | Incompatible |

### 2.1 oshash column (rank 1)

**Format** [F, High: Stash `pkg/hash/oshash/oshash.go` read in full; PyPI `oshash` 0.1.1 and the Rust `oshash` 0.4.0
crate describe the same head/tail scheme; ThePornDB's API and stash-box both name `OSHASH`]:

- chunk = 65 536 bytes. If the file is smaller than a chunk, chunk = floor(size/8)*8. Files of 8 bytes or fewer are
  refused.
- head = first chunk, tail = last chunk (seek to size - chunk).
- each chunk is read as unsigned 64-bit little-endian words and summed with wrap-around.
- hash = (head_sum + tail_sum + size) mod 2^64, printed as 16 lowercase hex digits (`%016x`).

**Fit**: `crates/core/src/fingerprint.rs::quick_fp` reads exactly the first and last 64 KiB (or the whole file up to
128 KiB). Computing oshash from the same buffers adds no I/O. Store as `media.oshash` (new `MIGRATIONS` entry, v5;
text or 8-byte BLOB, indexed), key `oshash` in `export_meta` v3.
**Caveats**: oshash is a weak checksum (easy to forge a collision); always compare with `size`. Treat a match as a
hint, never as proof. Add a test vector computed by the Python and Go implementations on a synthetic file.
**Legal/privacy**: none while local. Sending it to a service is a separate, opt-in decision (2.11).

### 2.2 Kodi-format NFO import (rank 2)

**Format** [F, High: kodi.wiki "NFO files/Movies", revision 2026-04-13; Jellyfin NFO docs; Plex NFO article; Emby via
forum, Medium]:

- UTF-8 XML, one file per title. Root `<movie>` (also `<musicvideo>`, `<episodedetails>`, `<tvshow>`).
- Name and location: `<VideoFileName>.nfo` (recommended by Kodi) or `movie.nfo` (only honoured when the folder holds
  a single movie). Jellyfin accepts both; Plex accepts both.
- Tags read: `title`, `originaltitle`, `sorttitle`, `ratings/rating[@name,@max,@default]/value|votes`,
  `userrating`, `outline`, `plot`, `tagline`, `runtime` (minutes), `thumb[@aspect]`, `fanart/thumb`, `mpaa`,
  `playcount`, `lastplayed` (yyyy-mm-dd), `uniqueid[@type,@default]`, `genre`, `tag` (multiple), `set/name|overview`,
  `country`, `credits`, `director`, `premiered` (yyyy-mm-dd; `year` deprecated since v17 and "use premiered only"
  from v20), `studio` (multiple), `trailer`, `fileinfo/streamdetails/video|audio|subtitle`, `actor/name|role|order|thumb`,
  `resume/position|total` (seconds), `dateadded`.
- Minimal synthetic example:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<movie>
  <title>Synthetic Example</title>
  <premiered>2026-01-31</premiered>
  <studio>Example Studio</studio>
  <genre>drama</genre><tag>favourite-lighting</tag>
  <userrating>8</userrating>
  <playcount>2</playcount><lastplayed>2026-09-30</lastplayed>
  <resume><position>754.5</position><total>3600</total></resume>
  <uniqueid type="stash" default="true">00000000-0000-0000-0000-000000000000</uniqueid>
  <actor><name>Example Performer</name><order>0</order></actor>
</movie>
```

**Mapping into JizzCraft** (needs the model work in 2.7 for the starred rows):

| NFO | JizzCraft |
|---|---|
| `title`* | `media_info.title` |
| `plot`/`outline`* | `media_info.details` |
| `premiered`* | `media_info.date` |
| `genre`, `tag` | tags (`labels` kind 2), created if missing, merged by name or alias |
| `set/name` | category (or playlist), user's choice |
| `studio`* | studio row |
| `actor/name`* | performer rows, ordered by `order` |
| `userrating` (0-10) | `rating` = round(value/2), 0-5; lossy, report it |
| `playcount`, `lastplayed`, `resume/position` | `play_count`, `last_played_at`, `resume_ms` = seconds x 1000 |
| `uniqueid[@type]`* | `external_ids(scheme, value)` |
| `fileinfo`, `runtime`, `thumb`, `fanart`, `trailer`, `dateadded` | ignored (JizzCraft probes the file; never fetch or follow URLs) |

**Rules**: default merge policy "fill empty / keep the larger count / keep the newer date"; never overwrite a
rating or favorite without an explicit switch; dry-run report with matched / applied / unmatched / ambiguous
(the existing `ImportReport` shape). Match NFO to media by sidecar name, then confirm by `fileinfo` duration within
a tolerance when present.
**Security**: cap file size (1 MiB), element depth and repeated-tag counts; reject DOCTYPE; `quick-xml` is already a
workspace dependency (used by `fetch`) and does not resolve external entities by default (Medium: confirm with a
billion-laughs fixture test). A "URL-only" or Kodi "combination" NFO (a scraper URL inside) is ignored, never
fetched. `core` would gain `quick-xml`; it is not a network crate, so `offline_deps` is unaffected.
**Legal/privacy**: NFO files from third-party managers carry real performer names and may carry URLs of source
sites; treat them as the user's data; do not log values (AGENTS: counts only).
**Confidence**: High on format; Medium on Emby (forum statement, not an official article: Emby built NFO support
"on Kodi standards").

### 2.3 Stash JSON export import (rank 3)

**Format** [F, High: Stash `ui/v2.5/src/docs/en/Manual/JSONSpec.md` and `pkg/models/jsonschema/*.go` on `develop`,
accessed 2026-10-16]: an export is a folder tree of one JSON file per object, produced by Settings > Tasks >
Export. The docs name the sub-folders `files`, `galleries`, `images`, `performers`, `scenes`, `studios`, `groups`;
the source also contains `tag.go` (a `tags` folder is expected; see Contradictions). File names are not significant
on import: all JSON is read from the sub-folders.

- `files/<depth>.<basename>.<md5 of path>.json`: `{"type":"video","path":"...","size":N,"mod_time":"...",
  "fingerprints":[{"type":"oshash","fingerprint":"..."},{"type":"phash","fingerprint":...},{"type":"md5",...}],
  "format","width","height","duration"(seconds, float),"video_codec","audio_codec","frame_rate","bitrate",
  "interactive":bool,"interactive_speed":int}`. The `fingerprint` value is a **raw JSON value, string or number**:
  Stash issue #6894 records a precision loss when phash was decoded as float64. Parse it as raw text and convert to
  `u64`/`i64` yourself (Stash stores phash as a signed 64-bit integer in SQLite).
- `scenes/<title or basename>.<hash>.json`: `title, code, studio (name), urls[], date, production_date, rating (int),
  organized, details, director, performers[] (names), movies[]/groups, tags[] (names), markers[{title, seconds
  (string), end_seconds, primary_tag, tags[]}], files[] (references to file JSON), cover (base64), resume_time,
  play_duration, play_history[], o_history[], stash_ids[{endpoint, stash_id}], custom_fields{}`.
- `performers/<name>.json`, `studios/<name>.json`, `groups/<name>.json`, `tags` similarly.
- Timestamps `YYYY-MM-DDThh:mm:ssTZD`; numeric values in the docs may appear as strings.

**Matching**: by `size` + `oshash` (needs 2.1), falling back to `size` + `duration` (+/- 0.5 s) + file name;
ambiguous matches are skipped and counted, as `import_meta` does. Do not match by path (the Stash server's paths
differ).
**Mapping**: tags -> tags (aliases merge); `parents[]` -> category tree if single parent, else flatten and report;
markers -> `markers(pos_ms = round(seconds*1000), label = title or primary tag)`, drop `end_seconds` and report;
`rating` -> 0-5 (**unit unverified**: the GraphQL field is `rating100` (1-100) while the export field is a plain
`rating` integer; verify with a fixture export before choosing the divisor; Medium); `play_history` -> `play_count`
and `last_played_at`; `o_history` is Stash-specific and is dropped or kept as a tag by choice.
**Legal/privacy**: the folder may contain base64 cover images and performer images of real people; do not import
images in v1. Parser must bound file counts and sizes (a library of 100 000 scenes is 100 000+ files).
**Licence**: Stash is AGPL-3.0; the format description is fine to implement from, the Go code must not be copied.

### 2.4 Funscript sidecar, read-only (rank 4)

**Format** [F, High: buttplug.io docs (Nonpolynomial Labs, 2026), OpenFunscripter `Funscript.cpp`, MultiFunPlayer
README, Stash `Interactive.md` and `funscript.go`]:

```json
{"version":"1.0","inverted":false,"range":90,
 "actions":[{"at":1000,"pos":0},{"at":2000,"pos":99},{"at":3000,"pos":0}],
 "metadata":{"title":"...","chapters":[]}}
```

- `at` = milliseconds from video start (integer), `pos` = integer position. The Buttplug page says 0-99;
  OpenFunscripter writes `range: 100` and clamps `pos` to 0-100 (see Contradictions). Accept 0-100, clamp, sort by
  `at`.
- Naming: `<video basename>.funscript` in the same folder (Stash: `video.mp4` -> `video.funscript`, discovered at
  scan). Multi-axis variants add a suffix before the extension: `.surge`, `.sway`, `.twist`, `.roll`, `.pitch`,
  `.vib`, `.lube`, `.suck`, `.valve` (MultiFunPlayer README, Medium: one project's convention, not a standard).
- `inverted` and `range` have no official spec (Buttplug page does not define them). Treat as hints; ignore for
  display.

**Fit**: copy the shape of `subtitles.rs` (`find_sidecars`, `MAX_BYTES`, `MAX_CUES`, "no file name leaves this
module"). Caps: 8 MiB, 200 000 actions. Read-only: detect, parse, draw a speed heatmap on the timeline, mark the
item "scripted" as a derived flag (not user data, safe to recompute). No device output.
**Do not copy Stash's device path**: the Stash UI converts the funscript to CSV and uploads it to
`https://www.handyfeeling.com/api/sync/upload` (source `ui/v2.5/src/hooks/Interactive/interactive.ts`, High). That
sends the script, and ties a device connection key to what the user watches, to a vendor cloud. If device output is
ever added, it should be a loopback WebSocket to a user-run Buttplug server (Intiface), only in the `network` build,
behind its own AGENTS.md rule.
**Licence**: OpenFunscripter is GPL-3.0; implement from the JSON shape.

### 2.5 Perceptual hashing (rank 5)

**Stash algorithm** [F, High: `pkg/hash/videophash/phash.go` read in full; `goimagehash` source for the hash itself]:

1. 25 frames (5 columns x 5 rows) at times `0.05*D + i*(0.9*D/25)` for i in 0..24, D = duration in seconds.
2. Each frame produced by ffmpeg scaled to **160 px wide** (height follows the aspect ratio), decoded from BMP.
3. Frames pasted left-to-right, top-to-bottom into a canvas of 800 px x 5*height.
4. `goimagehash.PerceptionHash`: bilinear resize to 64x64, grayscale, 2-D DCT, take the 64 coefficients flattened
   by `DCT2DFast64`, compare each to the median, bit set if greater; MSB-first. Result: one `uint64`.
5. If any frame fails, no phash is stored (to avoid false positives; Stash docs).
6. Distance = Hamming distance. The duplicate checker UI offers **Exact 0, High 4, Medium 8, Low 10**, plus a
   `duration_diff` filter (GraphQL `findDuplicateScenes(distance, duration_diff)`).

**Why not bit-compatible**: the frame timestamps, the ffmpeg scaler, the Go `disintegration/imaging` paste and
`nfnt/resize` bilinear kernel all feed the hash. GStreamer will not reproduce them exactly [S, Medium]. Because a DCT
hash is dominated by low frequencies, small decoder differences should stay within a few bits, so near-match
works but equality does not.
**Plan**: compute JizzCraft's own 64-bit phash with the same layout, store it separately (`media.phash`, plus
`phash_algo` version), and store imported Stash/TPDB phashes in the same column only when flagged `imported`.
Use thresholds 0/4/8/10. Add the BK-tree or a linear popcount scan (a 100 000-item library is a few ms per query
with SIMD popcount; Medium estimate).
**Legal/privacy**: local-only. Computation reads media but never writes it. CPU cost: 25 seeks per video, so run as
a job with a cancel flag and a count-only toast (AGENTS: "anything slow is a job").

### 2.6 Kodi NFO export (rank 6)

Kodi's own "Export library -> Separate files" writes NFO next to each video and optionally artwork
(kodi.wiki, revision 2025-02-07, High). Jellyfin's NFO saver also writes beside the media. **JizzCraft must not do
this**: AGENTS forbids sidecar files and touching media, and `engine_never_touches_media` hashes the tree.
Compatible variant: `meta.export_nfo --out DIR` writes a **mirror tree** `DIR/<relative folder>/<basename>.nfo`
outside every library root; the user decides whether to copy it in. Required safeguards:

- Refuse a destination inside any library root. `portability::check_dest` currently only rejects destinations
  with a media extension and existing files, so this is a new check (and a test).
- Exclude private tags and categories by default (the FTS view already does this for search).
- Write `<title>`, `<userrating>` (rating x 2), `<playcount>`, `<lastplayed>`, `<resume>`, `<genre>`/`<tag>`,
  `<studio>`, `<actor>`, `<uniqueid>`; no artwork; UTF-8; refuse to overwrite.
- Report counts only; no file names in logs or toasts.

Value is moderate: it lets a user leave JizzCraft for Kodi/Jellyfin without losing metadata, and it is the only
vendor-neutral exit path. **Privacy**: the exported files contain the user's ratings and play counts, which are
sensitive; tell the user plainly where they are written.

### 2.7 Data-model prerequisites (shared by ranks 2, 3, 6)

All as new `MIGRATIONS` entries; nothing edits the frozen v1 SQL.

- `media_info(media_id PK, title, details, date, code, source_url)`; `source_url` only stored, never fetched.
- `studios(id, name, name_lc UNIQUE, parent_id, private)` and `media.studio_id` or a link table.
- `people(id, name, name_lc, disambiguation, favorite, rating)` + `person_aliases` + `media_people(media_id,
  person_id, position)`. **Minimise**: do not model body measurements, ethnicity, tattoos, birth/death dates or
  gender in v1. They are data about real third parties, they are "special category" candidates, and nothing in
  the viewer needs them (data minimisation, GDPR Art. 5(1)(c), see section 5).
- `label_aliases(label_id, alias_lc UNIQUE)` for tag aliases (merge on import).
- `external_ids(media_id, scheme, value)` for `stash`, `tpdb`, `imdb` and similar; stored as inert text.
- `labels.kind` has `CHECK (kind IN (1,2))` in the frozen v3 SQL, so performers and studios cannot reuse `labels`;
  they need their own tables.
- FTS: the v4 view `media_fts_src` and the FTS table are frozen. Add a new FTS table and triggers in a later
  migration; private studios, people and tags must stay out of the index (the same rule as v4).
- The earlier JizzCraft report (`docs/research/turnkey-landscape-2026-10.md`, F9/F10) said tags "do not exist yet".
  They shipped in v3 (playlists, categories, tags), so F9 is now unblocked.

### 2.8 Stash GraphQL live import (rank 7, conditional)

**Endpoint** [F, High: Stash `internal/api/server.go`: `/graphql` and `/playground`; `pkg/session/session.go`: header
`ApiKey`, also accepted as query parameter `apikey`; `Configuration.md`: "If password protection is enabled, you may
also generate an API key"]. Stash serves plain http by default on its own port.

Read-only query sketch (syntax checked against the schema, not run against a server):

```graphql
query ($page: Int!) {
  findScenes(filter: {page: $page, per_page: 100, sort: "id", direction: ASC}) {
    count
    scenes {
      id title code date details rating100 organized play_count o_counter resume_time last_played_at
      studio { name }  tags { name }  performers { name }
      scene_markers { title seconds end_seconds primary_tag { name } }
      files { path size duration fingerprints { type value } }
    }
  }
}
```

**AGENTS.md conflict**: "A source with a key must use https; redirects never leave the host", and a public source may
not point at loopback or private addresses (names on the user's own network are exempt). A home Stash on
`http://nas:9999` carries the API key in clear text, so it would need an explicit, documented exception (https
reverse proxy, or a loopback-only plain-http allowance). It also lives in `crates/fetch` behind the `network`
feature, with the key only in `fetch.json` (0600). Prefer the offline export (2.3): same data, no key, no socket.
**Privacy**: the query reveals nothing outside the user's LAN, but the API key grants write access (mutations such
as `submitStashBoxFingerprints`, `backupDatabase`); there is no read-only key scope in Stash [S, Medium], so the
importer must send only `query` documents and refuse mutations.

### 2.9 Jellyfin / Emby / Plex API import (ranks 8-9, conditional)

- **Jellyfin** [F, High: OpenAPI 12.2.0 from `repo.jellyfin.org`, 2026-10-05 release]: `GET /Items?Recursive=true&
  IncludeItemTypes=Movie,Video&Fields=Path,Overview,Genres,Tags,Studios,People,ProviderIds,PremiereDate,
  ProductionYear&StartIndex&Limit&userId=...`; per-user state in `UserData` {`Played`, `PlayCount`,
  `PlaybackPositionTicks` (1 tick = 100 ns), `IsFavorite`, `Rating`, `LastPlayedDate`}; auth scheme is an
  `Authorization` header (`MediaBrowser Token="..."` form, Medium); `/System/Info/Public` is the unauthenticated
  probe. Server paths must be mapped to local paths by a user-supplied prefix pair, and matched by size/duration.
  Caveat: no NFO is needed if the user's NFO saver was enabled, so the NFO route (2.2) is simpler and offline.
  Jellyfin's 2026 advisories about unauthenticated endpoints (cited in the earlier report) are a reason not to
  recommend that users expose it.
- **Emby**: REST under `http[s]://host:port/emby/{apipath}`, JSON or XML, user or API-key auth (dev.emby.media,
  High). Same shape as Jellyfin (its fork parent). Emby's server is closed-source since the 4.x line (Jellyfin About
  page: "Emby planned to make its 4.x release closed-source"); the `MediaBrowser/Emby` GitHub repository
  (GPL-2.0, last push 2024-03-27) holds client/API material, not the current server (Medium).
- **Plex** [F, High: developer.plex.tv/pms]: `GET /library/sections/{id}/all` with `X-Plex-Token` ("an authentication
  token, obtained from plex.tv"); fields include `userRating` (0-10), `viewCount`, `viewOffset`,
  `lastViewedAt`. An account at plex.tv is required to get a token. The token and the plex.tv dependency make Plex
  the worst fit; use the Plex NFO route if the user already exports NFO.

### 2.10 User-supplied scraper rules (rank 10, deferred)

Stash rule format [F, High: docs.stashapp.cc scraper development, footer date 2025-11-29]: a YAML with `name`; one or
more of `performerByName`, `performerByFragment`, `performerByURL`, `sceneByName`, `sceneByQueryFragment`,
`sceneByFragment`, `sceneByURL`, `groupByURL`, `galleryByFragment`, `galleryByURL`, `imageByFragment`, `imageByURL`;
action types `scrapeXPath`, `scrapeJson` (GJSON), `script` (arbitrary local program, JSON over stdin/stdout) and
`stash`; blocks `xPathScrapers`, `jsonScrapers`, `common`, `driver` (`cookies`, `headers`, `useCDP` via Chrome DevTools
Protocol, `clicks`); placeholders `{}`, `{checksum}`, `{oshash}`, `{phash}`, `{filename}`, `{title}`, `{url}`;
postProcess `javascript` (goja), `feetToCm`, `lbToKg`, `map`, `parseDate` (Go layouts), `subtractDays`, `replace`
(regex), `subScraper` (a second fetch).

What a safe subset would be: the existing declarative source model (`docs/streaming.md`: `search_url`, JSON-pointer
`fields`, `per_minute`, key in `fetch.json`) extended with a read-only "metadata source" that returns the
`media_info` fields above. Never: `script`, `javascript`, `useCDP`, cookie import (owner rule), or direct loading of
Stash YAML. The community repo (stashapp/CommunityScrapers, AGPL-3.0, 709 `.yml` and 167 `.py` files at access
date, High) is a catalogue of per-site rules: exactly the "site presets" AGENTS forbids shipping. A user could
bring their own rule in JizzCraft-native JSON; the project would never host, list or link rule files.
**Legal**: scraping a site may breach its terms of service; copying rule files from an AGPL repo may bring
AGPL obligations; results contain data about real people. Deferred until 2.1-2.5 are done.

### 2.11 Remote identification by hash (rank 11, deferred)

- **stash-box** (MIT, `v0.10.3` 2026-08-14): GraphQL query `findScenesBySceneFingerprints(fingerprints: [[FingerprintQueryInput!]!]!)`
  with `FingerprintQueryInput{hash, algorithm: MD5|OSHASH|PHASH}`; auth by session cookie or `ApiKey` header; roles
  `READ`, `VOTE`, `EDIT`; submit mutations `submitFingerprint(s)`. [S6, High]
- **StashDB**: needs an account, and registration needs an invite key; new accounts are read-only; no terms of use,
  API limits or data licence were found on the access guide (undated). **UNVERIFIED**: whether third-party apps may
  query it. [S7, Medium]
- **ThePornDB**: OpenAPI v3.24.757 at `api.theporndb.net/specs?openapi.json`; `GET /scenes/hash/{hash}?type=OSHASH|PHASH`;
  `POST /scenes/{identifier}/hash` with `{hash, type, duration}` (contribution); bearer token (`sanctum`). Its
  Jellyfin plugin says to add an API token. [S13, High for the spec; terms UNVERIFIED]

Privacy: each lookup tells a third party which exact files you own (oshash and phash identify files) and, with the
token, who you are. Submitting fingerprints is a contribution to a shared database and is telemetry by another name.
If ever built: user-supplied endpoint, user-supplied token in `fetch.json`, one explicit click per batch with a
count preview, no background lookups, no bundled endpoint, results land in a review screen first.

### 2.12 Mounted remote libraries (rank 12)

WebDAV (RFC 4918, June 2007: PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK; security section lists
authentication, lock privacy, XML entities, DoS) and SMB/NFS can already be used **without code** by mounting the
share with the operating system and adding the mount point as a library folder. JizzCraft then sees plain local
paths and `file://` URIs. Document it, with these warnings: scans over slow mounts take long, fingerprints read 128
KiB per file over the network, and the mount itself is the user's network exposure.

### 2.13 Servers and device protocols (rank 14-15, rejected)

- **DLNA/UPnP**: discovery uses SSDP multicast (UDA, OCF-hosted; UDA 2.0 revision 2020-04-17) and ContentDirectory
  over HTTP; UPnP has no authentication. The DLNA organisation dissolved in 2017 (secondary sources, Medium) and
  its guidelines stopped in 2016. Jellyfin moved DLNA out of the core in 10.9 into a first-party plugin
  (`jellyfin/jellyfin-plugin-dlna`, GPL-3.0) because the protocol is "abandoned" (forum moderator, Medium).
  Anyone on the LAN would see the library: an outing risk. Also needs a listening socket and multicast, which the
  default build must never have. UPnP specs are public but the IP rights are "subject to the UPnP bylaws and Forum
  membership agreement" (OCF), so licence terms for implementers are unclear.
- **WebDAV server**: same listener problem; RFC 4918 clients expect Basic/Digest auth, which is cleartext without TLS.
- **Jellyfin-compatible server**: 294 API paths in the 12.2.0 OpenAPI; a faithful subset is XL work; compatible clients
  would also expect images, trickplay, sessions. **Subsonic/OpenSubsonic** is a music API (the page lists
  `getVideos`, `getVideoInfo`, `hls`, `getCaptions`, but music is the focus); authentication is `u` + `t =
  md5(password+salt)` + `s` (weak), and the original Subsonic site says API access needs a licence after a
  30-day trial. Not a fit.
- If any server is ever wanted, put it in a **separate project/binary** that reads an export, never in the core
  engine, bound to loopback, with a mandatory password, and written down in AGENTS.md first. The ROADMAP's own
  "local control channel over a Unix socket, never TCP" is the right ceiling.

### 2.14 Direct reads of other apps' databases (rank 13)

Stash's schema is at version **86** (`appSchemaVersion`, `pkg/sqlite/database.go`) with 85 up-migrations; fingerprints
live in `files_fingerprints(file_id, type, fingerprint)` (migration 32). Jellyfin and Plex change their databases
across versions without compatibility promises. Reading them is fragile and bypasses the documented contracts
(export JSON, GraphQL, REST). Not recommended; use the export routes.

---

## 3. Recommended roadmap (8 small work items)

Order matters; each item is releasable alone, DB-only, offline, and keeps `engine_never_touches_media`,
`offline_deps` and `tests/network.rs` green. Fixtures are synthetic and SFW (fake titles, fake names).

| # | Work item | Size | Depends on | Done when |
|---|---|---|---|---|
| 1 | **oshash**: compute in `quick_fp` pass; `media.oshash` (migration v5); `export_meta` v3 includes it; CLI `query --oshash`; test vector checked against the Python `oshash` and Stash Go code on a synthetic file | S | none | known vectors pass; rescan backfills lazily; no extra I/O test |
| 2 | **Metadata tables**: `media_info`, `studios`, `people` (+aliases, minimal fields), `label_aliases`, `external_ids`; commands `info.set`, `person.*`, `studio.*`; new FTS table excluding private rows | M | none | migration test; discreet-mode leak test extended |
| 3 | **NFO import** (`meta.import_nfo {dir, dry_run, policy}`): bounded `quick-xml` parser, DOCTYPE rejected, merge policy, report; CLI + registry command | M | 2 | fixture NFO set (movie, musicvideo, URL-only, hostile XML) passes; tree-hash test unchanged |
| 4 | **Stash JSON import** (`meta.import_stash {dir, dry_run}`): files/scenes/performers/studios/tags/markers, match by size+oshash then size+duration; phash and md5 kept as imported fingerprints | M | 1, 2 | fixture folder generated by hand from `jsonschema/*.go`; rating-unit decision recorded after a real export is inspected |
| 5 | **Funscript read-only**: `funscript.rs` modelled on `subtitles.rs` (caps, no names in logs); timeline heatmap; derived "scripted" flag; no device output | S-M | none | hostile-file tests (huge, unsorted, negative `at`, `pos` over 100); screenshot test |
| 6 | **Perceptual hash**: `media` crate extracts 25 frames (layout above), `core` stores `phash` + version, job with cancel flag; duplicate view with Hamming 0/4/8/10 and duration filter; imported Stash phashes flagged | L | 1 (for join), 4 (for import) | equal/near/different synthetic video triples; no deletion offered, only a review list |
| 7 | **NFO export to a mirror folder** with a "refuse destination inside a library root" guard in `check_dest`; private labels excluded by default | S-M | 2, 3 | guard test; round trip import->export->import is stable |
| 8 | **Interop docs and guardrails**: `docs/interop.md` (formats supported, what is refused and why, OS-mount recipe), AGENTS.md additions (no listening sockets in any build; no vendor-cloud device sync; imports read-only and bounded; remote identification requires a user-supplied endpoint and per-click confirmation), CI check that `core` gains no network crate | S | none | rules merged before items 3-4 land |

Explicitly deferred (not in the 8): Stash GraphQL/Jellyfin/Plex live importers, user-supplied scraper rules, hash
identification against stash-box/ThePornDB, device output, any server. Revisit after item 6, and only with the owner's
decision on the https-key rule for LAN Stash.

---

## 4. Detailed findings with confidence

| # | Finding | Type | Confidence | Sources |
|---|---|---|---|---|
| F1 | Kodi, Jellyfin, Emby and Plex (NFO agent, PMS >= 1.43.1) share the Kodi NFO format; local NFO wins over online metadata in Kodi and Jellyfin | F | High 90 % (Emby: Medium 70 %) | S8, S9, S10, S11 |
| F2 | Kodi NFO tag list, naming rules, UTF-8, `premiered` replaces `year` from v20 | F | High 95 % | S8 |
| F3 | Stash exports to a folder of JSON; file fingerprints are `[{type, fingerprint}]` with raw number-or-string values; export field names as listed | F | High 90 % (unit of `rating` Medium) | S3, S5 |
| F4 | Stash oshash algorithm = OpenSubtitles hash: 64 KiB head/tail, LE u64 sum, + size, 16 hex digits | F | High 95 % | S4, S21 |
| F5 | Stash phash = 25 frames, 160 px, 5x5 sprite, goimagehash PerceptionHash (64x64 bilinear, DCT, median); distances 0/4/8/10 | F | High 92 % | S4, S5, S21 |
| F6 | JizzCraft `fp1` reads the same two 64 KiB windows as oshash | F | High 98 % (observable) | S22 |
| F7 | Stash GraphQL: `/graphql`, `ApiKey` header (or `apikey` parameter), `findScenes`, `findDuplicateScenes(distance, duration_diff)`, `submitStashBoxFingerprints` mutation | F | High 93 % | S3, S5 |
| F8 | stash-box GraphQL fingerprint lookup with MD5/OSHASH/PHASH; MIT; StashDB needs invite and account | F | High 90 % | S6, S7 |
| F9 | ThePornDB v3 API has hash lookup by OSHASH/PHASH and contribution endpoints; bearer token | F | High 92 % | S13 |
| F10 | Stash uploads funscript CSV to handyfeeling.com from the browser | F | High 95 % (source line read) | S5 |
| F11 | Funscript JSON: `actions[{at ms, pos}]`, version/inverted/range; naming `<video>.funscript`; axis suffix convention is MultiFunPlayer's | F | High for core 90 %; Medium for suffixes 70 % | S15 |
| F12 | Plex legacy agents are being removed from PMS in 2026; replacement is HTTP metadata providers (PMS 1.43.0); announced 2025-12-09 | F | High 92 % | S10 |
| F13 | PhoenixAdult Jellyfin plugin last pushed 2024-10-29 (older than 12 months); ThePornDB Jellyfin plugin pushed 2026-09-10 | F | High 95 % | S13 |
| F14 | Whisparr v3 ("Eros") source contains StashDB import lists (favorites, performers, studios) and a `011_add_tpdb` migration; v2 is Sonarr-based and studio-centric | F | Medium 75 % (README text plus file names, not behaviour) | S12 |
| F15 | Jellyfin moved DLNA to a plugin in 10.9; the reason given is that DLNA is abandoned | F/E | Medium 72 % (secondary + repo existence) | S9, S16 |
| F16 | DLNA dissolved in January 2017; UPnP Forum assets went to OCF on 2016-01-01 | F | Medium 75 % (OCF primary for 2016; DLNA date secondary) | S16 |
| F17 | Absolute Series Scanner is a series/anime scanner paired with agents like HAMA; its README says TheTVDB "do not carry porn series". It is not an adult tool. Adult libraries on Plex used separate bundles | F | High 88 % | S14, S13 |
| F18 | Stash community scrapers: 709 YAML, 167 Python; AGPL-3.0 | F | High 95 % | S23 |
| F19 | Stash DB schema version 86; Stash latest stable release v0.31.1 (2026-04-13); develop pre-release 2026-10-07 | F | High 95 % | S3 |
| F20 | Jellyfin current API 12.2.0 (release 2026-10-05); `UserItemDataDto` fields as listed | F | High 95 % | S9 |
| F21 | GDPR Art. 9(1) prohibits processing "data concerning a natural person's sex life or sexual orientation" unless an exception applies; Art. 2(2)(c) excludes purely personal or household activity | F | High 98 % | S19 |
| F22 | Subsonic API: `u`/`t`/`s` token = md5(password+salt), `.view` endpoints, video methods exist, licence needed after trial for the Subsonic server | F | High 88 % | S18 |
| F23 | Calibre hierarchical tags use dots with `=.` and `=..` search operators | F | High 90 % | S20 |
| F24 | A phash computed by GStreamer will differ from Stash's by a few bits (not equal) | S | Low-Medium 55 % | inference from S4 |

---

## 5. Legal and privacy caveats (shared)

1. **Licences** [F, High]. Stash AGPL-3.0; CommunityScrapers/CommunityScripts AGPL-3.0; Jellyfin GPL-2.0; Kodi
   GPL-2.0-or-later; Whisparr GPL-3.0; OpenFunscripter GPL-3.0; jellyfin-plugin-dlna GPL-3.0; PhoenixAdult GPL-2.0;
   stash-box MIT; goimagehash BSD-2-Clause. The ThePornDB Jellyfin plugin is MIT per its `LICENSE` file and GPL 2.0
   per its README badge (conflict, see section 7). JizzCraft is MIT OR Apache-2.0: implement formats from their
   specifications; do not copy code, scraper files or UI. File formats and short algorithms (oshash) are
   generally treated as ideas rather than protected expression, but that is a legal judgement for the owner, not a
   finding here.
2. **Special-category data**. GDPR Art. 9 covers sex-life data. The household exemption (Art. 2(2)(c); Recital 18)
   likely covers a single person's private library on their own machine, but not publication, sync, a shared
   server, or a business. Exports (NFO, Stash JSON, JizzCraft meta) are sensitive files: write with 0600
   permissions (the `private_file_options` helper exists), warn on cloud-synced folders.
3. **Third parties' data**. Performer names, aliases, dates of birth and physical attributes are personal data of
   people who did not choose to be in your database. Store the minimum (name, aliases, optional note), never
   re-publish imported performer data, and never import their images in v1.
4. **Terms of service**. StashDB: invite and account, terms not found. ThePornDB: token, terms not reviewed.
   Plex: plex.tv account. Scraping sites: terms vary. Mark all as UNVERIFIED and keep them out of the default
   build and the repo (no endpoints, no sites, no rule files).
5. **Telemetry by contribution**. `submitStashBoxFingerprints` and `POST /scenes/{id}/hash` publish what you own.
   AGENTS forbids telemetry; treat contributions as out of scope.
6. **Servers**. Local media servers have a record of injection and traversal bugs (Stash CVE-2024-32231 sort-parameter
   SQL injection, fixed in 0.26.0; Jellyfin 2026 advisories; both from the earlier report, High). JizzCraft's whitelisted
   `SortKey`/`ALLOWED_KEYS` already avoids the Stash class of bug. A listener multiplies the attack surface.
7. **Tests**. NFO and Stash fixtures must be synthetic: invented titles, `Example Performer`, `.invalid` hosts. Never
   commit or download real adult content or real exports (AGENTS).
8. **Age and consent signals**. None of the tools reviewed define a verified age/consent field for performers. Do not
   add a performer "age" field computed from imported birth dates.

---

## 6. Timeline

| Date | Event | Source |
|---|---|---|
| 1999-10 | UPnP Forum formed | S16 |
| 2007-06 | RFC 4918 (WebDAV) | S17 |
| 2016-01-01 | UPnP Forum assets assigned to OCF; DLNA guidelines end (2016 version last) | S16 |
| 2017-01 | DLNA announces dissolution (secondary) | S16 |
| 2018-12 | Jellyfin forked from Emby 3.5.2 | S9 |
| 2024-05 | Jellyfin 10.9: DLNA moved to plugin (secondary) | S9 |
| 2024-08-15 | Stash CVE-2024-32231 advisory | earlier report |
| 2025-02-07 | Kodi wiki "Import-export library/Video" last revision | S8 |
| 2025-08-04 | Kodi wiki "NFO files" last revision | S8 |
| 2025-10-31 | Kodi 21.3 "Omega" released (latest stable on GitHub; 11.5 months old) | S8 |
| 2025-11-29 | Stash scraper/plugin docs footer date | S1, S2 |
| 2025-12-09 | Plex announces custom metadata providers; legacy agents to be removed in 2026 | S10 |
| 2026-03-30 / 04-13 | Stash v0.31.0 / v0.31.1 | S3 |
| 2026-04-13 | Kodi wiki "NFO files/Movies" last revision | S8 |
| 2026-05-31 | Kodi "Add-on rules" last revision | S8 |
| 2026-08-14 | stash-box v0.10.3 | S6 |
| 2026-10-05 | Jellyfin release with API 12.2.0 | S9 |
| 2026-10-07 | Stash `latest_develop` pre-release | S3 |
| 2026-10-09 | Calibre manual 9.16.0 | S20 |
| 2026-10-14 | JizzCraft earlier landscape report | S24 |
| 2026-10-15 | JizzCraft v0.2.0 published; project paused | S22 |

---

## 7. Contradictions and discrepancies

1. **Funscript position range.** buttplug.io says `pos` is "an integer from 0 to 99"; OpenFunscripter writes `range:
   100` and clamps `pos` to 0..100. Resolution: accept 0..100, clamp, never reject. (High that both exist.)
2. **Stash export folders.** `JSONSpec.md` lists seven folders (no `tags`); `jsonschema/tag.go` exists. Resolution:
   treat `tags/` as optional and read tag names from scenes if absent; verify with a real export. (Medium.)
3. **Stash rating unit.** GraphQL `rating100` (1-100) versus export field `rating` (int) with the docs saying only
   "rating (integer)". Resolution: do not choose a divisor until a real export is inspected. (Medium.)
4. **ThePornDB Jellyfin plugin licence.** `LICENSE` file is MIT (2021); README badge says GPL 2.0. GitHub's API
   reports MIT. Resolution: irrelevant if no code is copied; note as an unresolved metadata inconsistency.
5. **Stash version.** README mentions macOS 12 required for "version 0.32.0" while the latest published stable is
   v0.31.1; 0.32.0 is the upcoming `develop` line. Resolution: cite `develop` schema as 2026-10-16 snapshot and
   re-verify before implementation.
6. **Whisparr README vs behaviour.** The v2 README calls it a "movie collection manager"; v3 README line says
   "focuses on scenes". Resolution: treat features as per branch; Whisparr is not an integration target.
7. **Plex and legacy agents.** PhoenixAdult/ThePornDB bundles are legacy agents; Plex says legacy agents leave PMS in
   2026, a recent PMS hides them but allows re-enabling (forum, Feb 2026). The ecosystem is migrating; do not
   build anything on the bundles. (Medium.)
8. **Kodi NFO `year`.** Wiki table says deprecated in v17 and "use premiered only" from v20. Importers should still
   read `year` when `premiered` is absent. (High.)

---

## 8. Knowledge gaps

- No Stash, Jellyfin, Plex or Emby server was run. Queries, field names and units come from schemas and source,
  not from live responses. A real Stash export (synthetic data) should be produced once to settle contradictions 2
  and 3.
- StashDB terms of use, rate limits, data licence: not found. ThePornDB terms: not reviewed.
- Emby's current official NFO article could not be fetched (404); evidence is a forum statement and the REST docs.
- Jellyfin `Authorization` header exact format (`MediaBrowser Token="..."`) is from the OpenAPI scheme name and
  background knowledge; not exercised.
- Whisparr v3's real metadata behaviour (how it uses StashDB and TPDB) rests on source file names and README text.
- Kodi's JSON-RPC, UPnP server and DLNA support in Plex and Emby were not verified.
- Kodi "adult add-ons": the Add-on rules page has no adult-specific clause (searched for "adult", "porn",
  "sexual"; Medium), and their per-site behaviour is the pattern AGENTS forbids; no add-on was examined.
- Cross-decoder phash drift (GStreamer vs ffmpeg) is an inference; needs an experiment on synthetic clips.
- quick-xml hostile-input behaviour needs a test in this repository.
- Legal analysis of format/algorithm reuse and of scraping is not done by a lawyer.

---

## 9. Recommendations (summary)

1. Add oshash next to `fp1` (item 1). Highest value per hour; unlocks every join with Stash/TPDB/StashDB.
2. Build the metadata tables with minimal performer fields and tag aliases (item 2), then NFO import (item 3) and Stash
   JSON import (item 4): the two formats cover Kodi, Jellyfin, Emby, Plex-NFO and Stash users.
3. Read funscript sidecars and draw the heatmap; no device output, no vendor cloud (item 5).
4. Implement a JizzCraft-native phash with Stash's layout and thresholds; import Stash values but compare by distance
   (item 6).
5. Offer NFO export only to a folder outside the library, with a guard test (item 7).
6. Write down the refusals in AGENTS.md/docs before shipping imports (item 8): no servers or listeners in any build, no
   cloud device sync, no default remote identification, no bundled rules.
7. Defer live importers (Stash GraphQL, Jellyfin, Plex), user scraper rules and hash identification until the owner
   decides on the https-key-for-LAN rule and on contribution/telemetry policy.

---

## 10. Source quality matrix

Conflict-of-interest column: "self" = project describing itself; "vendor" = commercial interest; "ecosystem" = the
author benefits from adoption.

| ID | Source | Type | Date (published / last change / accessed 2026-10-16) | Credibility | Conflict of interest |
|---|---|---|---|---|---|
| S1 | docs.stashapp.cc scraper development | Primary, project docs | footer 2025-11-29 | High | self |
| S2 | docs.stashapp.cc plugins | Primary | 2025-11-29 | High | self |
| S3 | github.com/stashapp/stash `graphql/schema/*`, `pkg/models/jsonschema/*`, repo metadata | Primary, source | `develop` snapshot; v0.31.1 2026-04-13 | High | self |
| S4 | stash `pkg/hash/oshash`, `pkg/hash/videophash`, `pkg/utils/phash.go`, migrations | Primary, source | `develop` snapshot | High | self |
| S5 | stash manual sources: `JSONSpec.md`, `Interactive.md`, `Deduplication.md`, `Configuration.md`; `interactive.ts`; `session.go`; `server.go`; `SceneDuplicateChecker.tsx` | Primary | `develop` snapshot | High | self |
| S6 | github.com/stashapp/stash-box README + `graphql/schema/*` | Primary | v0.10.3 2026-08-14 | High | self |
| S7 | guidelines.stashdb.org "Accessing StashDB" | Primary, operator | undated | Medium | self (gatekeeper) |
| S8 | kodi.wiki: NFO files, NFO files/Movies, NFO files/Music videos, Import-export library/Video, Add-on rules; `xbmc/xbmc` LICENSE and release | Primary | 2025-02-07 to 2026-05-31; Kodi 21.3 2025-10-31 | High | self |
| S9 | jellyfin.org docs (NFO, About, Plugins); Jellyfin OpenAPI 12.2.0 (`repo.jellyfin.org`); `jellyfin/jellyfin` release; `jellyfin-plugin-dlna` | Primary | 2026-10-05 (release); docs undated | High | self |
| S10 | Plex forum announcement (Discourse JSON); support.plex.tv NFO article and Local Media Assets (modified 2025-11-19); developer.plex.tv/pms | Primary, vendor | 2025-12-09 / 2025-11-19 / undated | High | vendor (plex.tv account lock-in) |
| S11 | dev.emby.media REST docs; `MediaBrowser/Emby` repo; Emby forum threads | Primary + secondary | repo push 2024-03-27; forum undated | Medium | vendor |
| S12 | `Whisparr/Whisparr` README, releases; `Whisparr/Whisparr-Eros` tree; wiki.servarr.com | Primary (README, source) | releases 2026-09-19; push 2026-10-16 | Medium | self |
| S13 | `api.theporndb.net/specs` OpenAPI v3.24.757; `ThePornDatabase/Jellyfin.Plugin.ThePornDB`; `ThePornDB.bundle`; `DirtyRacer1337/Jellyfin.Plugin.PhoenixAdult` | Primary, source | plugin push 2026-09-10; bundle 2025-12-12; PhoenixAdult 2024-10-29 (stale) | Medium-High | self (service operator) |
| S14 | `ZeroQI/Absolute-Series-Scanner` README | Primary | push 2026-10-02 | Medium | self |
| S15 | buttplug.io funscript page; `Yoooi0/MultiFunPlayer` README; `OpenFunscripter/OFS` source; crates.io `funscript` | Primary + community | 2026; OFS push 2023-06-23 (stale); crate 2023-02-21 (stale) | Medium-High | ecosystem (Nonpolynomial Labs) |
| S16 | openconnectivity.org UPnP page; web search results on DLNA dissolution (Wikipedia mirrors, consortiuminfo) | Primary (OCF) + secondary | OCF page rev 2020-04-17; DLNA 2017 | Medium | ecosystem |
| S17 | RFC 4918 (datatracker) | Primary, standard | 2007-06 | High | none |
| S18 | subsonic.org API; opensubsonic.netlify.app | Primary | OpenSubsonic modified 2026-10-08; Subsonic undated | High | vendor (Subsonic AS licence) |
| S19 | EUR-Lex GDPR (CELEX 32016R0679) | Primary, law | 2016-04-27 | High | none |
| S20 | manual.calibre-ebook.com 9.16.0 | Primary | 2026-10-09 | High | self |
| S21 | `corona10/goimagehash` source (BSD-2, push 2024-01-21, stale but stable); PyPI `oshash` 0.1.1 (2020-03-16); crates.io `oshash` 0.4.0 | Primary + community | as stated | Medium-High | none |
| S22 | JizzCraft `AGENTS.md`, `ROADMAP.md`, `docs/streaming.md`, `docs/organise.md`, `crates/core/src/*.rs` | Primary (internal) | commit `a083af2`, 2026-10-16 | High | owner |
| S23 | `stashapp/CommunityScrapers` repo tree | Primary | push 2026-10-15 | High | self |
| S24 | JizzCraft `docs/research/turnkey-landscape-2026-10.md` | Internal prior research | 2026-10-14 | Medium | owner |

Sources older than 12 months, flagged: PhoenixAdult Jellyfin plugin and bundle (2024), OpenFunscripter (2023),
Rust `funscript` crate (2023), `goimagehash` (2024), RFC 4918 (2007; stable), DLNA material (2017; stable), Emby GitHub
repo (2024). The Stash docs footer dates (2025-11-29) are 10.5 months old.

---

## 11. Full bibliography

All accessed 2026-10-16.

1. Stash docs, scraper development. https://docs.stashapp.cc/in-app-manual/scraping/scraperdevelopment/ (footer 2025-11-29)
2. Stash docs, plugins. https://docs.stashapp.cc/in-app-manual/plugins/ (2025-11-29)
3. stashapp/stash, GraphQL schema. https://github.com/stashapp/stash/tree/develop/graphql/schema (files `schema.graphql`, `types/{file,scene,performer,tag,studio,scene-marker,scraper,filters}.graphql`); `pkg/models/jsonschema/{scene,performer,file_folder,studio,tag}.go`; releases API (v0.31.1 2026-04-13)
4. stashapp/stash hashing: https://github.com/stashapp/stash/blob/develop/pkg/hash/oshash/oshash.go ; .../pkg/hash/videophash/phash.go ; .../pkg/utils/phash.go ; .../pkg/sqlite/migrations/{20_phash,32_files}.up.sql ; .../pkg/sqlite/database.go (schema 86)
5. stashapp/stash manual and UI sources: `ui/v2.5/src/docs/en/Manual/{JSONSpec,Interactive,Deduplication,Configuration,Tasks,Identify}.md`; `ui/v2.5/src/hooks/Interactive/interactive.ts`; `pkg/session/session.go`; `internal/api/server.go`; `ui/v2.5/src/components/SceneDuplicateChecker/SceneDuplicateChecker.tsx`; `pkg/file/video/funscript.go`
6. stashapp/stash-box. https://github.com/stashapp/stash-box (README; `graphql/schema/{schema,types/scene,types/tag,types/performer}.graphql`; v0.10.3 2026-08-14)
7. Accessing StashDB. https://guidelines.stashdb.org/docs/faq_getting-started/stashdb/accessing-stashdb/ (undated)
8. Kodi wiki. https://kodi.wiki/view/NFO_files ; .../NFO_files/Movies (rev 2026-04-13) ; .../NFO_files/Music_videos ; .../Import-export_library/Video (rev 2025-02-07) ; .../Add-on_rules (rev 2026-05-31) ; https://github.com/xbmc/xbmc (LICENSE, release 21.3-Omega 2025-10-31)
9. Jellyfin. https://jellyfin.org/docs/general/server/metadata/nfo/ ; https://jellyfin.org/docs/general/about ; https://jellyfin.org/docs/general/server/plugins/ ; https://repo.jellyfin.org/files/openapi/jellyfin-openapi-stable.json (12.2.0) ; https://github.com/jellyfin/jellyfin (release v12.2, 2026-10-05) ; https://github.com/jellyfin/jellyfin-plugin-dlna
10. Plex. https://forums.plex.tv/t/announcement-custom-metadata-providers/934384 (2025-12-09) ; https://support.plex.tv/articles/using-nfo-metadata-files-with-plex/ ; https://support.plex.tv/articles/200220677-local-media-assets-movies/ (modified 2025-11-19) ; https://support.plex.tv/articles/200241558-agents/ ; https://developer.plex.tv/pms/
11. Emby. https://dev.emby.media/doc/restapi/index.html ; https://github.com/MediaBrowser/Emby ; Emby forum threads (secondary), e.g. https://emby.media/community/topic/56334-metadatalibrary-emby-kodi-compatibility
12. Whisparr. https://github.com/Whisparr/Whisparr ; https://github.com/Whisparr/Whisparr-Eros (tree `src/NzbDrone.Core/ImportLists/StashDB/*`, `Datastore/Migration/011_add_tpdb.cs`) ; https://wiki.servarr.com/whisparr ; https://whisparr.com/docs/api/
13. ThePornDB and adult metadata plugins. https://api.theporndb.net/docs ; https://api.theporndb.net/specs?openapi.json ; https://github.com/ThePornDatabase/Jellyfin.Plugin.ThePornDB ; https://github.com/ThePornDatabase/ThePornDB.bundle ; https://github.com/DirtyRacer1337/Jellyfin.Plugin.PhoenixAdult ; https://github.com/DirtyRacer1337/PhoenixAdult.bundle
14. ZeroQI Absolute Series Scanner. https://github.com/ZeroQI/Absolute-Series-Scanner
15. Funscript. https://buttplug.io/docs/stpihkal/video-encoding-formats/funscript ; https://github.com/Yoooi0/MultiFunPlayer ; https://github.com/OpenFunscripter/OFS (`OFS-lib/Funscript/Funscript.cpp`) ; https://crates.io/crates/funscript
16. UPnP and DLNA. https://openconnectivity.org/developer/specifications/upnp-resources/upnp/ ; DLNA dissolution (secondary): https://en.wikipedia.org/wiki/DLNA ; https://consortiuminfo.org/?p=4137
17. RFC 4918. https://datatracker.ietf.org/doc/html/rfc4918
18. Subsonic and OpenSubsonic. https://www.subsonic.org/pages/api.jsp ; https://opensubsonic.netlify.app/docs/
19. GDPR. https://eur-lex.europa.eu/legal-content/EN/TXT/HTML/?uri=CELEX:32016R0679
20. Calibre manual 9.16.0. https://manual.calibre-ebook.com/gui.html ; https://manual.calibre-ebook.com/sub_groups.html
21. Hash implementations. https://github.com/corona10/goimagehash (`hashcompute.go`) ; https://pypi.org/project/oshash/ ; https://crates.io/crates/oshash
22. JizzCraft internal: `AGENTS.md`, `ROADMAP.md`, `docs/streaming.md`, `docs/organise.md`, `crates/core/src/{db,fingerprint,portability,subtitles}.rs`
23. https://github.com/stashapp/CommunityScrapers (tree: 709 `.yml`, 167 `.py`; AGPL-3.0)
24. JizzCraft `docs/research/turnkey-landscape-2026-10.md`

## Method limits

Searches used: the web-search tool (US-only, summarised results) and direct fetches of primary files through
`curl` and the fetch tool. Summaries produced by the fetch tool were re-checked against raw source for the
claims that carry weight (oshash, phash, JSON schema, GraphQL fields, Kodi tags, Funscript, Plex announcement,
ThePornDB OpenAPI, Jellyfin OpenAPI). Items marked Medium or UNVERIFIED rest on a single document, a forum, or
inference. Downloaded files were treated as data only and parsed, not executed.
