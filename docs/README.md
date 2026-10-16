# Documentation

Pick the path that fits you. Both lead to the same facts; they just start in
different places.

## I want to install and use it

Start with **[getting-started.md](getting-started.md)**: what JizzCraft is,
the one-line install and what it prints, the first launch step by step, the
everyday keys, and a FAQ (where your data lives, whether it changes your
files, whether it connects to the internet, how to uninstall, what to do when
a video does not play).

Then, when you need them:

| Page | For |
| --- | --- |
| [troubleshooting.md](troubleshooting.md) | `doctor` checks, missing codecs, a window that does not open, crash notes |
| [organise.md](organise.md) | playlists, categories and tags |
| [accessibility.md](accessibility.md) | screen readers, zoom, contrast, reduce motion, and what is not verified |
| [editions.md](editions.md) | Offline or Online, and switching between them |
| [vault.md](vault.md) | the optional encrypted library (alpha, read the threat model first) |
| [interop.md](interop.md) | content hashes and what is read from other tools, and what is refused |
| [windows.md](windows.md) | Windows (experimental, never run on real hardware) |
| [streaming.md](streaming.md), [browser.md](browser.md) | Online edition: your own sources, resolvers, the optional browser |

## I want to see how it is built

Start with **[internals.md](internals.md)**: crates and dependency rules,
the runtime model, every guarantee with the test that enforces it, the command
registry and JSON CLI, `cargo xtask`, the threat model and known limits,
performance budgets, and how to run each CI job locally.

Then:

| Page | For |
| --- | --- |
| [../AGENTS.md](../AGENTS.md) | the rule book for contributors and coding agents |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | first contribution, checks before a pull request |
| [../SECURITY.md](../SECURITY.md) | reporting a vulnerability, scope |
| [audit-untested.md](audit-untested.md) | coverage, and what is not tested and why |
| [e2e.md](e2e.md) | the 18 end-to-end scenarios |
| [perf.md](perf.md), [bench.md](bench.md) | performance budgets, playback benchmark |
| [vault.md](vault.md) | vault design, key handling, conversion and recovery |
| [streaming.md](streaming.md), [browser.md](browser.md) | Online edition internals and isolation |
| [licenses.md](licenses.md) | licences of what ships |
| [feature-process.md](feature-process.md) | how features are proposed, chosen and shipped |

## Project pages

- [launch/](launch/posts.md): launch post drafts and the
  [GitHub settings](launch/github-settings.md) to apply by hand.
- [research/](research/turnkey-landscape-2026-10.md): background research.
- [../ROADMAP.md](../ROADMAP.md), [../CHANGELOG.md](../CHANGELOG.md).
