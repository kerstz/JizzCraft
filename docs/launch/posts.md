# Launch posts

Drafts for 0.2.0. Every claim below is in the README, the CHANGELOG or the
docs; keep it that way if you edit them. Before posting anywhere:

- Read the subreddit's rules on self-promotion and NSFW the same day you post;
  they change. Tag the post **NSFW** where the sub has the flag (the topic is
  adult, even if the repo and the GIF are SFW).
- Link the repository, not a release asset. Use the GIF
  (`docs/media/demo.gif`) or the social preview as the image where images are
  allowed; both are synthetic.
- Say it once: **alpha, Linux is the only tested platform, Windows and macOS
  are experimental, and the Windows files may be missing from a release.**
- Stay in the thread for the first few hours and answer questions; that matters
  more than the wording.
- Space the posts out (one or two a day), and do not ask for stars or upvotes.
- Each sub gets one extra variant, written for its main audience:
  **newcomer angle** (people who want to install and use it) links
  [getting-started.md](../getting-started.md); **builder angle** (people who
  judge the engineering) links [internals.md](../internals.md). Post the
  original *or* the variant, not both. The variants are drafts of the same
  facts; they claim nothing the originals do not.
- Ready-made [first replies](#first-replies) to the five questions most
  likely to come up are at the end.

Repository: https://github.com/kerstz/JizzCraft

---

## r/linuxmemes

The project started from a thread there ("Not another one"); say so, it is
true and it is the joke.

**Title** (pick one):

- `You asked for a keyboard-first, one-handed, offline porn library manager. I wrote it in Rust. It has a boss key.`
- `"Not another one" they said. Anyway, here is another one, but it is offline and written in Rust`

**Body:**

> A while ago a thread here ("Not another one") had a lot of opinions about
> managing a, let's say, *personal* media collection on Linux: one-handed use,
> accessibility, filtering the same things over and over. I took notes.
>
> JizzCraft is a native Rust app (egui + GStreamer + SQLite) that indexes your
> folders without ever touching the files, plays them, and has:
>
> - a left-hand key preset (everything around WASD, AZERTY works too)
> - a boss key: `Ctrl+Shift+H` pauses, mutes, blurs everything, retitles the
>   window "Viewer" and minimizes
> - no network code at all in the default build, and a test that fails if any
>   gets in
>
> Alpha, Linux only for real (Windows/macOS compile, nobody has run them).
> The GIF is rendered by the app from test patterns, so it is safe to open at
> work. The app itself, less so.
>
> https://github.com/kerstz/JizzCraft

**Variant, newcomer angle** (the sub is Linux users, not necessarily
developers; lead with "you can install this in one line"):

> Follow-up to the "Not another one" thread: someone had to.
>
> JizzCraft is a desktop app for the *personal* folder. You point it at a
> folder, it shows everything as a searchable grid, plays it, and never moves
> or renames a single file. Install is one line in a terminal; it asks before
> it installs anything with `sudo`, and `--uninstall` leaves your library
> alone.
>
> - `Ctrl+Shift+H`: pause, mute, blur, retitle the window "Viewer", minimize
> - left-hand mode: the whole thing works around WASD
> - the default version has no internet code at all; a test fails the build
>   if any gets in
>
> Alpha, Linux only for real. A step-by-step guide with a FAQ (where your
> data is, how to uninstall, what to do if a video does not play):
> https://github.com/kerstz/JizzCraft/blob/main/docs/getting-started.md

---

## r/rust

Technical, no jokes in the title. Flair: *Project* (or whatever the sub uses
that week).

**Title:** `JizzCraft 0.2: an offline media library and player in Rust (egui + wgpu, GStreamer, SQLCipher), with "no network" enforced by tests`

**Body:**

> I have been building a desktop media library and player for local adult
> collections (18+; the repo, tests and screenshots are all synthetic and SFW).
> Some of the engineering may be interesting here regardless of the subject:
>
> - **"Offline" is a build property.** All network code lives in one crate
>   behind an off-by-default `network` feature. A test runs `cargo tree` on the
>   default build and fails if an HTTP/TLS/socket crate appears, with a
>   positive control so the check cannot silently pass.
> - **GStreamer cannot phone home either.** At start-up the app removes
>   network-capable elements (HTTP/RTSP/UDP sources, HLS/DASH demuxers) from
>   its own plugin registry and watches every pipeline. A test feeds HLS/DASH
>   playlists disguised as `.mp4` to the player, thumbnailer, prober and
>   previewer, and asserts that a local listener sees zero connections.
> - **The UI thread never touches the pipeline.** A control thread owns
>   GStreamer; egui takes the newest frame from a one-slot mailbox and uploads
>   it into one persistent wgpu texture. No async runtime: `std::thread` and
>   channels.
> - **One command registry** drives the menus, the key bindings, a JSON CLI and
>   `--cmd NAME=JSON` on the desktop app. The README GIF is made that way:
>   `jizzcraft --import fixtures --cmd … --screenshot out.png`, one frame per
>   run.
> - **User data never touches media.** A test hashes a media tree before and
>   after every engine command. The schema is append-only, and a test freezes
>   the old migrations.
> - **Optional encrypted library** with SQLCipher (vendored OpenSSL is allowed
>   for that crate only; the dependency check enforces it). Alpha, off by
>   default, no recovery.
>
> Honest weak spots: colour conversion is still on the CPU, so playback costs
> 3–6× the CPU of `gst-play-1.0` (a YUV→RGB shader is next); Linux is the only
> platform anyone has run; Windows and macOS compile in CI but are untested
> on real hardware, and the Windows files may be missing from a release.
>
> Code review of the threading and the guard is very welcome.
> https://github.com/kerstz/JizzCraft

**Variant, builder angle** (the sub judges code; lead with where to look and
how to check it yourself):

> JizzCraft 0.2 is a desktop media library and player for local adult
> collections (18+; repo, fixtures and screenshots are synthetic and SFW).
> If you only have ten minutes, these are the places worth reviewing:
>
> - `crates/core/tests/offline_deps.rs`: `cargo tree` on the default build
>   must not contain an HTTP, TLS, async-runtime or BitTorrent crate; a
>   positive control checks that the network build does link librqbit;
>   `openssl-sys` is allowed only under `libsqlite3-sys` (SQLCipher).
> - `crates/media/src/guard.rs` and `crates/media/tests/network.rs`: network
>   elements removed from the process's GStreamer registry, every pipeline
>   watched, and a local listener that must count zero connections from the
>   player, thumbnailer, prober, previewer and integrity checker.
> - `crates/core/src/commands.rs`: one registry of
>   `fn(&mut C, Value) -> Result<Value>` commands behind the menus, the key
>   table, the JSON CLI and `--cmd`.
> - `crates/media/src/player.rs`: a control thread owns GStreamer, egui reads
>   a one-slot mailbox; no async runtime outside the opt-in network crate.
>
> Everything CI runs can be run locally; the commands are in
> `docs/internals.md`. Weak spots stated up front: CPU colour conversion
> (3–6× `gst-play-1.0`), GStreamer parses imports without a sandbox, Linux
> is the only platform anyone has run.
> https://github.com/kerstz/JizzCraft/blob/main/docs/internals.md

---

## r/SideProject

**Title:** `I built a private, offline media library for the collection nobody talks about`

**Body:**

> Most media managers want an account, a server, or at least to phone home for
> metadata. For an adult collection that is exactly what you do not want.
>
> JizzCraft is a desktop app that indexes your folders in place (never moves
> or modifies a file), lets you search, rate, tag, triage new imports and play
> them, and keeps everything in one local database. The default build has no
> network code at all. There is a boss key, a discreet mode that blurs
> thumbnails and hides names, and an optional encrypted library.
>
> Stack: Rust, egui, GStreamer, SQLite. Open source (MIT/Apache-2.0).
>
> Status: alpha 0.2.0. Linux is the tested platform; Windows and macOS are
> experimental and untested on real machines. One-line install script for
> Linux, `.deb` on the release page.
>
> Feedback on the first-run experience is what I need most.
> https://github.com/kerstz/JizzCraft

**Variant, newcomer angle** (the sub is makers and curious users; show the
first five minutes):

> What the first five minutes look like:
>
> 1. One line in a terminal installs it for your user only. It asks which
>    edition (Enter = Offline, no internet code) and asks before installing
>    the video codecs it needs.
> 2. First start: an 18+ check, then a short tour you can skip: add a
>    folder, pick privacy options (discreet mode, blur until hover, a
>    neutral "Viewer" name), and a check that your machine can play video.
> 3. Arrows to browse, `/` to search, `Alt+1`–`Alt+5` to rate, `V` to sort
>    new imports one by one, `Ctrl+Shift+H` when someone walks in.
>
> Your files are never moved or changed; ratings and history live in one
> local file. Alpha, Linux-tested. The guide I would like feedback on:
> https://github.com/kerstz/JizzCraft/blob/main/docs/getting-started.md

---

## r/coolgithubprojects

Short; the sub wants a link and a sentence. Check whether it requires a
language tag in the title.

**Title:** `[Rust] JizzCraft: private, offline library and player for your own adult media. No account, no telemetry, no network code in the default build`

**Body (if allowed):**

> Native app (egui + GStreamer + SQLite). Indexes folders without touching
> files, full-text search, ratings, triage, markers and A-B loop, boss key and
> discreet mode, JSON CLI. Alpha, Linux-tested; Windows/macOS experimental.
> The GIF is rendered by the app from synthetic test media.

**Variant, builder angle** (the sub browses repositories; one line on what
makes this repo worth opening):

> Native Rust app (egui + GStreamer + SQLite). "Offline" is a build property
> checked by tests (`cargo tree` on the default build, zero-connection tests
> on every decode path), and every guarantee in the README links the test
> that enforces it. Alpha, Linux-tested. Architecture and how to run the CI
> locally: `docs/internals.md`.

---

## Hacker News (Show HN)

Show HN rules: it must be something people can try, posted by the person who
made it. Plain title, no superlatives. HN flags adult topics quickly, so lead
with the privacy engineering and be matter-of-fact about the subject.

**Title:** `Show HN: JizzCraft – offline media library where "no network" is test-enforced`

(78 characters; HN cuts at 80. If the name gets the post flagged, the alternative is
`Show HN: An offline, no-telemetry media library and player for adult collections`.)

**URL:** https://github.com/kerstz/JizzCraft

**First comment:**

> I built this for a category of media where privacy is not a preference:
> local adult collections (18+). It indexes your folders in place, never
> modifies a file, and keeps ratings, history and markers in one SQLite
> database.
>
> The part I care about most is that "offline" is checked rather than
> promised:
>
> - The default build contains no network crate; a test runs `cargo tree` and
>   fails otherwise. Network features (torrents, user-configured indexers and
>   streaming sources) exist only in a separate opt-in build, with nothing
>   bundled and a proxy kill switch.
> - GStreamer's network-capable elements are removed from the app's plugin
>   registry, and a test feeds disguised HLS/DASH playlists to every decode
>   path while a local listener counts connections. It expects zero.
> - Another test hashes a media tree before and after every command.
>
> It is alpha. Linux is the only platform anyone has run; Windows and macOS
> builds compile in CI but have not run on real hardware, and the Windows
> files may be missing from the release. Playback uses 3–6× the CPU of
> `gst-play-1.0` because colour conversion is still on the CPU.
>
> Written in Rust (egui/wgpu, GStreamer, SQLCipher). MIT/Apache-2.0. The demo
> GIF is rendered by the app from synthetic test patterns.

**Variant first comment, builder angle** (HN readers will want to verify, not
be told; give them the files and the commands):

> Author here. The claims in the README each link the test that enforces
> them, so you do not have to take my word for any of it:
>
> - no network crate in the default build:
>   `crates/core/tests/offline_deps.rs` (`cargo tree`, with a positive
>   control on the network build);
> - zero connections from every GStreamer path, fed disguised HLS/DASH
>   playlists: `crates/media/tests/network.rs`;
> - media never modified: `engine_never_touches_media` in
>   `crates/core/tests/portability.rs`;
> - in the opt-in network build, the proxy kill switch means zero
>   connections: e2e scenario s14 in `tests/e2e/tests/scenarios.rs`.
>
> What is *not* covered is written down too (`docs/audit-untested.md`), and
> every CI job has a local command in `docs/internals.md`. Known limits:
> alpha, Linux only on real hardware, GStreamer parses imports unsandboxed,
> CPU colour conversion. Questions about the threat model welcome.

---

## First replies

Draft answers to the five questions most likely to come up, whatever the
sub. Adapt the tone, keep the facts. Each one says what JizzCraft does, what
it does not, and where to check. When comparing with another project, say
what it is good at and do not guess at what it lacks; if you are not sure,
link its docs instead.

### "Why not Stash / Jellyfin?"

> Both are good at what they do, and if they fit you, use them. They are
> servers you run and then use through a web interface. Stash has scrapers
> and plugins (installed from package sources) and can identify scenes
> against stash-box for metadata; Jellyfin is a general media server with a
> plugin catalog and metadata providers.
>
> JizzCraft is a different shape: one desktop app, no server, no web UI, no
> account, and a default build that contains no network code at all (a test
> checks the dependency tree). It does not fetch metadata and does not
> stream to other devices; it indexes your folders in place, never writes to
> them, and is keyboard-first with a boss key and discreet modes. If you
> want metadata from the web or playback on other devices, it is the wrong
> tool.

Sources for the comparison: their own docs, summarised with dates in
[research/turnkey-landscape-2026-10.md](../research/turnkey-landscape-2026-10.md).
Do not add security comparisons; they age badly and read as dunking.

### "Is it safe?"

> Depends on safe from what, so here is the honest split.
>
> Checked by tests on every change: the default build has no network crate,
> GStreamer paths open zero connections even on disguised playlists, and
> the engine never modifies, moves or deletes your media (it hashes a tree
> before and after every command). Logs carry opaque tags instead of file
> names unless you set `JIZZCRAFT_DEBUG=1`.
>
> Not protected: the library database and thumbnails are in clear unless
> you turn on the encrypted vault, which is alpha and has no recovery.
> GStreamer parses imported videos without a sandbox, like any media
> player. The age gate, boss key and discreet mode are conveniences, not
> security boundaries. It is alpha software.
>
> Scope and private reporting: SECURITY.md. Guarantees with their tests:
> docs/internals.md.

### "Windows?"

> It compiles, and CI builds, lints, tests and screenshots it with a
> software renderer, but nobody has run it on a real Windows machine yet.
> The portable zip and installer are experimental and can be missing from a
> release when that job fails. If you try it, a `jizzcraft-cli doctor
> --report` in issue #14 helps a lot (it contains no names, paths or URLs).
> Notes: docs/windows.md.

### "Why Rust?"

> Mostly because of what it let me check rather than promise. The network
> code sits behind a Cargo feature, so the Offline build does not contain
> it at all, and `cargo tree` turns "no network" into a test. Threads and
> channels keep GStreamer off the UI thread without an async runtime.
>
> To be fair about it: "written in Rust" covers this repository only. Video
> decoding is the system's GStreamer and the database is SQLCipher with a
> vendored OpenSSL, all C.

### "Really no network at all?"

> In the default (Offline) build: no HTTP, TLS, socket-library, async
> runtime or BitTorrent crate in the dependency tree (test:
> `crates/core/tests/offline_deps.rs`), GStreamer's network elements
> removed from the app's registry, and a test that counts zero connections
> from the player, thumbnails, probes, previews and integrity checks.
> No update checks, no telemetry.
>
> The honest footnotes: the installer downloads from GitHub and your
> package manager downloads codecs; files on a network mount are read
> through your OS like any file; `--bench-gst-play` runs the external
> `gst-play-1.0`, which the guard does not cover; the accessibility layer
> talks to the local session bus (not the network). The separate Online
> edition does connect, only after you act, to sources you configure.
