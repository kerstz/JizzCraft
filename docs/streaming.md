# Streaming from your own sources

JizzCraft ships **no** source, site, indexer, catalog or preset. Everything
below is something you configure yourself, in the network build
(`--features network`). Nothing connects before you act.

## What plays

| You give | Command | Result |
| --- | --- | --- |
| a direct `http(s)` video address | `stream.open {url}` | seekable playback through `Range` requests, nothing written to disk. A server that ignores `Range` falls back to save-while-playing. |
| a page address and a resolver you installed | `stream.resolve {url, resolver}` | the resolver prints file addresses, the app streams the best one |
| any other page address | `extract.page {url}`, or paste it in the Downloads box | the built-in page reader looks for standard video markup (see below); the app streams the best address it finds |
| a page address and `Download` | `download.start {url}` | the video is saved to your save folder; nothing is written to disk unless you press Download |
| a result of a source you defined | `fetch.play {result}` | same as `stream.open` on the result's media address |
| a magnet or torrent | `fetch.play` | unchanged |

HLS (`.m3u8`, TS and fMP4, on demand or live) and DASH (`.mpd`, on-demand)
are read **inside the
fetch crate**: it parses the manifest, downloads the segments and hands the
player plain bytes with timestamps. The player still never receives a URL, and
it still refuses a manifest if one ever reaches it.

- **One box.** Paste anything into the Downloads box: a video link, a `.m3u8` or
  `.mpd`, a page one of your resolvers understands, a magnet, a `.torrent`, a
  path to a `.json` source, or search words. Drop a `.json` or `.torrent` file
  on the window.
- **Quality.** When a stream has several qualities the player shows a chip
  (AUTO · 720p, then each height; click to cycle). Auto lowers the quality by
  itself after repeated stalls and says so.
- **Seeking** works on on-demand streams; the buffered-ahead part is drawn on
  the timeline.
- **Live HLS** (a playlist without `#EXT-X-ENDLIST`). The player shows a
  `LIVE` chip ("Direct" in French) instead of a timeline: no length, no seek
  bar, no playhead, and seeking, speed and A-B loops are off. Playback starts
  three segments from the live edge. The playlist is reloaded every target
  duration (half of it while nothing changed, never faster than once a second)
  and each segment is played once, tracked by media sequence number. If you
  were away longer than the window, playback resumes at the oldest segment
  the server still lists; a hole, a restarted numbering or
  `#EXT-X-DISCONTINUITY` break the timeline cleanly. A segment that is
  missing is skipped (five in a row end the stream). The quality is fixed at
  the one the master selected (no quality chip). The stream ends cleanly on
  `ENDLIST` or after 12 hours of listening, and with "The live stream ended."
  when the playlist cannot be reloaded three times in a row or does not change
  for two minutes. Only a short tail behind the playhead and the usual
  prefetch (60 s, 64 MiB) are kept in memory; nothing is written to disk.
  Every reload and segment obeys the same address checks and the
  *Block the network without a proxy* switch as any other request.
- **Separate audio and video** (most DASH, HLS `#EXT-X-MEDIA` audio groups).
  The video and audio tracks are fetched by the same reader code (prefetch,
  retries, address checks, proxy switch, cancellation), their init segments
  are merged into one and their fragments are interleaved by time, so the
  player still gets one plain stream and never a URL. If the stream offers
  several audio languages, the one matching the preferred language
  (`StreamOpts::audio_lang`, e.g. `fr` or `pt-BR`) plays, otherwise the one
  marked default, otherwise the first. The quality chip and automatic
  quality change only the video; seeking moves both tracks together.
  Both tracks must be fragmented MP4 (CMAF, the usual DASH case); audio in
  TS, WebM or another container, a live HLS stream with an audio group, and
  DRM are refused with a sentence. Each track keeps its own prefetch
  window, so memory is bounded by two windows (at most 60 s and 64 MiB
  each; audio is a small fraction).
- **Encryption.** Plain AES-128 HLS is supported when the key address is
  reachable like any other address. DRM (SAMPLE-AES, Widevine, PlayReady,
  FairPlay) is refused with a clear sentence, also on a live playlist, also
  when it only appears in a later reload.

## Declarative sources

A source is a JSON file you write and import with `sources.import {path}`:

