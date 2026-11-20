# vps-watcher design

vps-watcher is a free, open-source monitor for one VPS. You install one binary on the server. It finds what runs there (Docker containers, systemd services, listening ports, disks, network interfaces) and shows usage, history, logs and reports in a small web UI.

## Goals

- **Light.** One static binary. Under 15 MB of RAM and close to 0% CPU when idle on a typical VPS.
- **Zero setup.** It finds apps on its own, with no AI and no agents. Users can add their own monitors on top.
- **Complete.** Covers CPU, memory, swap, disk space and inodes, disk I/O, network, load, CPU steal and I/O wait, per-app usage, availability checks, logs, 30-day history and reports.
- **Secure by default.** Listens on `127.0.0.1` only. You create the admin on first visit. Login uses an email and password, and the password is hashed with argon2id.

## Non-goals (v1)

- **More than one server.** vps-watcher watches one server per install. There is no hub and no agents.
- **Built-in TLS.** A reverse proxy (Caddy or nginx) handles HTTPS. vps-watcher prints a ready config for either.
- **Alert delivery.** No email or webhook alerts in v1. They are planned next.
- **Windows and macOS hosts.** vps-watcher runs on Linux only.

## How it works

```
 /proc, /sys/fs/cgroup ──┐
 Docker socket (GET)  ───┼──► collector (every 5s) ──► history (RAM rings, saved to disk)
 systemctl, journalctl ──┘                                  │
                                                           ▼
            browser ◄── HTTP + SSE ◄── axum server ◄── reports, logs, on-demand scans
```

- **Runtime.** A single-threaded tokio runtime. Very little runs on a timer, so one thread is enough.
- **Host metrics.** Read straight from `/proc`: `stat`, `meminfo`, `loadavg`, `uptime`, `net/dev`, `diskstats` and `self/mountinfo`, plus `statvfs`.
- **Per-app metrics.** Read from each app's cgroup: `cpu.stat`, `memory.current`, `memory.max`, `io.stat` and `pids.current`. Containers and services each have their own cgroup, so the numbers are nearly free. vps-watcher never calls Docker's stats API, which costs the Docker daemon CPU for every container.
- **Docker.** A minimal HTTP/1 client over the Unix socket. It only calls `GET /containers/json`, `GET /containers/{id}/json` and `GET /containers/{id}/logs`.
- **systemd.** Services are found by walking `/sys/fs/cgroup/system.slice/*.service`. Failed units come from `systemctl --failed`, checked every 30s.
- **On-demand scans.** The process list and listening ports (from `/proc/net/tcp*` plus `/proc/*/fd`) are read only while someone is looking at them.
- **Monitors you add.** TCP port, HTTP(S) URL, process match (name or command line) and log file.

## History

- Each series keeps three ring tiers of `f32` values:
  - 5s steps for 1 hour (720 points)
  - 1-minute steps for 48 hours (2880 points)
  - 15-minute steps for 30 days (2880 points)
- A series costs about 26 KB.
- Buckets are aligned to wall-clock time. Gaps are stored as NaN. A clock that jumps backwards never rewrites history.
- Tiers 1 and 2 are written to `history.bin` every 5 minutes and on shutdown. The write goes to a temp file first, then a rename.
- Series idle for 30 days are dropped. At most 1500 series are kept, so a CI box that creates thousands of short-lived containers cannot exhaust memory.

## Security

- **Access modes** (`VPSW_AUTH`):
  - `login` (the default): every page needs the admin to be signed in.
  - `open`: anyone can view. Only the admin can change things. Logs stay private unless `VPSW_OPEN_LOGS=true`.
- **First-run setup.** With no admin yet, the UI shows *Create admin*. It accepts only direct loopback requests without proxy headers. So the account is created over `127.0.0.1:9100`, never through a public domain.
- **Passwords.** Hashed with argon2id (19 MiB memory, 2 passes). The minimum length is 12 characters.
- **Sessions.** 32 random bytes in an HttpOnly, SameSite=Strict cookie. The cookie is Secure behind HTTPS. Sessions expire after 7 days idle.
- **Login limits.** 10 failed attempts per IP per 15 minutes, then the IP is blocked.
- **Forgot password.** `vps-watcher reset-admin` on the server.
- **CSRF.** Every write must be JSON. A browser cannot send a cross-site JSON request without a preflight, and the server never answers one. It also checks that `Origin` matches `Host`.
- **Response headers.** A strict CSP, so no inline scripts. Also `frame-ancestors 'none'`, `nosniff` and `no-referrer`.
- **Proxies.** `X-Forwarded-*` headers are trusted only from the addresses in `VPSW_TRUSTED_PROXIES`.
- **Least privilege.** The service runs as user `vps-watcher` under a hardened systemd unit. It gets read-only Docker access through the `docker` group, which is effectively root on that host. It gets `CAP_DAC_READ_SEARCH` and `CAP_SYS_PTRACE` to map ports to processes.

## Configuration (env file)

| Variable | Default | Meaning |
|---|---|---|
| `VPSW_LISTEN` | `127.0.0.1:9100` | Address the web UI binds to |
| `VPSW_DATA_DIR` | `/var/lib/vps-watcher` | Where the admin account, monitors and history are stored |
| `VPSW_INTERVAL` | `5` | Seconds between samples (1–60) |
| `VPSW_AUTH` | `login` | `login` or `open` |
| `VPSW_OPEN_LOGS` | `false` | In `open` mode, also show logs publicly |
| `VPSW_DOMAIN` | | Domain used by `vps-watcher proxy caddy` or `proxy nginx` |
| `VPSW_TRUSTED_PROXIES` | `127.0.0.1,::1` | Addresses whose forwarding headers are trusted |
| `VPSW_DOCKER_SOCKET` | `/var/run/docker.sock` | Path to the Docker socket |

## Edge cases handled

- cgroup v1 hosts. App metrics fall back to `cpuacct` and `memory`.
- No Docker, rootless Docker or Podman. A missing socket simply means no containers.
- Counters that reset or wrap. That sample is skipped instead of showing a spike.
- Containers recreated by Compose. History is keyed by container name, so it continues.
- Containers in host network mode. Per-app network stats are hidden, since they would duplicate the host totals.
- Huge logs. Requests are capped at 1000 lines and 2 MB, and files are read backwards from the end.
- A full disk. Saving history fails softly, and the error shows in the UI and the service log.
- Clock jumps. Rates use a monotonic clock, and history buckets never go backwards.

## API

All JSON is under `/api`, plus one SSE stream.

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/session` | Access mode, whether setup is required, whether signed in |
| POST | `/api/setup` | Create the admin (loopback only) |
| POST | `/api/login`, `/api/logout` | Sign in or out |
| POST | `/api/account` | Change email or password; signs out other sessions |
| POST | `/api/logout-others` | Sign out every other session |
| GET | `/api/overview` | Host snapshot plus all apps and monitors |
| GET | `/api/live` | SSE: the overview every interval |
| GET | `/api/history?keys=a,b&range=1h\|24h\|7d\|30d` | Series values for charts |
| GET | `/api/logs?source=…&lines=…&q=…` | Log lines |
| GET | `/api/log-sources` | Containers, services and files that have logs |
| GET | `/api/processes`, `/api/ports` | On-demand scans |
| GET, POST | `/api/monitors` | List or add monitors (adding needs the admin) |
| DELETE | `/api/monitors/{id}` | Remove a monitor (admin) |
| GET | `/api/report?range=…` | Averages, peaks, availability, disk forecast |
| GET | `/api/report.csv?range=…` | The same report as CSV |
