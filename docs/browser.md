# The browser helper (`jizzcraft-browser`)

An optional, separate program. JizzCraft itself contains no browser engine,
no HTTP client and no web view; `offline_deps` proves that no app or library
crate depends on the helper, gtk4 or webkit6.

## What it is

A small window with an address entry, back, forward, reload and a
"Play this page" button, drawn by WebKitGTK 6.0. "Play" sends the page (or a
right-clicked link, "Play link in JizzCraft") to the app, which vets the URL
(`check_play_url`: http/https only, no credentials, no local or private host)
and plays it in its own player.

It is started only by the app, with an argument list (never a shell), and
speaks the line protocol of `crates/bridge` over stdin/stdout. The first line
must be `Hello` within 2 seconds. Started from a terminal, or with no Hello,
it prints "Start it from JizzCraft." and exits 2, so the 18+ gate in the app
always stands in front of it.

## Isolation model

- Separate process, no shared memory, no files shared with the app. The only
  channel is the bridge; lines are capped at 8 KiB and unknown messages are
  ignored.
- Session in memory by default. A 0700 folder
  `$XDG_RUNTIME_DIR/jizzcraft-browser-<pid>` holds only the content-rule
  store; it is wiped on exit and leftovers of dead helpers are wiped on the
  next start. Without `$XDG_RUNTIME_DIR` nothing touches disk.
- "Remember" (`Hello.remember`) keeps cookies and site data in
  `$XDG_DATA_HOME/jizzcraft/browser` (0700). History is never saved.
- The helper never writes outside its profile or temp folder.
- Proxy: `Hello.proxy` (`socks5://host:port`) goes to WebKit as custom proxy
  settings, never on a command line. With `require_proxy` and no proxy it
  sends `Error{proxy_required}` and exits (4) before any network use.

## What it never does

- Open popups or new windows (`create` returns nothing; scripts cannot open
  windows).
- Download files: every download is cancelled and the page shows "The browser
  does not download files."
- Grant any permission: camera, microphone, location, notifications, pointer
  lock and clipboard requests are all denied.
- Use WebRTC, EME/DRM or autoplay. Media and JavaScript follow `Hello.media`
  and `Hello.js`. (WebKitGTK 6.0 has no DNS-prefetch or form-autofill feature
  to enable.)
- Navigate top-level to anything but http, https or about:blank
  (file:, data:, javascript:, blob: are refused).
- Bundle any block list. Content blocking comes only from the user's own rules
  file (`Hello.rules_path`, WebKit content-rule JSON).
- Contact anything before the user acts: the start page is local and blank.
- Offer a search engine. Typed words with no search template show "Type an
  address. No search engine is set; you can add your own in Settings."

## Discreet behaviour

The window title is always "Viewer", never the page title. With
`Hello.disguised` the application id is `local.Viewer`. The application is not
single-instance (`NON_UNIQUE`). `Discreet{on}` hides and mutes the page.
Ctrl+Shift+H sends `Boss` and exits immediately. With `Hello.software` the
helper sets `WEBKIT_DISABLE_DMABUF_RENDERER=1`.

## Installation

A separate package (`jizzcraft-browser`), recommended by the online edition,
never in the offline one. WebKitGTK is LGPL-2.1 and linked dynamically from
the distribution; it is never bundled in any JizzCraft archive. The helper's
crates (gtk4, webkit6) are MIT. Build: `cargo build --release -p
jizzcraft-browser --features engine` (needs the WebKitGTK 6.0 and GTK 4
development files). Without `engine` the binary is a stub that only answers
Hello with `Error{engine_missing}` and exits 3.

## Failure modes

| Exit | Error code | Meaning |
|------|------------|---------|
| 2 | none | not started by the app / no Hello in 2 s |
| 3 | `engine_missing` | built without the engine |
| 4 | `proxy_required` | `require_proxy` without a proxy |
| 5 | `bad_hello`, `bad_version`, `bad_proxy`, `bad_rules_path`, `bad_search_template` | invalid Hello |
| 1 | `no_display`, `profile_unwritable` | no display or unwritable profile |

On any error the app shows one fixed sentence; nothing the page said is echoed.

## Verified

Run on 2026-10-14, Arch/CachyOS, Wayland session, WebKitGTK 6.0 2.54.1,
gtk4 0.11, webkit6 0.6.1 (debug build, `--features engine`).

Self-test (hidden; needs `JIZZCRAFT_BROWSER_SELFTEST=1` and `--self-test`; no
Hello, no stdin): the helper serves a page from two loopback servers inside its
own process and prints one JSON line per check. Exit 0 only if all hold.

```
JIZZCRAFT_BROWSER_SELFTEST=1 cargo test -p jizzcraft-browser \
  --features engine --test engine_selftest -- --ignored
```

Loopback is reachable in this mode only. The helper's navigation filter looks
at the scheme, never the host, so nothing is relaxed there; the "no local or
private host" rule is the app's `check_play_url`, which the self-test does not
touch.

All 14 checks passed: popup (window.open returns null, no request reaches the
server), download (cancelled, notice shown), geolocation (error code 1),
notifications (denied, none shown), camera/microphone (getUserMedia absent),
file: navigation (filter refuses `file:///etc/hostname`), cross-origin fetch
(read blocked), WebRTC absent, EME absent, autoplay (NotAllowedError), window
title "Viewer" while the page title is different, app id `local.Viewer`,
ephemeral session, Ctrl+Shift+H handler.

Proxy (loopback SOCKS5 trap, `Hello.proxy` = `socks5://127.0.0.1:19050`):
page loads to `socks-trap-target.invalid`, `127.0.0.1` and `localhost` all
arrived at the proxy only; the domain names were sent as domain names (ATYP 3),
and nothing connected directly to the target. With the proxy port closed the
load failed ("Could not connect to proxy server") and did not fall back to a
direct connection.

### Known limits

- The Boss shortcut handler was run by emitting the key controller's signal,
  not by a real keystroke.
- Popups were tried from script without a user gesture; WebKit blocks those
  itself. The `create` handler (returns nothing) and the new-window policy
  refusal are the second layer; a gesture-driven popup was not injected.
- A cross-origin `fetch` is not stopped, only its result is unreadable: the
  request itself reaches the server (same-origin policy, not a helper rule).
- WebKit itself refuses `file:` from an http page before the policy hook; the
  helper's filter was driven separately with a direct `load_uri`.
- Absence of DNS leaks was inferred from loads of an unresolvable name
  succeeding through the proxy, not from a packet capture.
- An unreadable or invalid rules file is ignored silently (no content rules).
- The self-test suite does not exercise "remember" persistence.