```json
{
  "name": "my-api",
  "search_url": "https://api.example.invalid/v1/search?q={query}&page={page}",
  "items": "/data/results",
  "fields": { "title": "/name", "media": "/play_url", "size": "/bytes" },
  "auth_header": "X-Api-Key",
  "per_minute": 30
}
```

- Placeholders are `{query}`, `{page}`, `{limit}` only, always percent-encoded.
- Fields are JSON pointers (RFC 6901). No regex, no scripting.
- The key is never read from the file: `sources.secret.set {id, value}` stores
  it in `fetch.json` (mode 0600). It is not shown, exported or logged.
- A source with a key must use https; redirects never leave the host.
- A public source cannot point the app at loopback or private addresses, in
  any spelling a URL parser accepts (`127.1`, `0x7f000001`, `[::ffff:10.0.0.1]`,
  single-label names such as `nas`). A public name that *answers* with a
  private address (DNS rebinding) is refused too, unless the connection goes
  through a SOCKS5 proxy, which resolves names itself. Names on your own
  network (`nas`, `jackett.lan`, the host of an indexer you configured) are
  exempt.
- Search terms never reach the host part of the address: a placeholder there
  is refused.
- `per_minute` is a rate limit (default 30, 0 = none). Be gentle with APIs;
  they remember.

## Resolvers

`resolvers.add {name, argv, timeout_secs?, honours_proxy?}` registers a program
you installed, e.g. `["/usr/bin/tool", "-J", "--", "{url}"]`. The program must
print `{"formats":[{"url": "...", "height": 720}]}`.

- Started from an argument vector: no shell, `{url}` is one whole argument.
- Environment emptied (plus `PATH`, `HOME`, and the proxy), empty working
  directory, stdin closed, 1 MiB output cap, timeout, whole process group killed.
- It never delivers bytes: the address it returns is fetched by this app.
- The app cannot force a proxy on another program. With *Block the network
  without a proxy* on, a resolver runs only if `honours_proxy` is true.
- The program must not be writable by everyone, and must not live in a folder
  that is: such a program is refused when you add it and again when it starts
  (anyone on the machine could have swapped it for something else). Fix with
  `chmod o-w` on the file and its folder.
- Addresses a resolver returns pass the same checks as any other: a public
  address cannot point at loopback or the local network.

### Sandboxing a resolver

A resolver is a program you installed and it runs as you. The app limits what
it is handed (no environment, empty working directory, no stdin) but not what
it can do. To confine it, make the sandbox the program and the tool its
argument. Both examples are starting points: adjust the read-only paths to
your system and test them with the tool by hand first.

bubblewrap, read-only system, private `/tmp`, no home directory, network kept
(a SOCKS5 proxy on loopback needs the host network namespace):

```json
{
  "name": "tool-in-bwrap",
  "argv": ["/usr/bin/bwrap", "--unshare-all", "--share-net", "--die-with-parent",
           "--new-session", "--ro-bind", "/usr", "/usr",
           "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64",
           "--symlink", "usr/bin", "/bin",
           "--ro-bind", "/etc/ssl", "/etc/ssl",
           "--ro-bind", "/etc/resolv.conf", "/etc/resolv.conf",
           "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
           "--", "/usr/bin/tool", "-J", "--", "{url}"],
  "honours_proxy": true
}
```

firejail, private home, no capabilities, no new privileges, IP sockets only:

```json
{
  "name": "tool-in-firejail",
  "argv": ["/usr/bin/firejail", "--quiet", "--private", "--private-tmp",
           "--caps.drop=all", "--nonewprivs", "--noroot", "--seccomp",
           "--nodbus", "--nosound", "--no3d", "--protocol=inet,inet6",
           "--", "/usr/bin/tool", "-J", "--", "{url}"],
  "honours_proxy": true
}
```

Set `honours_proxy` to true only when the wrapped tool really reads
`ALL_PROXY` (the environment passes through both sandboxes). The sandbox
program itself is checked like any resolver: absolute path, not writable by
others. Use `--net=none` (firejail) or drop `--share-net` (bwrap) for a tool
that needs no network because it only parses text.

## Playing a page with your own resolver

Paste the address of a web page. If it is not a media file or a playlist,
each resolver you enabled is asked, in order, what the page contains. The
first one that gives a playable address wins. The tool is yours: nothing is
bundled, named or updated by the app.

What works:

