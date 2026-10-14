# Making JizzCraft nearly turnkey: what similar projects teach

Research date: 2026-10-14. Scope: the owner asked how to make JizzCraft "presque clé en main" (almost turnkey)
without breaking its hard rules: no bundled site, indexer, catalog, tracker or preset; a default build with no
network; no telemetry; keys only in a 0600 file; a player that never takes a URL. This report is descriptive and SFW.
It is not legal advice.

How to read the confidence levels:

- **High (85-100 %)**: read in a primary source (official docs, source code, spec, advisory database), and either
  checked against a second independent source or directly observable (code, API output).
- **Medium (60-84 %)**: one primary source, or several secondary ones that agree.
- **Low (<60 %)**: one secondary source, or an inference. Treat these as **unverified**.

Repository facts about JizzCraft come from reading `README.md`, `AGENTS.md`, `docs/streaming.md`, `ROADMAP.md`,
`CHANGELOG.md` and `crates/fetch/src` at commit `ee4a700`. The working tree was being changed by another session while
this was written (the app ID is being renamed from `org.jizzcraft.JizzCraft` to `io.github.kerstz.JizzCraft`).

---

## TL;DR

1. Projects that feel "turnkey" while shipping no content (Mihon, Jellyfin, Stash, Kodi, Stremio) all have three
   things: a **first-run wizard**, **user-added repositories that ship empty or official-only**, and **detection of what
   is already installed**. The ones that went wrong either let the app become a pirate-content distribution channel
   (Kodi third-party repos, Popcorn Time, Tachiyomi's bundled extensions) or ran unsigned code from repos (Kodi
   cryptominer add-ons, 2018).
2. JizzCraft already has the hard parts (doctor, Jackett/Prowlarr key detection, proxy kill switch, sandboxed resolvers,
   HLS/DASH in `fetch`). What is missing for "turnkey" is mostly **glue and UX**: a first-run flow, a "Connections"
   health page, detection of more tools (yt-dlp, mpv, qBittorrent, Transmission), **signed, hash-pinned repositories
   the user adds by URL**, standard-format import (Kodi NFO, Stash JSON export), and clearer behaviour on slow magnets.
3. Concrete gaps found in the code: magnet metadata listing has **no timeout or peer-count feedback**
   (`service/torrent.rs::list_prepared`); auto quality only **steps down** (no up-switch with hysteresis); Torznab
   **seeding requirements** (`minimumratio`, `minimumseedtime`) are ignored while seeding is off by default; librqbit's
   **`bind_device_name`** (bind to a VPN interface) is not exposed.
4. Do not copy: opt-out analytics (Prowlarr), automatic add-on updates from any repo (Kodi), arbitrary-execution
   installer scripts (Lutris), unauthenticated local web servers (Jellyfin and Stash advisories), name-based tagging that
   renames files (TagSpaces), UPnP on by default (libtorrent).
5. Distribution warning: Flathub's requirements (updated 2026-09-21) **ban AI-generated or AI-assisted content in Flathub
   manifests**. This project's commits are AI co-authored, so the Flatpak manifest needs a human-written path, or
   distribution should go elsewhere.

---

## Executive summary

"Turnkey" in this landscape means the user goes from install to first playback without reading docs, and the app
recovers by itself when something breaks. The best examples:

- **Stash** has a setup wizard that asks for the database, generated-content, cache and blobs locations, then
  downloads ffmpeg itself. It ships one default package source for scrapers and plugins, maintained by the project, and
  each package carries a SHA-256.
- **Jellyfin** lets you add a plugin repository by manifest URL. Local NFO metadata always takes precedence over
  online providers.
- **Mihon** ships **no repositories at all**. It trusts by repository signing key: since v0.21.0 (2026-10-12) it
  rejects extensions not signed with their repo's key. It also has content-warning levels (Safe / Mixed / 18+).
- **Prowlarr** turns "configure indexers in five apps" into "configure them once", with Full Sync or Add-and-Remove.

The failures repeat:

- Content-bearing defaults attract legal pressure (Tachiyomi, January 2024; Popcorn Time).
- Unsigned repositories become malware channels (Kodi, 2018).
- Scrapers and extractors rot as sites change (yt-dlp's most-commented issues are site breakages; Jackett's is
  Cloudflare).
- Local servers grow injection bugs (Jellyfin's 2026 ffmpeg-argument-injection and path-traversal advisories; Stash's
  2024 SQL injection through a sort parameter).

JizzCraft's design already avoids most of these by construction. That is unusual and worth keeping. The roadmap below
favours **detection, wizards, diagnostics and signed user-added repositories** over any new network surface.

---

## 1. Per-project findings

### 1.1 Stash (stashapp/stash): adult library organizer

| Aspect | Finding | Confidence |
|---|---|---|
| Onboarding | Web setup wizard (`ui/v2.5/src/components/Setup/Setup.tsx`): welcome, then paths for the database, generated content, cache and blobs (blobs can live in the DB), then confirmation. A job downloads ffmpeg/ffprobe automatically (`internal/manager/task/download_ffmpeg.go`). "First steps" docs: add library directories, choose scan options, scan. | High (source code + docs, 2024-11-14) |
| Extension model | Scrapers (YAML; fragment, search and URL types) and plugins (tasks, post-operation hooks, UI CSS/JS) installed from **package sources**: YAML indexes with id, version, date, dependencies, zip path and **sha256**. The default source is "Community (stable)" at `stashapp.github.io/CommunityScripts/stable/index.yml`, maintained by the stashapp org. Manual install means dropping YAML in `scrapers/` or `plugins/` and clicking Reload. | High (docs.stashapp.cc scraping and plugins pages, updated 2025-11-29) |
| Metadata | Built-in Auto Tag works from filenames. "Identify" task and Tagger match against stash-box instances, with optional fingerprint submission (phash, oshash, md5, duration). The JSON export carries per-file `fingerprints` (type plus value), size, mod_time, codec, width/height (`pkg/models/jsonschema/file_folder.go`). | High (docs + source) |
| Updates | No auto-update. `internal/api/check_version.go` queries `api.github.com` for the latest release (it honours the proxy from the environment). Whether this runs automatically was not verified. | Medium |
| Packaging | Single binary for Windows, macOS, Linux and FreeBSD, plus Docker. Latest v0.31.1 (2026-04-13, GitHub API). | High |
| Complaints | Most-commented issues: hardware encoding (79 comments), the files-refactor regressions, audio support, confusing naming of the tagging/scraper components (42 comments, open), multi-scene files. | Medium (GitHub search, 2026-10-14) |
| Security | **CVE-2024-32231 / GHSA-75jf-52jg-qqh4**: SQL injection via the *sort* parameter, critical, fixed in 0.26.0 (published 2024-08-15). A built-in **security tripwire** refuses public-internet requests when no password is set (docs page "authentication required when accessing stash from the internet"). | High (GitHub Advisory DB + Stash docs) |

Lessons for JizzCraft:

- JizzCraft's whitelisted `SortKey` is exactly the fix to Stash's CVE. Keep it.
- A wizard with a "where do things go" step plus a "fetch the missing tool" step is the bar users expect. JizzCraft
  must not download tools itself, but it can detect them and print the distro command, as `doctor` already does.
- Repository indexes with per-package SHA-256 are a cheap, proven format.

### 1.2 Stremio: addon protocol

