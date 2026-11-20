# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Copilot and others) working in this repository or installing vps-watcher on a server. People are welcome to read it too.

## Two rules that come first

1. **Never expose secrets.** Do not print, read aloud, copy, commit, upload or paste the contents of `/etc/vps-watcher/vps-watcher.env`, any `.env` file, or the files in `/var/lib/vps-watcher/`. That covers chat, logs, issues, pull requests and test fixtures. Never put a secret value in a command argument. If you install or configure vps-watcher on a server, follow [docs/ai-install.md](docs/ai-install.md) step by step.
2. **Keep it light.** vps-watcher's promise is a ~3 MB binary using about 5 MB of RAM and close to 0% CPU. Any change that adds a dependency, a thread, a timer or a per-sample allocation must justify its cost. Do not add a crate when about 50 lines of code will do.

## What this project is

vps-watcher is a free, open-source monitor for one Linux VPS, written in Rust. One static binary runs as a hardened systemd service. It samples the host, Docker containers and systemd services every few seconds, keeps 30 days of history in fixed-size rings, and serves a small web UI. It sends alerts by email (SMTP) and webhook.

**Maintainer:** Abdulaziz Hafiz ([AHafiz.sa](https://ahafiz.sa), abdulazizhafez2004@gmail.com). **License:** MIT, copyright © 2026 Abdulaziz Hafiz. Keep the copyright notice in `LICENSE`. Never remove or change author or license notices.

## Repository map

| Path | Responsibility |
|---|---|
| `src/main.rs` | CLI: `serve`, `config`, `set`, `test-alert`, `proxy`, `reset-admin`, `version` |
| `src/config.rs` | Settings from the environment and the env file (`VPSW_*`), validated once at start |
| `src/setcmd.rs` | `vps-watcher set KEY`: safe edits of the env file, with hidden prompts for secrets |
| `src/collect/` | The sampler. `mod.rs` builds a `Snapshot` every interval. `host.rs` parses `/proc`, `cgroup.rs` reads per-app usage, `docker.rs` is a read-only Docker client over the Unix socket, and `systemd.rs`, `logs.rs`, `procs.rs` and `ports.rs` handle services, logs, processes and ports. `checks.rs` runs the monitors users add |
| `src/store/` | History: three ring tiers per series (`ring.rs`), binary persistence (`persist.rs`) |
| `src/alert/` | Alerts. `engine.rs` is a pure state machine over snapshots, `settings.rs` parses settings and holds the `Secret` type, `smtp.rs`, `oauth.rs` and `webhook.rs` deliver, `html.rs` renders emails, and `mod.rs` batches, retries and rate-limits |
| `src/net.rs` | Outbound HTTP/1 client and verified TLS (rustls with webpki roots), shared by monitors and alerts |
| `src/web/` | axum server: `mod.rs` (accept loop, security middleware, routes), `api.rs` (JSON handlers), `assets.rs` (UI compiled into the binary) |
| `src/auth.rs` | Admin account (argon2id), sessions, login rate limits, the localhost-only first-run setup |
| `src/update.rs` | Daily release check, and `vps-watcher upgrade`: signed-manifest verification, swap, rollback |
| `src/report.rs` | Averages, peaks, availability and disk forecasts; CSV |
| `src/proxy.rs` | Caddy and nginx config generators |
| `web/` | The UI: `index.html`, `app.js` (plain JS, no dependencies), `app.css`, `logo.svg` |
| `packaging/` | systemd unit and the default env file the installer ships |
| `install.sh` | Installer, upgrader and uninstaller (POSIX sh) |
| `scripts/` | `verify.sh` (all local checks), `tag-release.sh` (releases), `sign-release.sh` (signs a release with the offline key), `make-signing-key.sh` (one-time key creation) |
| `docs/` | `design.md` (architecture and API), `alerts.md` (alert setup), `ai-install.md` (server setup for agents) |

## How it fits together

```
/proc, /sys/fs/cgroup, Docker socket, systemctl
        │ every VPSW_INTERVAL seconds (default 5)
        ▼
 collect::Collector::tick ──► Snapshot ──► History rings (saved every 5 min)
        │                        │
        │                        ├──► alert::Engine::evaluate ──► queue ──► email / webhook
        │                        └──► web: /api/overview, SSE /api/live
        ▼
 monitors (checks.rs, every 30 s)
```

- **One thread.** A `current_thread` tokio runtime with at most 2 blocking threads. Do not hold a `std::sync::Mutex` guard across an `.await`. Do not block the runtime: file writes that may fsync go through `spawn_blocking`.
- **Read-only to the system.** vps-watcher only reads `/proc`, cgroups and the Docker API (GET only). It never starts, stops or changes anything it monitors.
- **Snapshots are immutable.** Each tick builds a new `Arc<Snapshot>`. Anonymous viewers in open mode get `Snapshot::public()`, which redacts monitor targets. Any new field that can carry a URL, path or command line must be redacted there too.
- **Alerts are pure state.** `Engine::evaluate(&Snapshot)` returns events. Conditions must hold for `fire_after` seconds to fire and `clear_after` to clear. State lives in `alerts.json` (mode 0600), so restarts never re-send.
- **Settings are env only.** Every setting is a `VPSW_*` variable, read by `Config::from_lookup` or `AlertConfig::from_vals`. To add one:
  1. Parse and validate it.
  2. Add it to `setcmd::KEYS` (and `SECRET_KEYS` if it is a secret). A test fails if you forget.
  3. Add it to `packaging/vps-watcher.env` with a one-line comment.
  4. Add it to the README settings table and, for alerts, to `docs/alerts.md`.

## Build, test and verify

vps-watcher targets Linux only. Tests read `/proc` and use Unix APIs, so on macOS or Windows run them in a Linux VM or container.

```sh
scripts/verify.sh          # fmt check, clippy -D warnings, tests, JS syntax, shellcheck, release build
cargo test                 # unit tests only
cargo build --release --target x86_64-unknown-linux-musl   # the static binary releases ship
```

- **Toolchain:** stable Rust. The minimum supported version (MSRV) is **1.88** (edition 2024, let-chains), and CI checks it. Do not use newer standard-library APIs without raising `rust-version`.
- **Before calling a change done:**
  1. Run `scripts/verify.sh`.
  2. For anything user-visible, run the binary on a Linux machine with Docker and look at the UI in a browser.
  3. Report measured numbers, not guesses.

## Code style

- `rustfmt.toml` sets a 120-column width. `cargo clippy --all-targets -- -D warnings` must pass.
- **Comments.** Every function gets a one- or two-line doc comment saying what it does or why. No comment essays, and no comments that restate the code.
- **Edge cases first.** Counters wrap, files vanish, Docker goes away, clocks jump backwards, servers send garbage. Never `unwrap()` on anything from outside the process. Return `Result<_, String>` whose message a user can act on.
- **Bounded everything.** Reads from sockets, files and HTTP replies have size limits and timeouts. Collections that grow with input have caps.
- **Secrets** use `alert::settings::Secret`, whose `Debug` output hides the value. Error messages must not include passwords, tokens or webhook URLs; name the host instead. Add a test that a secret does not leak when you touch these paths.
- **Web UI.**
  - Plain JS with no dependencies and no build step.
  - The CSP forbids inline scripts and styles in HTML.
  - Build DOM with `h()` and `fill()`. Server text goes through `textContent`, never `innerHTML`.
  - Every color comes from a CSS token defined in both themes.
- **Email HTML** (`src/alert/html.rs`) escapes every dynamic string with `esc()` and uses inline styles and tables only.

## Security invariants (do not break)

- **Network exposure.** Listens on `127.0.0.1` by default. The admin can be created only from a direct loopback request with no proxy headers and a loopback `Host`.
- **CSRF and headers.** State-changing requests must be JSON, with `Origin` matching `Host`. The strict CSP and security headers apply to every response.
- **Request limits.** Request bodies are capped at 16 KB, headers must arrive within 10 seconds, and each request has a 30-second budget.
- **Passwords and sessions.** argon2id passwords, rate-limited logins and HttpOnly SameSite=Strict session cookies.
- **Log files** open with `util::open_regular`, which refuses symlinks, FIFOs and devices.
- **Outbound TLS** is always verified. SMTP refuses data sent before the TLS handshake, and never sends credentials unencrypted to another host.
- **The systemd unit's hardening** in `packaging/vps-watcher.service` stays as strict as it is.

## Git and releases

- **Branches.** Branch from `dev` as `ft/<topic>`, or from `main` as `hotfix/<topic>`. Never push directly to `main` or `dev`.
- **Pull requests** are titled `Merging <source> into <target>` and merged with a merge commit, never a rebase.
- **Commit messages.** A 1–4 sentence plain-language summary, then **Features:** and/or **Fixes:** bullets. Use no code identifiers.
- **Releases.**
  1. `dev` is merged into `main`.
  2. `scripts/tag-release.sh` tags `main` as `vYY.MM.DD.HH.MM` (Riyadh time) and publishes the GitHub release.
  3. The release workflow attaches static musl binaries for x86_64 and aarch64.
  4. The same script then runs `scripts/sign-release.sh`, which signs `SHA256SUMS` on the maintainer's machine.
- **Unsigned releases are never installed** by `vps-watcher upgrade` or automatic updates.
- **The signing key** lives only at `~/.config/vps-watcher/release-signing-key.pem` on the maintainer's machine. Never read, print, copy, upload or commit it, and never move signing into CI. `RELEASE_KEY` in `src/update.rs` is its public half; change it only together with a key rotation.
- **Tags are never moved.**
- **Never commit secrets:** `.env` files, real credentials, tokens, or `git-guide.md` (it is ignored on purpose).
