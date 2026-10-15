# GitHub repository settings (to apply by hand)

Nothing here is applied automatically. Each block says where it goes.

## About (Settings → General, or the gear next to "About")

**Description** (max 350 characters; this one is 135):

```
Private, offline library and player for your own adult collection. Rust, egui, GStreamer, SQLite. No account, no telemetry. Linux, 18+.
```

**Website:** leave empty, or the latest release:
`https://github.com/kerstz/JizzCraft/releases/latest`

**Topics** (GitHub allows 20; lowercase, hyphens only):

```
rust egui wgpu gstreamer sqlite sqlcipher media-library media-player
video-player image-viewer privacy offline-first local-first linux
desktop-app no-telemetry keyboard-driven accessibility cli nsfw
```

Notes on the list:

- `nsfw` is honest and helps people who are looking for exactly this find it,
  and lets those who are not filter it out. Drop it if you prefer the repo not
  to surface in NSFW topic pages.
- `accessibility` is backed by the left-hand preset and
  [docs/accessibility.md](../accessibility.md); keep it only while that stays
  true.
- Avoid `windows` and `macos` until a real-hardware run exists (issues #14, #15).

Gh CLI equivalent (run it yourself; it changes the public repository):

```sh
gh repo edit kerstz/JizzCraft \
  --description "Private, offline library and player for your own adult collection. Rust, egui, GStreamer, SQLite. No account, no telemetry. Linux, 18+." \
  --add-topic rust,egui,wgpu,gstreamer,sqlite,sqlcipher,media-library,media-player \
  --add-topic video-player,image-viewer,privacy,offline-first,local-first,linux \
  --add-topic desktop-app,no-telemetry,keyboard-driven,accessibility,cli,nsfw
```

## Social preview (Settings → General → Social preview)

Upload [`docs/media/social-preview.png`](../media/social-preview.png)
(1280 × 640, PNG, under 1 MB). It is rendered from the app's own screenshot of
synthetic fixtures, like the README GIF; regenerate both with
`docs/media/make-demo.sh`.

## Features (Settings → General → Features)

- **Issues**: on. The templates in `.github/ISSUE_TEMPLATE/` cover bugs,
  feature requests and maintainer-filed starter tasks (`good_first_issue.yml`).
- **Discussions**: optional. Turning it on gives questions a home that is not
  the bug tracker; if you do, add a `contact_links` entry for it in
  `.github/ISSUE_TEMPLATE/config.yml`.
- **Wiki**: off (docs live in `docs/`).
- **Sponsorships**: only if you have a funding account; then add
  `.github/FUNDING.yml`.

## Labels

Create these if they do not exist yet (the templates and `release.yml` use them):

| Label | Colour | Used by |
| --- | --- | --- |
| `good first issue` | `#7057ff` (GitHub default) | `good_first_issue.yml`, CONTRIBUTING |
| `help wanted` | `#008672` (GitHub default) | tasks that need hardware or a distro you do not have |
| `edition: offline`, `edition: online`, `edition: both` | any | `docs/feature-process.md` |

```sh
gh label create "good first issue" --color 7057ff --description "Small, self-contained, says how to check it" --force
gh label create "help wanted" --color 008672 --description "Needs hardware, a distro or a language we do not have" --force
```

## Starter issues worth filing

Each one is real today (checked against the README status block, ROADMAP.md and
docs/audit-untested.md at 0.2.0). File them with the *Good first issue* template.

1. **Build the Fedora package on a real Fedora** and report what breaks
   (`packaging/fedora/jizzcraft.spec`; no prebuilt rpm exists). `help wanted`.
2. **Build the Arch package** from `packaging/arch/jizzcraft` in a clean
   chroot and fix what `namcap` reports (#16 is the AUR publication).
3. **Proofread one interface language** in `crates/ui/src/i18n.rs`
   (French, Spanish, German, Italian or Portuguese).
4. **Run a Windows build on real hardware** and attach
   `jizzcraft-cli doctor --report` (#14). `help wanted`.
5. **Run a macOS build on real hardware** (see #15). `help wanted`.
6. **Document one troubleshooting case** you hit in
   `docs/troubleshooting.md`.

## Branches

After the edition branches are deleted, `main` is the only long-lived branch.

- Set `main` as the default branch and protect it (require the CI checks,
  including *e2e scenarios*).
- **Before deleting `edition/offline` and `edition/online`**: the release
  workflow still fast-forwards them. `.github/workflows/release.yml` (job
  `channels`) and `.github/workflows/channels.yml` both
  `git push origin "<sha>:refs/heads/edition/…"`, which would recreate the
  branches at the next release. Remove that job and that workflow in the same
  change that deletes the branches.
