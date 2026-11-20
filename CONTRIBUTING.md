# Contributing

Thanks for helping. vps-watcher stays small on purpose, so please open an issue to discuss a feature before you build it.

## Ground rules

- **Light first.** A change must not add noticeable CPU or memory at idle. Heavy work runs only while someone is looking at it.
- **No new runtime dependencies** for the UI (no frameworks, no CDN), and new crates only when they clearly pay for themselves.
- **Read-only.** vps-watcher observes; it never changes the server it watches.
- **Comments are short.** One or two lines saying what a function does or why, not how.

## Workflow

1. Branch from the latest `dev`: `ft/<topic>` for features, `hotfix/<topic>` for fixes to a release (hotfixes branch from `main`).
2. Run `scripts/verify.sh` (format, lint, tests, UI syntax, shell scripts, release build).
3. Open a pull request into `dev` titled `Merging <branch> into dev`.

`main` always holds the latest release. A release merges `dev` into `main` with a merge commit, then `scripts/tag-release.sh` tags it `vYY.MM.DD.HH.MM` (Riyadh time) and publishes the GitHub Release. CI then attaches the binaries.

## Commit messages

A short summary of what changed and why, then bullet sections that have content:

```text
Adds log search so users can find errors quickly.

**Features:**
- Logs can be filtered by text, with matches highlighted

**Fixes:**
- Very long log lines no longer stretch the page
```

Bullets describe the effect for users in plain language, not class or file names.

## Running locally

vps-watcher reads Linux interfaces (`/proc`, cgroups), so develop on Linux or in a Linux VM or container.

```sh
cargo run                                   # http://127.0.0.1:9100
VPSW_DATA_DIR=/tmp/vw VPSW_INTERVAL=2 cargo run
```

Run it as root, or with the provided systemd unit, to see every process, port and log.

## License

By contributing, you agree that your contributions are licensed under the project's [MIT License](LICENSE), copyright © 2026 Abdulaziz Hafiz.