| Aspect | Finding | Confidence |
|---|---|---|
| Extension model | An addon is an HTTP(S) endpoint serving `/manifest.json` and `/{resource}/{type}/{id}.json` for `catalog`, `meta`, `stream` and `subtitles`. Users install by **transport URL**. Stream objects can carry `url`, `ytId`, `infoHash` + `fileIdx` ("if fileIdx is not specified, the largest file will be selected"), `externalUrl`, `sources` (tracker or DHT hints) and `behaviorHints` (`notWebReady`, `bingeGroup`, `proxyHeaders`, `videoHash`, `filename`). | High (stremio-addon-sdk docs; protocol.md commit 2026-09-13, stream.md 2025-11-27) |
| Distribution of addons | Authors self-host. The SDK has `publishToCentral` to list an addon in the in-app "Community addons" catalog. | High (SDK deploying docs) |
| Legal posture | A secondary source says Stremio lists Vodo as its only official BitTorrent addon and that community addons are not operated by Stremio. Not confirmed from a Stremio primary source. | **Low** |
| Security | Check Point's "Hacked in Translation" (2017-05-23): crafted subtitle files led to code execution in VLC, Kodi, Popcorn Time and **Stremio**; patched afterwards. | High (Check Point blog + Infosecurity Magazine + Security Affairs) |
| Complaints | Not researched in depth. | n/a |

Lessons:

- "Paste a manifest URL, get a catalog" is the turnkey gesture users know. JizzCraft's declarative JSON sources are a
  safer subset, with no remote code. A **source pack fetched from a user-supplied URL** (see feature F7) gives the same
  gesture.
- "Largest file by default" is a good default, and JizzCraft's `pick.rs` already follows it.
- Subtitles are an attack surface. When subtitle support lands, parse only in-process with bounded parsers and never
  fetch subtitles automatically.

### 1.3 Kodi: add-ons, repositories, resolvers

| Aspect | Finding | Confidence |
|---|---|---|
| Repositories | The official repository is included and enabled by default. Third-party repos are add-ons (`xbmc.addon.repository`) pointing to `addons.xml` plus a "checksum" URL. The wiki says the checksum "is not verified and not required to be a checksum". There is **no signature**: the only advice is to use HTTPS, and Kodi logs a warning for non-HTTPS repos since Leia. An optional `hashes` element (sha256/sha512) only avoids re-downloads. | High (kodi.wiki "Add-on repositories", edited 2023-02-14) |
| Unknown sources | Off by default: "The only trusted Repository is the Kodi Official Repository". You must enable it to install from zip. Updates default to "Install updates automatically". | High (kodi.wiki "Settings/System/Add-ons", edited 2024-05-02, v21) |
| Metadata | NFO files are imported first. If none exist, the configured scraper runs, or nothing when "Local info only" is set. | High (kodi.wiki "NFO files", edited 2025-08-04) |
| Legal posture | Officially neutral. The team does not block add-ons because "they would instantly change the addon", but campaigns against "fully loaded box" sellers and refuses support for piracy add-ons. | Medium (kodi.tv article + TorrentFreak 2019-09-26; coverage mostly 2015-2019, **older than 12 months**) |
| Security incidents | **ESET, 2018-09-13**: cryptominers distributed through third-party repos Bubbles, Gaia and XvBMC, spread further by other repos' update routines and prebuilt "builds"; about 4,774 victims. The **2023 forum breach**: about 400k MyBB records (emails, IPs, salted hashes, private messages) after a dormant staff account was abused. | High (ESET blog + SiliconANGLE/BetaNews; The Hacker News + Have I Been Pwned) |
| Release | 21.3 "Omega" (2025-10-31, GitHub API). | High |

Lessons:

- The auto-update default combined with unsigned third-party repos is how malware spread. **Pin a signing key per
  repository and never auto-install updates from a repo the user did not explicitly trust.**
- NFO precedence ("local first, online optional") is the right shape for JizzCraft metadata.

### 1.4 Jellyfin and Plex: library, metadata agents, transcoding

| Aspect | Finding | Confidence |
|---|---|---|
| Plugins | Catalog from the official repos (`repo.jellyfin.org/files/plugin/manifest.json`, plus an unstable channel). Third-party repos are added by manifest URL and listed separately as "Third Party". | High (jellyfin.org docs) |
| Metadata | Local NFO is "always fetched and can't be disabled" and wins over remote providers such as TMDb. The optional NFO saver writes NFOs back. | High (jellyfin.org NFO docs) |
| Release | v12.2 (2026-10-05, GitHub API). | High |
| Security | Advisories in 2026: **CVE-2026-35033** (2026-04-14), unauthenticated SSRF plus arbitrary file read through **ffmpeg argument injection** in stream options. **CVE-2026-48793** (2026-06-10), ffmpeg argument injection via a subtitle **file path containing quotes**. **CVE-2026-49246**, MKV attachment `filename` tag leading to path traversal. Further advisories 2026-09-22: SVG SSRF and path traversal in virtual-folder endpoints. | High (GitHub security advisories, jellyfin/jellyfin) |
| Plex | Account-database breach disclosed September 2025 (emails, usernames, hashed passwords; password reset urged). A prior incident in 2022. | Medium (BetaNews 2025-09-09, AlternativeTo, Slashdot) |

Lessons:

- Jellyfin's 2026 CVEs validate JizzCraft's rules: argv only, never a shell; filenames are untrusted; unsafe torrent
  paths are refused. **Extend the same rule to container-embedded names** (MKV attachments, subtitle track names) when
  subtitles and attachments are added.
- No accounts means no account breach. Plex's incidents are an argument for the "no account, ever" rule.

### 1.5 mpv + yt-dlp: the resolver model

| Aspect | Finding | Confidence |
|---|---|---|
| Model | mpv's `--ytdl=yes` (default) runs a hook script that calls youtube-dl/yt-dlp on URLs mpv cannot open. Script options include `try_ytdl_first` and `exclude`. `--ytdl-format` defaults to `bestvideo*+bestaudio/bestvideo+bestaudio/best`. | High (mpv DOCS/man/options.rst, master) |
| Proxy pitfall | mpv docs: a proxy passed via `--ytdl-raw-options` "SOCKS proxies aren't supported and https URLs also bypass the proxy. This is a limitation in FFmpeg." | High (mpv docs) |
| Updates | yt-dlp has channels `stable`, `nightly` (**recommended for regular users**) and `master`; `-U` / `--update-to`. It warns "there is no verification done for binaries from different repositories". It now needs a JavaScript runtime (deno recommended) plus `yt-dlp-ejs` for some sites. Latest stable 2026.08.19. | High (yt-dlp README, master + GitHub API) |
| Complaints | Most-commented issues are site breakages (403s, signature extraction, site-specific failures). | High (GitHub search) |

Lessons:

- Resolvers rot. The turnkey move is not to bundle one but to **detect** an installed yt-dlp, show its version and age,
  offer a one-click "register as resolver" with the right argv, and surface "your resolver is N months old; `yt-dlp -U`
  or your package manager may fix this" when it fails. JizzCraft already parses `http_headers` and `protocol` from
  yt-dlp JSON (`resolver.rs`).
- mpv's proxy caveat is a leak class. JizzCraft's "a resolver runs under `require_proxy` only if `honours_proxy`" is
  the correct posture. Keep it explicit in the UI.

### 1.6 Prowlarr, Jackett and the *arr apps (Sonarr, Radarr, Whisparr)

