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

HLS (`.m3u8`, TS and fMP4) and DASH (`.mpd`, on-demand) are read **inside the
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
- **Encryption.** Plain AES-128 HLS is supported when the key address is
  reachable like any other address. DRM (SAMPLE-AES, Widevine, PlayReady,
  FairPlay) is refused with a clear sentence.

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
- A public source cannot point the app at loopback or private addresses.
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

Live streams, separate audio and video tracks (most DASH), subtitles, saving a
stream while watching it, and a proxy-aware key fetch UI. Muxed on-demand
streams are what plays today; anything else is refused with "This stream
format is not supported."

## Will not be done

DRM, live DASH and low-latency HLS, and bundled sources of any kind.
