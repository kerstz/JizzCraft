# How features are proposed, chosen and shipped

Everything about features lives on GitHub, in the open. Nothing in the app ever
contacts GitHub: no update checks, no feature flags fetched at run time, no
telemetry. Features are decided here, shipped in releases, and that is all.

## Ask for one

1. Search the [feature requests](https://github.com/kerstz/JizzCraft/issues?q=is%3Aissue+label%3A%22type%3A+feature%22).
   If yours exists, add a 👍 reaction instead of a comment; reactions are the vote.
2. Otherwise open a **Feature request**. The form asks which edition and area,
   the problem you are trying to solve, and confirms the project rules.
3. Do not attach real media, file names, paths, URLs or keys.

## What happens next

| Label | Meaning |
| --- | --- |
| `status: needs-triage` | Not looked at yet. |
| `status: needs info` | Waiting for details from you. |
| `status: planned` | Accepted. It has a milestone. |
| `status: in progress` | Someone is working on it. |
| `status: blocked` | Waiting on something (named in the issue). |
| `rule: out of scope` | Conflicts with a project rule; closed with an explanation. |

Priorities: `priority: p0` blocks the next release, `p1` is important, `p2`
is nice to have. Milestones: `v0.1.0 (alpha)`, `v0.2.0`, `Later`.
The reasoning behind the order is in [ROADMAP.md](../ROADMAP.md).

## Rules every feature must respect

- No accounts, no telemetry, no cloud sync.
- No bundled site, indexer, tracker, catalog or preset; the user supplies them.
- The **Offline** edition links no network code (a test enforces it).
- Nothing bypasses another site's age check, sign-in, regional restriction or DRM,
  and cookies are never read from other browsers.
- User data stays in the database; media files are never modified.
- Discreet, disguised and share-safe modes must not leak names.

## Editions and labels

Issues carry `edition: offline`, `edition: online` or `edition: both`. The
Offline edition only receives features that need no network code.

## Working on one

Comment on the issue before starting, branch from `main`, and use the pull request
template. Pull requests are labelled by the files they touch. Release notes are
grouped by label (see `.github/release.yml`), so the labels on a merged pull
request become the changelog sections.