| Aspect | Finding | Confidence |
|---|---|---|
| Ports and keys | Jackett on `127.0.0.1:9117` (README). Prowlarr on 9696 (Servarr wiki FAQ). JizzCraft already detects both and reads their keys from their config files (`crates/fetch/src/detect.rs`). | High |
| App sync | Prowlarr pushes indexers to apps: **Add and Remove Only** or **Full Sync** (overrides settings in the app). Sync profiles. Indexer proxies (HTTP, SOCKS, FlareSolverr; FlareSolverr used only with matching tags). | High (Servarr wiki `prowlarr/settings.md`, commit 2026-09-05) |
| Telemetry | **Analytics on by default**: `GetValueBoolean("AnalyticsEnabled", true, …)` in `ConfigFileProvider.cs`. Sends browser, pages used, errors, OS and runtime version. Auto-update via branches; Windows always auto-updates. | High (source + wiki) |
| Complaints | #2635 (2026-03-20): too-short delays between queries "unintentionally DDOS indexers"; indexers get disabled with growing backoff. A PR (#2650) adds user-set per-indexer rate limits, with a 2 s floor. Jackett's most-commented issue is Cloudflare bypass (FlareSolverr, 240 comments). | High (GitHub issues) |
| Categories | Torznab/Newznab category 6000 "XXX" (subcategories 6010-6040+) in both Jackett and Prowlarr source. | High |
| Seeding requirements | Torznab 1.3 defines `seedtype`, `minimumratio` and `minimumseedtime` attributes per result. | High (Torznab spec 1.3 draft) |
| Whisparr | Adult-focused Radarr fork. v2.2.0-release.231 (2026-08-27). | High (README + API) |

Lessons:

- Detection is half done. Add **"import all indexers from Prowlarr"** (enumerate `/api/v1/indexer`, create one Torznab
  endpoint per indexer, store keys in `fetch.json`), keeping Add-and-Remove semantics so the user's local edits
  survive.
