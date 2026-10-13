# Security policy

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability
reporting instead:
[Report a vulnerability](https://github.com/kerstz/JizzCraft/security/advisories/new).

Include what you found, how to reproduce it (with synthetic media only), and
the commit or version. We will acknowledge the report as soon as we can; this is
a small volunteer project, so there is no guaranteed response time.

## Supported versions

JizzCraft is early alpha. Only the latest commit on `main` receives fixes.

## Scope

In scope, for example: anything that makes JizzCraft open a network connection
(including through GStreamer, despite the guard in `crates/media/src/guard.rs`),
modify/move/delete user media, run a shell command built from a file name, leak
full paths or file names into logs without `JIZZCRAFT_DEBUG=1`, inject SQL
through filters or saved views, or crash on a crafted file in a way that looks
exploitable.

Out of scope: the age gate, boss key, discreet mode and share-safe mode are
conveniences, not security boundaries. The library database and thumbnail cache
are **not encrypted** today. Bugs in GStreamer, its plugins or the `image`
decoders themselves should be reported upstream.

## Things to know

- Every imported video is parsed by the system GStreamer plugins to make its
  thumbnail, without a sandbox. Keep GStreamer up to date, and treat files from
  untrusted sources the way you would in any media player.
- `--bench-gst-play` launches the external `gst-play-1.0`, which the network
  guard does not cover.
