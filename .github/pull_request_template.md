<!-- Thanks for contributing. First pull request? CONTRIBUTING.md and AGENTS.md have the rules, and docs/internals.md lists every CI job with the command that runs it locally. Small, focused pull requests get reviewed fastest. -->

## What and why

<!-- One or two sentences. Link the issue: "Closes #123". -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] If it touches the network crate: also with `--features jizzcraft/network,jizzcraft-cli/network`
- [ ] The default (Offline) build still links no network code (`offline_deps` test)
- [ ] User data stays in the database; media files are never modified
- [ ] No names, paths, URLs or keys in logs, toasts or errors; discreet modes checked
- [ ] New strings go through `tr()` and are translated (fr, es, de, it, pt) or listed as missing
- [ ] Tests and fixtures are synthetic and safe for work
- [ ] No preset site, indexer, catalog or tracker added