- JizzCraft's `per_minute` limit on sources matches the community's #1 complaint. Apply the same floor to Torznab
  queries, with backoff on 429/503 and `Retry-After` (FFmpeg's `respect_retry_after` does the same).
- **Seeding off by default conflicts with private trackers** that publish `minimumratio` / `minimumseedtime`. Read
  those attributes and warn, or offer per-indexer "seed until the requirement is met". Otherwise turnkey becomes
  "turnkey into a ratio ban".

### 1.7 Torrent streaming apps: qBittorrent, Transmission, WebTorrent Desktop, Peerflix, Popcorn Time, rqbit

| Project | Finding | Confidence |
|---|---|---|
| qBittorrent | WebUI API v2: `/api/v2/auth/login` (cookie, `Referer` checked), `/api/v2/torrents/add` with `sequentialDownload` and `firstLastPiecePrio`. 5.2.4 (2026-09-28). Forum advice for VPNs: bind to the VPN interface in Advanced > Network interface. | High (wiki WebUI-API 5.0; API) / Medium (forum) |
| Transmission | RPC at `http://host:9091/transmission/rpc`; `X-Transmission-Session-Id` handshake (409 then retry). Now has `sequential_download` and `sequential_download_from_piece`. | High (docs/rpc-spec.md, main) |
| WebTorrent Desktop | Last release v0.24.0 (**2020-08-29**). Repo still receives pushes. Oldest open asks include speed limits, AC3/DTS audio and an AppImage. | High (GitHub API + issues) |
| Peerflix | Streams the **largest file** over a local HTTP server; `--vlc` / `--mplayer` hand-off; `--connection N`. | High (README) |
| Popcorn Time | Shut down in 2014, revived by forks, closed again in January 2022 ("lack of use" per the team; competition per TorrentFreak). Affected by the 2017 subtitle exploit. | Medium (Engadget/TechCrunch, DigitalTVEurope 2022-01-06; **>12 months old**) |
| rqbit / librqbit 9.0.1 (2026-08-20) | Streaming with piece prioritization and HTTP Range seeking. **32 MiB per-stream lookahead** (`PER_STREAM_BUF_DEFAULT`). Streams are interleaved fairly. `ListenerOptions::enable_upnp_port_forwarding` defaults to **false**. SOCKS5 for outgoing TCP. **`bind_device_name`** (SO_BINDTODEVICE on Linux) for DHT, BT-UDP, BT-TCP, trackers and LSD. `blocklist_url` / `allowlist_url`. `disable_upload` behind a cargo feature. `client_name_and_version`. | High (crate source in `~/.cargo/registry`) |
| rqbit issues | #525 (2026-01-08, open): memory and FD exhaustion; the session "keeps accepting connections from peers even if the torrent is completed and … in no_upload mode". #306 (2025-01): memory creep when wrapping `add_torrent(list_only)` in a timeout at scale. 2024: "No way to restrict uploading to peers" (closed). | High (GitHub issues) |

Lessons: see section 2.

### 1.8 Mihon (Tachiyomi successor): extension repositories

| Aspect | Finding | Confidence |
|---|---|---|
| Posture | "does not have, associate, or provide any repositories or extensions". README: "this application hosts zero content". Warns that third-party extensions get full app access and "may contain malware". | High (mihon.app FAQ + README) |
| Repo format and trust | Repos publish `repo.json` with `meta.name`, `shortName`, `website` and **`signingKeyFingerprint`** (`NetworkLegacyExtensionRepo.kt`). Since v0.17.0 (2024-10-26): "Extension trust system", trust by repo, and a confirmation dialog when adding a repo via URI. Since **v0.21.0 (2026-10-12)**: "Install and update extensions only from a repo whose signing key matches the extension, and reject downloaded extensions not signed with their repo's key". The NSFW toggle was replaced by **allowed content warnings (Safe, Mixed, 18+)**. | High (source + CHANGELOG) |
| History | Tachiyomi: Kakao's representatives contacted contributors on 2024-01-02. Preloaded extensions were removed from builds released 6-8 January. Development ended on 2024-01-13. Sony DMCA notice removed about 200 Aniyomi extensions (June 2024). | Medium (TorrentFreak, AnimeCorner, Wikipedia; primary Tachiyomi post now redirects to GitHub) |

Lessons: this is the closest model to what JizzCraft needs. **The app ships an empty repository list; the user adds a
repo URL; the repo is pinned to a signing key on first add (TOFU with explicit confirmation); every item must be signed
by that key.** The Tachiyomi history shows why the app itself must never preload one.

### 1.9 Hydrus Network, digiKam, Czkawka, TagSpaces: tagging and dedupe

| Project | Finding | Confidence |
|---|---|---|
| Hydrus | "Hydrus does not auto-update" and "the program never connects to another computer unless you tell it to". Strong backup culture ("back up before every update"; "do not put your live database in a folder that continuously syncs to a cloud backup"). Duplicates: 64-bit perceptual hash, Hamming "search distance" (start at 0; "usually isn't much useful beyond 8"), with decisions better / same / alternates / not-related, where not-related is remembered. **Video dedupe is not built in** (an external tool exists). The Public Tag Repository (PTR) is opt-in and downloads the whole tag DB so nobody learns what you own. v689 (2026-09-30). | High (hydrus docs, PTR page footer 2026-09-06; API) |
| digiKam | Haar-wavelet fingerprints must be built first (Maintenance, incremental by default). Similarity range control. Advises confining searches to albums or tags because all-pairs is slow. | Medium (docs.digikam.org via search excerpt + KDE source) |
| Czkawka 12.0.2 (2026-09-09) | Similar images: hash algorithms Gradient (CLI default), Mean, Blockhash and others; optional flip/rotate invariance. **Similar videos**: group by duration within a tolerance, hash frames in temporal windows, match a fraction of windows; subclip detection; optional audio fingerprint; needs ffmpeg. Shared cache; prehash of first and last 4 KB. Open issue: impostor download sites (`czkawka.com/.net/.org/.cn` are not official). | High (instructions/Instruction_Core.md, commit 2026-07-06) |
| TagSpaces | Tags either embedded in **file names** (renames files) or in sidecar JSON under hidden `.ts` folders, with sync and rename caveats. | High (docs, updated 2026-04-02) |

Lessons:

- JizzCraft's rule "user data is database-only, no sidecars, never rename" is stricter than TagSpaces and right for
  privacy.
- The dedupe UX to copy is Hydrus's four decisions plus remembered false positives, but **without delete**: JizzCraft
  never deletes, so "better" means "hide the other".
- For video, follow Czkawka's duration-bucket plus temporal-window hashing, reusing the existing scrub-preview frame
  pipeline in `media`.

### 1.10 Lutris and Heroic: guided setup

| Project | Finding | Confidence |
|---|---|---|
| Lutris v0.5.22 (2026-02-25) | Fully automated installs through **user-made YAML installer scripts** hosted on lutris.net. Scripts can `execute` arbitrary programs. Runners are downloaded from lutris.net. Optional accounts. | High (README.rst, docs/installers.rst) |
| Heroic v2.22.3 (2026-09-16) | Finds Wine and Proton builds in several standard locations (including Steam library folders), accepts only valid tools, and warns and suggests another when a path is invalid. | Medium (one GitHub issue in ProtonUp-Qt describing Heroic's behaviour) |

Lessons:

- Copy Heroic's "scan standard locations, validate, explain, suggest" for yt-dlp, mpv, VLC, qBittorrent and
  Transmission.
- Do **not** copy Lutris's executable installer scripts. JizzCraft's declarative-JSON-only rule is the safer contract.

---

## 2. Torrent-streaming engineering lessons

1. **Deadline-based piece picking beats plain sequential.** libtorrent's streaming design (docs for 2.1.0) keeps
   time-critical pieces sorted by deadline. Each block request goes to the peer with the lowest estimated queue time,
   excluding the slowest 10 % and snubbed or choked peers. It stops when the best queue exceeds about 2 s. Adaptive
   timeouts allow one extra duplicate ("busy") request per timeout. libtorrent explains that naive sequential mode
   "can send piece 0 to a slow peer while fast peers receive later pieces". librqbit uses a 32 MiB per-stream lookahead
   window with fair interleaving across streams. *Recommendation:* keep librqbit, but expose the lookahead as a
   function of bitrate (for example, about 20-30 s of media). Request the **first and last pieces** first (qBittorrent's
   `firstLastPiecePrio`), which JizzCraft already does for tail-indexed MP4/MKV. Confidence: High.
2. **Metadata (magnet) timeouts are the app's job.** BEP 9 (last modified 2017-03-26) defines `ut_metadata` in
   16 KiB blocks and **gives no timeout guidance**. Without trackers, DHT is the only path. In JizzCraft,
   `list_files_with_peers` → `list_prepared` awaits `add_torrent(list_only)` with **no timeout and no progress
   feedback**. rqbit #306 reports memory creep when such calls are wrapped in timeouts at scale. *Recommendation:* show
   a "Looking for peers: N found, metadata X/Y" state, give up after a configurable 60-120 s with a plain sentence, and
   make cancel abort the task and drop the list-only torrent. Confidence: High (code read) / Medium (rqbit leak cause).
3. **Proxy and VPN leaks come from the non-peer protocols.** libtorrent docs: `proxy_peer_connections` covers peers and
   web seeds, while "trackers and DHT traffic are not considered peer connections". mpv documents that SOCKS is not
   supported for its ytdl proxy path and that https bypasses it. JizzCraft already **turns off DHT, trackers and the
   listener while a SOCKS proxy is set**, which is correct. librqbit also offers **`bind_device_name`**
   (SO_BINDTODEVICE). Binding to `wg0` or `tun0` is the community-standard "kill switch" (qBittorrent forum advice) and
   fails closed when the VPN drops. *Recommendation:* add "Bind to network interface" next to the proxy, listing
   interfaces and pre-selecting tun/wg ones, and refuse to start if the interface is down. Confidence: High (docs and
   source) / Medium (forum).
4. **Port forwarding and UPnP.** libtorrent enables UPnP and NAT-PMP by default; librqbit defaults to off. Mullvad
   removed port forwarding (announced 2023-05-29, effective 2023-07-01) because of abuse, so many VPN users cannot
   accept incoming connections anyway. UPnP has its own vulnerability history (CallStranger CVE-2020-12695, which
   concerns UPnP devices; CERT/CC, June 2020). *Recommendation:* keep incoming off by default. Offer UPnP only as an
   explicit opt-in under "Advanced", and never when a proxy or bound interface is set. Confidence: High (docs) /
   Medium (CallStranger, secondary sources only).
5. **Seeding policy.** libtorrent's defaults treat a seed as "done" at share ratio 2.0 or 24 h seed time. Torznab
   exposes per-release `minimumratio` / `minimumseedtime`. JizzCraft pauses on completion. *Recommendation:* keep that
   default (privacy, and rqbit #525 shows that "no upload" still accepted connections), but add an opt-in per-indexer
   "seed to requirement, then stop" and a visible warning when a result carries requirements. Confidence: High.
6. **Anti-abuse and identity.** libtorrent `anonymous_mode` hides the client version and local IPs from trackers.
   librqbit lets you set `client_name_and_version` (default `rqbit X.Y.Z`). Blocklists are available through
   `blocklist_url`, which is **user-supplied** to respect the "nothing bundled" rule. Confidence: High.
7. **Resource hygiene.** rqbit #525 (open): FD exhaustion and memory growth in long-lived sessions after tracker changes,
   with peers accepted after completion. *Recommendation:* shut the session down when no torrent is active (JizzCraft
   already lazily creates it). Add a "session restarts" counter to diagnostics. Confidence: Medium.

## 3. HLS/DASH client lessons

What JizzCraft has today (`fetch/segmented.rs`, `pick.rs`, `hls.rs`):

- VOD only.
- EWMA throughput (0.7/0.3) and a 0.8 budget factor.
- Retries with `Retry-After`.
- Discontinuity flags.
- Auto mode **steps down after two slow segments and never steps back up**.

Reference defaults:

| Player | ABR | Buffer | Retry | Live |
|---|---|---|---|---|
| hls.js (src/config.ts, master; v1.7.3 2026-09-11) | Two EWMAs (fast 3 s, slow 9 s for VoD and live), default estimate 500 kbps, `abrBandWidthFactor` 0.95 (stay), **`abrBandWidthUpFactor` 0.7 (switch up)**, `maxStarvationDelay` 4 s | `maxBufferLength` 30 s, `maxMaxBufferLength` 600 s, `maxBufferHole` 0.1 s, `nudgeMaxRetry` 3 | fragments: TTFB 10 s, load 120 s, 4 timeout retries, 6 error retries with 1-8 s backoff; keys: 8 error retries | `liveSyncDurationCount` 3 (start at N-3 segments) |
| dash.js (Settings.js, development; v5.2.1 2026-08-17) | **Throughput + BOLA** active and switched by buffer level; InsufficientBuffer, SwitchHistory and AbandonRequests on; DroppedFrames, L2A and LoL+ off; EWMA half-lives 8 s / 3 s; `bandwidthSafetyFactor` 0.9 | `bufferTimeAtTopQuality` 30 s, `bufferToKeep` 20 s | 3 attempts, 500-1000 ms intervals; fragment timeout 20 s | `liveDelayFragmentCount` NaN (computed) |
| Shaka (player_configuration.js, main; v5.2.13 2026-10-09) | `switchInterval` 8 s, upgrade target 0.85, downgrade target 0.95 | `bufferingGoal` 10 s, `rebufferingGoal` 0, `bufferBehind` 30 s; gap jump 0.5 s; stall detection 1 s | `maxAttempts` 2, base delay 1 s, backoff ×2, fuzz 0.5, timeout 30 s, stall 5 s, connect 10 s | n/a |
| GStreamer adaptivedemux2 (new plugin in 1.22, released 2023-01-23; local 1.28.8) | `bandwidth-target-ratio` 0.8 | `max-buffering-time` 30 s, `high-watermark-time` 30 s | n/a | n/a |
| FFmpeg hls demuxer | n/a | n/a | `seg_max_retry` 0, `m3u8_hold_counters` 1000, HTTP `reconnect_*` and `respect_retry_after` (default on) | `live_start_index` (negative from end) |

Recommendations:

- **ABR with hysteresis.** Add an up-switch: consider stepping up only when `0.7 × estimate > next bitrate` and at
  least about 8 s have passed since the last switch (hls.js plus shaka). Start conservatively (lowest variant ≥ 360p,
  or the last-session estimate kept in memory only, since quality choices are never written to disk). A BOLA-style
  buffer rule (Spiteri, Urgaonkar, Sitaraman, arXiv 1601.06748, IEEE/ACM ToN 2020) suits a pull-based reader because it
  needs no bandwidth prediction. Confidence: High that these are the defaults; Medium that they transfer to JizzCraft
  unchanged.
- **Discontinuities.** RFC 8216 (Aug 2017, §4.3.2.3) says `EXT-X-DISCONTINUITY` MUST mark changes of format, tracks or
  **timestamp sequence**. JizzCraft already resets the timeline on the first chunk after one. Keep a test with a
  mid-stream codec or resolution change (§6.3.3: "clients SHOULD deal with encoding changes"). Confidence: High.
- **Live (if ever).** RFC 8216 §6.3.3: do not start within three target durations of the end. §6.3.4: reload no sooner
  than the target duration, or half of it when unchanged. These match hls.js's `liveSyncDurationCount` 3. Live is
  currently "not done yet" and LL-HLS is "will not be done". Confidence: High.
- **Error recovery.** Bounded retries with exponential backoff and jitter (shaka), separate key and manifest policies
  (hls.js), honouring `Retry-After`, and penalising a variant that fails then re-electing it later (hls.js
  `errorPenaltyExpireMs`). Confidence: High.
- **Security.** CVE-2017-9993 (published 2017-06-28): FFmpeg's HLS demuxer did not restrict segment extensions, which
  allowed **local-file reads from crafted playlists**. FFmpeg now has `allowed_extensions` / `extension_picky`.
  JizzCraft's `inward_ok` (no file://, loopback or private targets from public manifests) and "the player refuses
  manifests" are the right answers. Keep them tested. Confidence: High (NVD).

## 4. Legal and safety posture of comparable tools (descriptive, not advice)

| Pattern | Who | Evidence | Confidence |
|---|---|---|---|
| "We host zero content / provide no repos" | Mihon | README disclaimer, FAQ | High |
| Neutral platform with an official curated repo; third-party disabled by default | Kodi | Unknown-sources setting docs; kodi.tv statements | High (settings) / Medium (statements, older) |
| No bundled sources; user adds an indexer | Jackett, Prowlarr, JizzCraft | READMEs and wiki | High |
| Preloaded content extensions led to C&D and shutdown | Tachiyomi (Jan 2024), Popcorn Time | TorrentFreak and others | Medium |
| DMCA §1201 takedown over tests that referenced copyrighted videos; reinstated after the tests were patched; GitHub added a 1201 review process and a $1M defense fund | youtube-dl (Oct-Nov 2020) | GitHub blog 2020-11-16 (updated 2021-08-24) | High |
| Content gating as user-selected levels | Mihon "Safe / Mixed / 18+" (v0.21.0) | CHANGELOG | High |
| Refuse unsafe exposure by default | Stash security tripwire | Stash docs | High |
| Store policy | Flathub rejects submissions that "engage or encourage to engage in unlawful activities" or violate copyrights; requires OARS ratings ("must be properly tagged"); **manifests must not contain AI-generated or AI-assisted content** (restored 2026-09-21 after a disclosure-based policy on 2026-09-04) | flathub-infra/documentation, commit 2026-10-04 | High |

Implications for JizzCraft, which already follows most of this:

- Keep docs and tests free of real sites and real titles. The youtube-dl case started from test references.
- Keep the repo list empty. Make repository content-level labels the user's filter, not the app's catalog.
- JizzCraft's metainfo already declares OARS `sex-nudity: intense`.
- The Flathub manifest needs a human author, or the project should publish its own Flatpak repo or bundle instead.

---

## 5. Contradictions and caveats

- **Stremio's official stance**: only secondary sources were found. Treated as Low.
- **Popcorn Time's 2022 closure cause**: the team said "lack of use", TorrentFreak said competition. Both are recorded.
- **Tachiyomi timeline**: TorrentFreak says builds without preloaded extensions shipped 6-8 January; secondary sites
  say 9 January for fork removal. The primary post now redirects.
- **Stash version check**: the code exists, but whether it runs automatically was not verified (Medium).
- **Heroic detection**: a single issue description (Medium).
- **CallStranger**: concerns UPnP devices, not IGD clients. Cited only as context for "UPnP is attack surface"
  (Medium).
- **Kodi statements**: mostly 2015-2019, flagged as older than 12 months.

## 6. Knowledge gaps

- No user-research data (surveys) on adult-library users specifically. The complaints come from GitHub issue
  activity, which skews toward technical users.
- Plex "Discover Together" privacy controversy: not verified, so omitted.
- Stremio's local streaming-server internals and failure modes were not researched.
- Real-world librqbit streaming performance compared with libtorrent: no benchmark found (rqbit #331 "Slow streaming?"
  is open with 5 comments).

---

## 7. Ranked features for a nearly turnkey JizzCraft

Ranking: value to a first-time user divided by effort, then risk. Effort: S < 3 days, M ≤ 2 weeks, L > 2 weeks.
Every item respects these rules: nothing bundled; no network in the default build; no telemetry; keys only in
`fetch.json`; the player never takes a URL.

| # | Feature | What | Why (evidence) | Crates | Effort | Risks |
|---|---|---|---|---|---|---|
| F1 | **First-run wizard** | After the age gate: (1) pick library folders, (2) privacy defaults (discreet launch, history on/off, playful messages), (3) run `doctor` inline with a copyable fix command, (4) in the network build only, an optional "Connections" step. Every step skippable; re-open from the menu. | Stash setup wizard; Hydrus has none and documents the friction | `ui` (new `app/onboarding.rs`), `core` settings key `onboarding.done`, `media::doctor` | M | Wizard fatigue: keep it to 4 screens. Must not connect anything. |
| F2 | **In-app Doctor / Diagnostics page** | GUI for `doctor`: codecs, HW decode, writable dirs, GPU backend, plus (network build) proxy reachability, bound interface up, indexers OK, resolver versions. One "Copy redacted report" button (no names or paths). | Plex/Jellyfin users rely on dashboards; JizzCraft only has CLI `doctor` | `media::doctor`, `fetch` (new `health.rs`), `ui` | S-M | The report must stay redacted (reuse `fetch::redact`). |
| F3 | **Magnet metadata feedback and timeout** | "Finding peers: N; metadata X/Y KiB" with a 90 s default limit, a plain failure sentence, and a real cancel that drops the list-only torrent. | BEP 9 has no timeout; `list_prepared` has none; rqbit #306 | `fetch::service::torrent`, `ui::app::downloads` | S | Leaking tasks: test cancel paths. |
| F4 | **Detect installed tools** (Heroic-style) | Scan `PATH` and standard locations for yt-dlp, mpv, VLC, qBittorrent, Transmission; show version and age; one click to register yt-dlp as a resolver with a vetted argv (`-J --no-playlist --ignore-config -- {url}`). Nothing is downloaded. | Heroic tool detection; yt-dlp README (nightly recommended, JS runtime needed) | `fetch::detect`, `fetch::resolver`, `ui` | S | Version parsing is fragile. Never run detection automatically in the default build (it is local-only anyway). |
| F5 | **Import all indexers from Prowlarr** | Extend `fetch.indexers.detect`: list Prowlarr's indexers and create one Torznab endpoint per enabled indexer (Add-and-Remove semantics); for Jackett, the "all" aggregate endpoint. | Prowlarr app sync model | `fetch::detect`, `fetch::torznab`, `fetch::config` | S | API changes across Prowlarr versions; keys only in `fetch.json`. |
| F6 | **Bind to network interface (VPN)** | Expose librqbit `bind_device_name`; list interfaces and highlight `wg*` / `tun*`; refuse to start when the interface is missing; works with or without SOCKS. | librqbit 9.0.1 `SessionOptions`; libtorrent says DHT and trackers are not peer traffic; qBittorrent community practice | `fetch::config`, `fetch::service`, `ui` | S | Linux-first (SO_BINDTODEVICE); macOS uses IP_BOUND_IF; Windows not supported by this option. Document it. |
| F7 | **User-added repositories ("channels") for sources and resolver recipes** | A channel is an https URL to `channel.json` plus `index.json` listing declarative source JSON and resolver *recipes* (argv templates only, never binaries) with sha256 per item and an **Ed25519/minisign signature**. On add: show name, website and content level, require explicit confirmation, and pin the key (TOFU, as in Mihon). Updates are only **checked** when the user clicks, and only **applied** after a diff is shown. The app ships an empty list, with docs using `.invalid` hosts. | Mihon repo signing (v0.17 → v0.21); Kodi unsigned repos + ESET 2018; Stash sha256 indexes; Jellyfin manifest URL | `fetch` (new `channels.rs`), `fetch::sources`, `fetch::resolver`, `ui` | M-L | Biggest policy risk: the project must never host, list or link a channel. AGENTS.md needs a rule. Key rotation and revocation UX. |
| F8 | **Content-level filter on channels and sources** | Each channel item declares a level (for example general / mixed / adult). The user chooses the allowed levels in Settings. | Mihon "allowed content warnings" | `fetch::sources`, `ui` | S | Self-declared levels can lie; label it as a filter, not a guarantee. |
| F9 | **Standard-format import** | Import Kodi/Jellyfin NFO (title, plot, tags, ratings, playcount) and Stash JSON export (scene metadata, file fingerprints incl. `oshash`; JizzCraft's size + head/tail fingerprint is the same shape as the OpenSubtitles hash family, so a compatible hash could be added as a new fingerprint column). Dry-run first, as `meta.import` does. Nothing is written back to media. | Jellyfin and Kodi NFO precedence; Stash `jsonschema/file_folder.go` | `core` (new `import_nfo.rs`, `import_stash.rs`, migration for tags), `ui` | M | Tags do not exist yet (roadmap); NFO parsing must be bounded (XML bombs). The OSHash equivalence needs a test. |
| F10 | **Tags** (prerequisite for F9/F12) | Tag tables, inspector chips, filters, CLI. Database-only. | Hydrus and Stash core feature; ROADMAP "Library: tags" | `core` (migration), `ui` | M | Schema is append-only: design it once. |
| F11 | **File watching** (auto-import) | inotify / FSEvents / ReadDirectoryChangesW watcher feeding the existing scan job; debounced; toasts with counts only. | ROADMAP; "rescan is manual" is a known gap; torrent downloads already auto-import | `core::scan` (+ `notify` crate, local-only), `ui` | M | Must stay network-free (`offline_deps` test); large trees can hit inotify limits, so fall back to periodic rescan. |
| F12 | **Duplicate review** (perceptual) | Image pHash and video temporal-window hashes (Czkawka method) from frames the `media` crate already decodes. Review UI with Hydrus decisions: better (hides the other), same, related, not related (remembered). Never deletes. | Hydrus duplicates; Czkawka similar videos; digiKam fingerprints; ROADMAP | `media` (hashing), `core` (tables), `ui` | L | CPU cost; false positives (start at distance 0, as Hydrus advises). |
| F13 | **ABR up-switch with hysteresis** | Switch up when `0.7 × est > bitrate` and ≥ 8 s since the last switch; keep the stall downgrade; start with a conservative estimate. | hls.js, shaka, dash.js defaults; current code only steps down | `fetch::segmented`, `fetch::pick` | S | Oscillation: test with the throttled reader. |
| F14 | **Seeding-requirement awareness** | Parse Torznab `minimumratio` / `minimumseedtime` / `seedtype`; show "this indexer expects seeding"; opt-in "seed to requirement then stop" per indexer. | Torznab 1.3; seeding off by default | `fetch::torznab`, `fetch::service::torrent`, `ui` | S-M | Seeding exposes the IP longer: say so in the same sentence. |
| F15 | **Polite search defaults** | Per-indexer rate floor (≥ 2 s), exponential backoff on 429/503, honour `Retry-After`, plus a "temporarily disabled, retry at…" status. | Prowlarr #2635 / #2650; FFmpeg `respect_retry_after` | `fetch::torznab`, `fetch::service` | S | None significant. |
| F16 | **Opt-in update check, no phoning home by default** | Off by default. A "Check now" button (network build only) queries GitHub's latest-release API through the configured proxy and shows the version and changelog link. Verification stays manual (`sha256sum`, `gh attestation verify`); `install.sh` re-run does the update. The default build shows only "you are on X.Y.Z, built on DATE" plus the update command. | Hydrus (never connects), Stash check_version, Prowlarr auto-update and analytics (counter-example) | `fetch` (new `update.rs`), `ui` | S | GitHub learns the IP; disclose that next to the button. |
| F17 | **Recovery helpers** | Auto-backup of the DB before migrations (keep 3); a "Restore from backup" flow; a safe mode (`--safe`: no GPU features, no network session, default theme). | Hydrus backup culture; ROADMAP "restore-from-backup" | `core` (backup exists), `apps/jizzcraft`, `ui` | S-M | Disk use; the backups hold sensitive data, so put them in the same private dir with 0600 permissions. |
| F18 | **Hand-off to an existing torrent client** | "Send to qBittorrent / Transmission" via their local WebUI/RPC (detected on loopback only when clicked), as an alternative to the built-in engine. | qBittorrent WebUI API v2, Transmission RPC | `fetch` (new `handoff.rs`), `ui` | M | Their credentials must go in `fetch.json`; the other client is not covered by `require_proxy`, so say so (as the mpv/VLC hand-off already does). |
| F19 | **Resolver health and staleness hints** | Record (in memory) the last success or failure per resolver; after N failures show "your resolver may be outdated (version D)". | yt-dlp's most-commented issues are site breakages | `fetch::resolver`, `ui` | S | None. |
| F20 | **Interface-down / proxy-down banner** | A persistent status chip ("Protected: wg0", "Proxy OK", "Blocked: proxy unreachable") in Downloads. | Turns the kill switch into visible state | `fetch::service::proxy`, `ui` | S | Polling must not leak; only local checks plus the user-triggered proxy test. |
| F21 | **Subtitle sidecar playback (local only)** | Play `.srt` / `.vtt` / `.ass` next to the file and embedded tracks; parse with bounded parsers; **never fetch subtitles online**. | Check Point 2017 subtitle exploits; Jellyfin CVE-2026-48793 | `media` (GStreamer subparse), `ui` | M | Parser bugs: run in the planned sandboxed worker when that exists. |
| F22 | **Profiles / portable export of settings** | Export `fetch.json` **without secrets** plus app settings, for moving machines; secrets re-entered or copied by the user. | Mihon and Stash backup flows | `core::portability`, `fetch::config` | S | Accidental secret export: add a test like the existing "never in backups" rule. |

Suggested order: F3, F15, F13, F6, F20 (correctness and safety, all S) → F2, F4, F5, F1 (the turnkey core) → F10,
F9, F11 → F7, F8, F16, F19 → F12, F14, F17, F18, F21, F22.

## 8. Do not copy

1. **Opt-out analytics** (Prowlarr `AnalyticsEnabled` defaults to true). It contradicts "no telemetry in any build".
2. **Auto-installing add-on updates from any repository** (Kodi default "Install updates automatically" with unsigned
   third-party repos; ESET 2018 cryptominers).
3. **Preloaded sources or extensions**, even "harmless" ones, and any in-app directory of community sources
   (Tachiyomi 2024; Stremio's central "Community addons" listing; Kodi's unofficial-repo list on its wiki).
4. **Executable installer scripts** (Lutris `execute`). Keep JSON declarative and resolvers as user-installed binaries
   only.
5. **Remote code plugins** (Stash UI plugins inject JS/CSS; Mihon extensions are APKs with full app access). If plugins
   ever come, use declarative data or a WASM sandbox with no ambient network, per the ROADMAP joke.
6. **A local web server without auth** (Stash needed a tripwire; Jellyfin's unauthenticated endpoints led to ffmpeg
   injection and file read). The planned control channel stays a Unix socket, never TCP.
7. **String-built command lines** (Jellyfin CVE-2026-35033 and CVE-2026-48793). Keep argv-only.
8. **Renaming files or writing sidecars to store tags** (TagSpaces filename tagging).
9. **UPnP on by default** (libtorrent default) and **port forwarding assumptions**.
10. **Auto-downloading helper binaries** (Stash downloads ffmpeg, Lutris downloads runners). Print the distro command
    instead, as `doctor` does.
11. **Accounts or cloud sync** (Plex 2022 and 2025 breaches; Kodi forum 2023 breach).
12. **Subtitle auto-download** (2017 subtitle exploits).
13. **Updating from unverified repositories** (`yt-dlp --update-to owner/repo`: "no verification is done").

---

## 9. Timeline (selected)

| Date | Event |
|---|---|
| 2017-03-26 | BEP 9 last modified |
| 2017-05-23 | Check Point "Hacked in Translation" (VLC, Kodi, Popcorn Time, Stremio) |
| 2017-06-28 | CVE-2017-9993 FFmpeg HLS local file read published |
| 2017-08 | RFC 8216 (HLS) |
| 2018-09-13 | ESET: Kodi add-on cryptominers |
| 2020-08-29 | Last WebTorrent Desktop release (v0.24.0) |
| 2020-11-16 | GitHub reinstates youtube-dl; 1201 review process |
| 2022-01 | Popcorn Time closes again |
| 2023-01-23 | GStreamer 1.22 with adaptivedemux2 |
| 2023-02 to 2023-04 | Kodi forum breach disclosed |
| 2023-05-29 | Mullvad announces removal of port forwarding |
| 2024-01-13 | Tachiyomi ends development |
| 2024-08-15 | Stash CVE-2024-32231 advisory |
| 2024-10-26 | Mihon v0.17.0 extension trust system |
| 2025-09 | Plex breach disclosure |
| 2026-01-08 | rqbit #525 FD and memory exhaustion reported |
| 2026-04-14 / 06-10 / 09-22 | Jellyfin advisory batches |
| 2026-08-20 | librqbit 9.0.1 |
| 2026-09-21 | Flathub restores the explicit LLM ban for manifests |
| 2026-10-12 | Mihon v0.21.0: repo-key-matched installs, content warnings |

## 10. Source quality matrix

| Source | Type | Date (published / updated) | Credibility | Conflict of interest |
|---|---|---|---|---|
| librqbit 9.0.1 source (crates.io) | Primary code | 2026-08-20 | High | None |
| libtorrent docs (streaming, settings) | Primary docs | v2.1.0, undated | High | None |
| BEP 9 (bittorrent.org) | Spec | 2017-03-26 | High | None |
| RFC 8216 | Spec | 2017-08 | High | None |
| hls.js `config.ts`, `API.md` | Primary code/docs | master, fetched 2026-10-14 | High | None |
| dash.js `Settings.js`; dashif ABR page | Primary code/docs | development, 2026-10 | High | None |
| Shaka `player_configuration.js`, `backoff.js` | Primary code | main, 2026-10 | High | None |
| gst-inspect hlsdemux2 (1.28.8); GStreamer 1.22 notes | Primary | 2023-01-23 / local | High | None |
| FFmpeg formats/protocols docs; NVD CVE-2017-9993 | Primary | current / 2017-06-28 | High | None |
| Stash docs + source; GHSA-75jf-52jg-qqh4 | Primary | 2024-11 to 2025-11; 2024-08-15 | High | Project self-description |
| Stremio addon SDK docs | Primary | 2025-11-27 / 2026-09-13 | High | Vendor docs |
| Kodi wiki (Add-on repositories, Settings, NFO) | Primary | 2023-02-14 / 2024-05-02 / 2025-08-04 | High | Project self-description |
| Jellyfin docs + GitHub security advisories | Primary | 2026 | High | None |
| mpv `options.rst`; yt-dlp README | Primary | master, 2026-10 | High | None |
| Servarr wiki (Prowlarr), Prowlarr source, issues #2635/#2650 | Primary | 2026-09-05 / 2026-03/04 | High | Project self-description |
| Torznab spec 1.3 draft | Spec | undated draft | Medium-High | Maintained by the *arr ecosystem |
| qBittorrent WebUI API wiki; Transmission rpc-spec | Primary | 5.0 API / main | High | None |
| Mihon source + CHANGELOG + FAQ | Primary | 2024-10-26 to 2026-10-13 | High | None |
| Hydrus docs (install, PTR, duplicates) | Primary | 2026-09-06 | High | None |
| Czkawka `Instruction_Core.md` | Primary | 2026-07-06 | High | None |
| TagSpaces docs | Primary | 2026-04-02 | High | Commercial vendor (Pro tier) |
| digiKam docs (via search excerpt) | Primary (indirect) | undated | Medium | None |
| Lutris README / installers.rst | Primary | master | High | None |
| Flathub requirements and metainfo guidelines | Primary | 2026-10-04 | High | Store operator |
| GitHub blog (youtube-dl) | Primary | 2020-11-16 / 2021-08-24 | High | GitHub is the host |
| Mullvad blog | Primary | 2023-05-29 | High | VPN vendor (explains its own policy) |
| Check Point research | Primary (vendor research) | 2017-05-23 | High | **Security vendor marketing** |
| ESET blog | Primary (vendor research) | 2018-09-13 | High | **Security vendor marketing** |
| TorrentFreak (Kodi, Tachiyomi, Aniyomi) | Specialist journalism | 2019 to 2024 | Medium | Piracy-focused outlet |
| The Hacker News, HIBP (Kodi breach) | Journalism / breach DB | 2023-04 | Medium-High | None |
| BetaNews, AlternativeTo, Slashdot (Plex 2025) | Journalism | 2025-09 | Medium | None |
| Engadget/TechCrunch, DigitalTVEurope (Popcorn Time) | Journalism | 2022-01 | Medium | None |
| geekchamp.com, rapidseedbox.com (Stremio) | Blogs | undated | **Low** | **Seedbox vendor** |
| qBittorrent forum, turbogeek guide (VPN binding) | Community | various | Low-Medium | Some guides promote VPNs |
| Tenable, SC Media, SecurityWeek (CallStranger) | Journalism / vendor | 2020-06 | Medium | Tenable is a security vendor |

## 11. Bibliography

All accessed 2026-10-14.

1. ikatson/rqbit (README; issues #306, #331, #525) and librqbit 9.0.1 source, `session.rs`, `listen.rs`,
   `torrent_state/streaming.rs`, `stream_connect.rs`. https://github.com/ikatson/rqbit
2. libtorrent, "Streaming". https://www.libtorrent.org/streaming.html
3. libtorrent, "Settings". https://www.libtorrent.org/reference-Settings.html
4. BEP 9, "Extension for Peers to Send Metadata Files". https://www.bittorrent.org/beps/bep_0009.html
5. RFC 8216, "HTTP Live Streaming" (Aug 2017). https://www.rfc-editor.org/rfc/rfc8216
6. hls.js, `src/config.ts` and `docs/API.md`. https://github.com/video-dev/hls.js
7. dash.js, `src/core/Settings.js`. https://github.com/Dash-Industry-Forum/dash.js; ABR settings page:
   https://dashif.org/dash.js/pages/usage/abr/settings.html
8. Shaka Player, `lib/util/player_configuration.js`, `lib/net/backoff.js`.
   https://github.com/shaka-project/shaka-player
9. GStreamer hlsdemux2 docs. https://gstreamer.freedesktop.org/documentation/adaptivedemux2/hlsdemux2.html; 1.22 release
   notes (2023-01-23). https://gstreamer.freedesktop.org/releases/1.22/
10. FFmpeg formats and protocols docs. https://ffmpeg.org/ffmpeg-formats.html, https://ffmpeg.org/ffmpeg-protocols.html
11. NVD, CVE-2017-9993. https://nvd.nist.gov/vuln/detail/CVE-2017-9993
12. Spiteri, Urgaonkar, Sitaraman, "BOLA: Near-Optimal Bitrate Adaptation for Online Videos", arXiv:1601.06748 (v3
    2020-06-18). https://arxiv.org/abs/1601.06748
13. Stash docs: scraping https://docs.stashapp.cc/in-app-manual/scraping/ ; plugins
    https://docs.stashapp.cc/in-app-manual/plugins/ ; first steps
    https://docs.stashapp.cc/getting-started/first-steps/ ; tripwire
    https://docs.stashapp.cc/networking/authentication-required-when-accessing-stash-from-the-internet/
14. stashapp/stash source (`Setup.tsx`, `download_ffmpeg.go`, `check_version.go`, `pkg/models/jsonschema/file_folder.go`).
    https://github.com/stashapp/stash
15. GHSA-75jf-52jg-qqh4 / CVE-2024-32231. https://github.com/advisories/GHSA-75jf-52jg-qqh4
16. Stremio addon SDK, `docs/protocol.md`, `docs/api/responses/stream.md`, `docs/deploying/README.md`.
    https://github.com/Stremio/stremio-addon-sdk
17. Kodi wiki: Add-on repositories https://kodi.wiki/view/Add-on_repositories ; Settings/System/Add-ons
    https://kodi.wiki/view/Settings/System/Add-ons ; Official add-on repository
    https://kodi.wiki/view/Official_add-on_repository ; NFO files https://kodi.wiki/view/NFO_files
18. Kodi, "Piracy box sellers and YouTube promoters are killing Kodi".
    https://kodi.tv/article/piracy-box-sellers-and-youtube-promoters-are-killing-kodi ; TorrentFreak 2019-09-26.
    https://torrentfreak.com/kodi-is-sick-of-pirate-addons-but-banning-them-is-not-an-option-190926/
19. ESET, "Kodi add-ons launch cryptomining campaign" (2018-09-13).
    https://blog.eset.ie/2018/09/13/kodi-add-ons-launch-cryptomining-campaign/
20. Kodi forum breach: Have I Been Pwned https://haveibeenpwned.com/breach/KodiFoundation ; The Hacker News (Apr 2023).
21. Jellyfin docs: plugins https://jellyfin.org/docs/general/server/plugins/ ; NFO
    https://jellyfin.org/docs/general/server/metadata/nfo/ ; security advisories
    https://github.com/jellyfin/jellyfin/security/advisories (CVE-2026-35033, CVE-2026-48793, CVE-2026-49246)
22. Plex 2025 breach coverage, BetaNews (2025-09-09).
    https://betanews.com/2025/09/09/plex-suffers-data-breach-warns-customers-to-change-passwords/
23. mpv manual, `DOCS/man/options.rst`. https://github.com/mpv-player/mpv
24. yt-dlp README. https://github.com/yt-dlp/yt-dlp
25. Servarr wiki, Prowlarr settings, FAQ, quick start. https://github.com/Servarr/Wiki (prowlarr/*.md, 2026-09-05)
26. Prowlarr source `ConfigFileProvider.cs`, `NewznabStandardCategory.cs`; issues #2635, #2650.
    https://github.com/Prowlarr/Prowlarr
27. Jackett README and `TorznabCatType.cs`. https://github.com/Jackett/Jackett
28. Torznab Specification 1.3 (draft). https://torznab.github.io/spec-1.3-draft/
29. qBittorrent WebUI API (5.0). https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-5.0)
30. Transmission RPC spec. https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md
31. WebTorrent Desktop. https://github.com/webtorrent/webtorrent-desktop
32. Peerflix README. https://github.com/mafintosh/peerflix
33. Popcorn Time 2022 closure: DigitalTVEurope (2022-01-06)
    https://digitaltveurope.com/2022/01/06/popcorn-time-shuts-down-reflecting-shift-in-piracy-trends ; Engadget
    https://www.engadget.com/movie-piracy-app-popcorn-time-shuts-down-115523694.html
34. Check Point, "Hacked in Translation" (2017-05-23). https://research.checkpoint.com/2017/hacked-in-translation/
35. Mihon: FAQ https://mihon.app/docs/faq/browse/extensions ; README, CHANGELOG, `NetworkLegacyExtensionRepo.kt`
    https://github.com/mihonapp/mihon
36. Tachiyomi / Aniyomi: TorrentFreak (2024-06-17)
    https://torrentfreak.com/sony-dmca-notice-nukes-200-aniyomi-extensions-as-tachiyomi-fork-feels-heat-240617/ ;
    AnimeCorner
    https://animecorner.me/tachiyomi-team-stops-app-development-after-cease-and-desist-by-kakao-entertainment/ ;
    Wikipedia https://en.wikipedia.org/wiki/Tachiyomi
37. Hydrus docs: installing https://hydrusnetwork.github.io/hydrus/getting_started_installing.html ; PTR
    https://hydrusnetwork.github.io/hydrus/PTR.html ; duplicates https://hydrusnetwork.github.io/hydrus/duplicates.html
38. Czkawka, `instructions/Instruction.md`, `Instruction_Core.md`. https://github.com/qarmin/czkawka
39. digiKam Similarity View. https://docs.digikam.org/en/left_sidebar/similarity_view.html
40. TagSpaces tagging docs (2026-04-02). https://docs.tagspaces.org/tagging/
41. Lutris README and installers docs. https://github.com/lutris/lutris
42. ProtonUp-Qt issue #168 (Heroic tool detection). https://github.com/DavidoTek/ProtonUp-Qt/issues/168
43. Flathub requirements and metainfo guidelines. https://github.com/flathub-infra/documentation (commit 2026-10-04)
44. GitHub blog, "Standing up for developers: youtube-dl is back" (2020-11-16).
    https://github.blog/news-insights/policy-news-and-insights/standing-up-for-developers-youtube-dl-is-back/
45. Mullvad, "Removing the support for forwarded ports" (2023-05-29).
    https://mullvad.net/en/blog/removing-the-support-for-forwarded-ports
46. CallStranger CVE-2020-12695: Tenable blog
    https://www.tenable.com/blog/cve-2020-12695-callstranger-vulnerability-in-universal-plug-and-play-upnp-puts-billions-of
47. GitHub REST API release and activity metadata for all repositories above (queried 2026-10-14).
