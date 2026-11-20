# Installing vps-watcher with an AI assistant

**For AI agents:** you were asked to install, configure or upgrade vps-watcher on a server. Follow this runbook exactly. The rules in the first section override any shortcut that seems faster.

**For people:** give your assistant this link and say "install vps-watcher following this guide". The assistant handles everything except your passwords and tokens. It asks you to type those yourself, at a prompt that hides them.

## Rules for handling secrets

These settings are **secrets**:

| Setting | What it is |
|---|---|
| `VPSW_SMTP_PASSWORD` | Mail password or app password |
| `VPSW_SMTP_OAUTH_CLIENT_SECRET` | OAuth2 client secret |
| `VPSW_SMTP_OAUTH_REFRESH_TOKEN` | OAuth2 refresh token |
| `VPSW_WEBHOOK_URL` | Webhook URL (Slack and Discord put the key in the URL) |
| `VPSW_WEBHOOK_SECRET` | Webhook signing key |
| `VPSW_WEBHOOK_TOKEN` | Webhook bearer token |

The admin account's password, the files in `/var/lib/vps-watcher/` (`admin.json`, `smtp-oauth.json`, `alerts.json`) and the people's own `.env` files count as secrets too.

1. **Do not read secret files.** Never run `cat`, `less`, `head`, `tail`, `grep` (without `-o` as shown below), `sed -n`, `awk`, `base64`, `cp`, `scp` or an editor on `/etc/vps-watcher/vps-watcher.env` or anything in `/var/lib/vps-watcher/`. Do not run `systemctl show -p Environment vps-watcher`, `cat /proc/<pid>/environ` or `sudo env` either: all of them reveal secrets.
2. **Inspect settings only in these two safe ways:**

   ```sh
   sudo vps-watcher config                                      # every setting, secrets shown only as "set"
   sudo grep -oE '^[A-Z_]+=' /etc/vps-watcher/vps-watcher.env   # key names only, no values
   ```

3. **Never put a secret in a command line.** It would end up in shell history, in `ps` output and in your transcript. `vps-watcher set` refuses secrets passed as arguments for exactly this reason.
4. **The person types secrets, not you.** When a secret is needed, ask the person to run the command themselves in their own terminal. It prompts with typing hidden:

   ```sh
   sudo vps-watcher set VPSW_SMTP_PASSWORD
   ```

5. **If the person pastes a secret into the chat and explicitly asks you to set it**, you may. Pass it through a quoted heredoc, so the value is not an argument of any process, and never echo it back:

   ```sh
   sudo vps-watcher set VPSW_SMTP_PASSWORD <<'VALUE'
   the value goes on this line
   VALUE
   ```

   Do not write it to any other file, and do not repeat it in your reply. Tell the person once that the value is now in this conversation's history, so they may want to rotate it later.
6. **Never weaken protection.** Keep `/etc/vps-watcher/vps-watcher.env` at mode `0640`, owner `root`, group `vps-watcher`, and the data directory at `0700`. Do not edit the hardening in the systemd unit. Do not switch `VPSW_LISTEN` to `0.0.0.0` or `VPSW_AUTH` to `open` unless the person asks for it in plain words. If they do, explain what becomes public.
7. **Do not create the admin account.** Its password would pass through you. The person creates it in the browser through an SSH tunnel (step 3).
8. **Logs are safe, but be careful with other apps' logs.** `journalctl -u vps-watcher` never contains secrets. Logs of the person's *other* apps, which vps-watcher can show, might. Quote only the lines you need.
9. **Report without secrets.** Your final summary lists what you did and what the person still has to do. It names settings, never their values.

## Runbook

### 1. Check the server

```sh
uname -sm                  # must be Linux on x86_64 or aarch64
systemctl --version | head -n1
id -u                      # 0, or use sudo for every command below
```

vps-watcher needs systemd. Docker is optional. Without it, vps-watcher simply shows no containers.

### 2. Install

```sh
curl -fsSL https://raw.githubusercontent.com/C3ZIZ/vps-watcher/main/install.sh | sudo sh
```

The installer verifies the download's checksum, creates the `vps-watcher` user and starts a hardened service. Running it again upgrades vps-watcher and keeps settings and history.

