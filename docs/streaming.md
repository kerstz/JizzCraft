# Streaming from your own sources

JizzCraft ships **no** source, site, indexer, catalog or preset. Everything
below is something you configure yourself, in the network build
(`--features network`). Nothing connects before you act.

## What plays

| You give | Command | Result |
| --- | --- | --- |
| a direct `http(s)` video address | `stream.open {url}` | seekable playback through `Range` requests, nothing written to disk. A server that ignores `Range` falls back to save-while-playing. |
| a page address and a resolver you installed | `stream.resolve {url, resolver}` | the resolver prints file addresses, the app streams the best one |
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
- **Separate video and audio tracks, live DASH and DRM-protected streams** are
  refused with their own sentences (see above).
- **A rate limit** from the site ends the attempt with "limiting requests";
  wait before trying again. The app does not retry in a loop.

When several resolvers fail, the most specific reason is the one shown (DRM,
then age confirmation, sign-in, region, rate limit, unsupported page, gone).
A link that expires during playback is renewed by asking the same resolver
again; if that fails, you see "The link expired."

## Privacy

- *Block the network without a proxy* (`fetch.settings {require_proxy}`):
  no request, torrent or resolver starts without a SOCKS5 proxy, and changing
  the proxy or the switch ends the running session. Use it with a VPN or Tor
  SOCKS endpoint so a dropped proxy never falls back to a direct connection.
- Streams write nothing to the library, history, recents or markers.
- The boss key and the panic switch close a stream and cancel any that is still
  opening.
- Discreet and disguised modes show "Video" instead of a title.

## Not done yet

Separate audio and video tracks (most DASH), subtitles for streams, saving a
stream while watching it, quality switching and seeking inside a live window,
and a proxy-aware key fetch UI. Muxed on-demand
streams are what plays today; anything else is refused with "This stream
format is not supported."

## Will not be done

DRM, live DASH and low-latency HLS (`#EXT-X-PART`), and bundled sources of
any kind.