- A page that the resolver turns into one file or one HLS/DASH playlist with
  picture and sound together. The video plays from the network and is not
  written to disk. The window title is the title the page gave (or "Video"),
  kept in memory only, never the file name of the content server.
- *Look for a tool again* (Settings) lists tools already installed under the
  usual names. It runs `--version` on each with an empty environment and a
  5-second limit, and refuses files anyone can modify. It never installs or
  downloads anything. A suggested entry is
  `["/abs/path/tool", "-J", "--no-playlist", "--ignore-config", "--", "{url}"]`.
- `--proxy {proxy}` as two arguments in `argv` is replaced by the proxy in
  force (as `socks5h://`, so the proxy resolves names), or both are left out
  when there is none. A resolver that uses it counts as proxy-aware for
  *Block the network without a proxy*. Note that an argument is visible to
  other users of the same machine in the process list, as is any proxy
  password in it: prefer a proxy without a password on a shared computer.

What is refused, and why:

- **Age confirmation, sign-in, regional blocks and DRM.** JizzCraft does not
  bypass them, does not read your browser's cookies or sessions and never
  signs in for you. The resolver's complaint is reduced to one fixed sentence
  ("requires age confirmation or sign-in on its own website", "requires an
  account", "not available in your region", "protected"). The resolver's own
  text, which can hold the address or the title, is dropped.
- **A file that cannot be read in pieces.** If the content server does not
  honour byte ranges, the only way to play it would be to save it first. For
  web pages the app does not do that: you get "cannot be streamed without
  saving it" and no file, not even a partial one, is left in the download
  folder.
- **Live DASH, DRM-protected streams and separate tracks that are not
  fragmented MP4** are refused with their own sentences (see above).
- **A rate limit** from the site ends the attempt with "limiting requests";
  wait before trying again. The app does not retry in a loop.

When several resolvers fail, the most specific reason is the one shown (DRM,
then age confirmation, sign-in, region, rate limit, unsupported page, gone).
A link that expires during playback is renewed by asking the same resolver
again; if that fails, you see "The link expired."

## The built-in page reader

When an address is a page and not a video, JizzCraft reads the page itself
before (setting `extractor`: `fallback`, the default, asks your resolvers
first) or after your own tools. The reader is generic code. It knows no site.
It looks only at markup any page may carry:

- HTML5 `<video>` and `<source>` elements;
- Open Graph and Twitter player tags (`og:video`, `twitter:player:stream`);
- schema.org `VideoObject` in JSON-LD (a page with several of them lists
  several videos);
- plain media addresses (`.mp4`, `.webm`, `.m3u8`, `.mpd`) written in the page
  text or in the JSON of its scripts. Scripts are read, never run.

If the page shows nothing playable, the reader looks inside at most three
embedded players (`<iframe>`, one level deep, `Referer` set to the page).
Preview clips rank last. Every address it finds must pass the same check as
every other address in the app (`inward_ok`: a public page cannot point the
app at your local network) and the kill switch.

What it does not do, on purpose: it only sends GET requests, keeps no cookie,
submits no form, does not follow a meta refresh, runs no script, and does not
get past age confirmation, sign-in, paywalls or regional blocks. A page that
needs one of those ends in a fixed sentence ("Sign-in needed", "Age
confirmation needed", ...). Limits: 30 seconds per page, 2 MiB read, at most
two page fetches at once. The `User-Agent` is generic and can be changed
(`page_user_agent`); media requests carry only that, `Referer` and `Origin`.

`extract.page {url}` lists what a page holds and returns a token for
`download.start`; it prints no address unless you pass `show_urls`.

## Your own extractor rules

For a page the generic reader does not understand, you can write rules. A rule
is a small JSON file you write yourself; JizzCraft ships none. A rule never
makes a request: it is matched against the page text that was already fetched
and what it finds goes through the same checks as any other address.

```json
{
  "name": "my rule",
  "hosts": ["videos.example.invalid", "*.cdn.example.invalid"],
  "media": [
    {"regex": "player\\.load\\('(?P<url>[^']+)'\\)", "height": 720}
  ],
  "json": [
    {"regex": "var cfg = (?P<json>\\{.*?\\});", "items": "/sources", "url": "/file", "height": "/label"}
  ],
  "title": "<h1>(?P<title>[^<]+)</h1>",
  "unescape": "js"
}
```

- `hosts`: exact names or `*.suffix`; at most 16. The rule runs only on a page
  from one of them, before the generic scan.
- `media`: patterns with a named group `url`; optional `height` and
  `protocol` (`hls`, `dash`, `http`). `json`: a pattern with a named group
  `json`, then RFC 6901 pointers (`items`, `url`, `height`).
- `unescape`: `none`, `js`, `html` or `url`, applied to what a pattern finds.
- Limits: a file of 64 KiB, 32 rules, 8 `media` and 4 `json` patterns per
  rule, patterns of 1024 characters, 64 matches per pattern, 2 MiB of page
  text. Patterns use a regular-expression engine without backtracking, so no
  pattern can stall the reader. Patterns are never printed or logged.

Commands: `extract.rules.import {path}`, `extract.rules.list`,
`extract.rules.enable {id, on}`, `extract.rules.remove {id}`. Rules are kept
in `fetch.json`.

## Saving videos

Streaming writes nothing to disk. Saving is a separate act, and yours:
paste an address (a file, a page, a `.m3u8` or a `.mpd`) and press Download,
or run `download.start {url}`. The same address can be watched and saved at
the same time; they are two separate downloads.

- **Where.** The save folder (`save_dir`, default the download folder), or
  `dir` in the command. Saved videos are added to the library once per
  finished batch (`import_after_save`).
- **Never overwrites.** Each save reserves a free name, writes a `.part` file
  created with `create_new` (mode 0600) and gives it its final name only if
  nothing has appeared under that name meanwhile. A cancelled or failed save
  removes its own `.part` file, checked by inode, and nothing else.
  `crates/fetch/tests/security.rs::downloads_never_overwrite` keeps it so.
- **Names and formats.** The file is named after the page's title, cleaned up;
  with no title (or in discreet or disguised mode) it is called `video`. The
  extension comes from the content. HLS in TS becomes one `.ts` file; fMP4 and
  DASH become one fragmented `.mp4` (separate audio is merged in `fetch`).
  Nothing is remuxed. Live streams are refused.
- **Checks.** The start of the file is sniffed, the length must match what
  the server said, a changed `ETag` or `Last-Modified` stops the resume,
  HLS segments are all required, and the size is checked again after the
  rename. A failure leaves nothing behind.
- **Queue.** Up to `save_concurrency` saves at once (1 to 4, default 2), a
  speed cap shared with torrents, pause, resume and cancel. The queue lives in
  memory: a save resumes after a dropped connection or a pause, but not after
  the app restarts. A signed link that expires is renewed from the page.
- **Disk.** Free space is checked before and every second. When it runs low
  the save pauses with "Disk full. Downloads paused." and keeps what it has;
  it resumes once there is room.
- **Kill switch.** `require_proxy` applies to every request of the page
  reader and of every save. The panic switch pauses saves; changing the proxy
  or the switch ends the session, removes its own `.part` files and reports a
  count.

Saving a video is your decision and your responsibility; see
[LEGAL.md](../LEGAL.md).

## Privacy

- *Block the network without a proxy* (`fetch.settings {require_proxy}`):
  no request, torrent or resolver starts without a SOCKS5 proxy, and changing
  the proxy or the switch ends the running session. Use it with a VPN or Tor
  SOCKS endpoint so a dropped proxy never falls back to a direct connection.
  If `fetch.json` cannot be read or parsed, the switch comes up on (the file
  is set aside as `fetch.json.corrupt-<time>`), so a damaged file never opens
  the network. Set a proxy or turn the switch off to go on.
- Streams write nothing to the library, history, recents or markers.
- The boss key and the panic switch close a stream and cancel any that is still
  opening.
- Discreet and disguised modes show "Video" instead of a title.

## Not done yet

Separate audio that is not fragmented MP4, separate audio on a live stream,
subtitles for streams, resuming a download after the app restarts, quality
switching and seeking inside a live window, and a proxy-aware key fetch UI.
Anything not listed above is refused with "This stream format is not
supported."

## Will not be done

DRM; live DASH and low-latency HLS (`#EXT-X-PART`); recording live streams;
getting past age confirmation, sign-in, paywalls or regional blocks; reading
another browser's cookies; running a page's scripts; per-site code; and
anything bundled: no source, site, host, rule, preset or catalog ships with
JizzCraft. The built-in page reader is generic code that reads standard markup
any page may carry (HTML5 video, Open Graph, schema.org) and knows no site.