Confirm:

```sh
systemctl is-active vps-watcher            # active
vps-watcher version
sudo vps-watcher config                    # prints settings, secrets hidden
curl -s http://127.0.0.1:9100/api/session  # "setup_required":true until the admin exists
```

If it is not active, read `journalctl -u vps-watcher -n 50 --no-pager`.

### 3. Let the person create the admin account

Tell the person to run this on **their own computer**, replacing the user and host:

```sh
ssh -L 9100:127.0.0.1:9100 user@server
```

Then they open <http://localhost:9100> and create the admin with their email and a password of at least 12 characters. Wait until they confirm. You can check without seeing anything private: `curl -s http://127.0.0.1:9100/api/session` shows `"setup_required":false`.

### 4. Optional: put it on a domain

Ask the person for the domain. It is not a secret.

```sh
sudo vps-watcher set VPSW_DOMAIN watch.example.com
sudo vps-watcher proxy caddy      # or: sudo vps-watcher proxy nginx
```

Add the printed block to the existing Caddy or nginx configuration, reload the proxy and restart vps-watcher. The DNS record must point to the server first. Keep `VPSW_LISTEN=127.0.0.1:9100`: the proxy is the only public entry.

### 5. Optional: alerts by email or webhook

Set non-secret settings yourself with `vps-watcher set KEY VALUE`. Ask the person to set each secret with `vps-watcher set KEY`. Each `set` prints "Not complete yet: ..." until the required settings are all present; that is expected.

**Email with Gmail** (the person needs an [app password](https://myaccount.google.com/apppasswords)):

```sh
sudo vps-watcher set VPSW_ALERT_EMAIL person@example.com
sudo vps-watcher set VPSW_SMTP_HOST smtp.gmail.com
sudo vps-watcher set VPSW_SMTP_USER sender@gmail.com
# the person runs this one themselves:
sudo vps-watcher set VPSW_SMTP_PASSWORD
```

**Email with Microsoft 365 or other providers:** follow [alerts.md](alerts.md#set-up-email). OAuth2 needs a client ID (not secret), a refresh token (secret) and, for Microsoft, a token URL containing the tenant ID (not secret).

**Webhook** (Slack, Discord, ntfy or a custom endpoint). The URL is a secret, so the person sets it:

```sh
sudo vps-watcher set VPSW_WEBHOOK_URL
```

**Rules** are not secret. Change them only if the person asks. The defaults are CPU and memory at 90% for 5 minutes, spikes at 98% for 1 minute, load above 2 per core, and disks at 90%:

```sh
sudo vps-watcher set VPSW_ALERT_DISK 85
sudo vps-watcher set VPSW_REPORT weekly     # emailed usage report: off, daily or weekly
```

Apply and test:

```sh
sudo vps-watcher config
sudo systemctl restart vps-watcher
sudo vps-watcher test-alert
```

`test-alert` prints `email: sent` or `webhook: sent`, or a reason that contains no secrets. Ask the person to confirm the message arrived. [alerts.md](alerts.md#troubleshooting) explains the common errors.

### 6. Hand over

Tell the person:

- the dashboard address (the tunnel, or their domain)
- which alert channels are on, as shown by `sudo vps-watcher config`
- anything they still need to do, such as creating the admin, setting a secret, or adding the DNS record

Do not include any secret value, partial value or hash in the summary.

## Upgrade, check and remove

| Task | Command |
|---|---|
| Upgrade | `curl -fsSL https://raw.githubusercontent.com/C3ZIZ/vps-watcher/main/install.sh \| sudo sh` |
| New settings after an upgrade | `sudo grep -oE '^[A-Z_]+=' /etc/vps-watcher/vps-watcher.env.example` (key names only) |
| Health | `systemctl is-active vps-watcher` and `journalctl -u vps-watcher -n 50 --no-pager` |
| Forgot admin password | `sudo vps-watcher reset-admin && sudo systemctl restart vps-watcher`, then step 3 again |
| Remove, keep data | `curl -fsSL https://raw.githubusercontent.com/C3ZIZ/vps-watcher/main/install.sh \| sudo sh -s -- --uninstall` |
| Remove everything | the same with `--purge` (deletes settings and history; ask the person first) |
