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
