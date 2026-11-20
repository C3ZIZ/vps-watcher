# Alerts and email reports

vps-watcher can tell you when something goes wrong: a resource runs out, a container stops, a website goes down. Alerts always appear on the **Alerts** page. You can also have them sent by email, to a webhook (Slack, Discord, ntfy or your own endpoint), or both. The same email setup sends usage reports, either on demand or every day or week.

- [What triggers an alert](#what-triggers-an-alert)
- [How alerts behave](#how-alerts-behave)
- [Set up email](#set-up-email)
  - [Gmail with an app password](#gmail-with-an-app-password)
  - [Microsoft 365 or Outlook with OAuth2](#microsoft-365-or-outlook-with-oauth2)
  - [Gmail with OAuth2](#gmail-with-oauth2)
  - [Other providers and local relays](#other-providers-and-local-relays)
- [Set up a webhook](#set-up-a-webhook)
- [Email reports](#email-reports)
- [Check and test](#check-and-test)
- [Troubleshooting](#troubleshooting)
- [Keeping secrets safe](#keeping-secrets-safe)

Every setting below goes in `/etc/vps-watcher/vps-watcher.env`. Edit it with `sudo nano /etc/vps-watcher/vps-watcher.env` (or `sudoedit`), then restart: `sudo systemctl restart vps-watcher`.

## What triggers an alert

| Alert | Severity | Fires when | Setting (default) |
|---|---|---|---|
| CPU high | warning | CPU stays at or above the limit for `VPSW_ALERT_AFTER` minutes | `VPSW_ALERT_CPU=90` |
| Memory high | warning | Memory stays at or above the limit for `VPSW_ALERT_AFTER` minutes | `VPSW_ALERT_MEMORY=90` |
| CPU or memory spike | critical | CPU or memory reaches the spike level for one minute. Memory this full means apps may be killed | `VPSW_ALERT_SPIKE=98` |
| Load high | warning | The 1-minute load stays above this many per CPU core for `VPSW_ALERT_AFTER` minutes | `VPSW_ALERT_LOAD=2` |
| Disk full | critical | A disk's space or inodes reach the limit | `VPSW_ALERT_DISK=90` |
| Monitor down | critical | A monitor you added fails two checks in a row (about one minute) | always on |
| App stopped | critical | A container that was running exits, dies or keeps restarting for a minute | `VPSW_ALERT_APPS=true` |
| App unhealthy | warning | A container's Docker health check fails for a minute | `VPSW_ALERT_APPS=true` |
| App restarted or out of memory | warning | Docker restarted a container, or the kernel killed it for using too much memory | `VPSW_ALERT_APPS=true` |
| Service failed | critical | systemd marks a unit as failed | `VPSW_ALERT_APPS=true` |
| Docker unreachable | critical | The Docker socket stops answering for a minute | `VPSW_ALERT_APPS=true` |

`VPSW_ALERT_AFTER` defaults to 5 minutes. Set any percent rule to `off` to disable it.

CPU, memory and load alerts name the apps using the most, such as `top: web 45%, worker 30%`. The email adds a table of the server's state and its busiest apps, so you can tell at a glance whether to fix an app or add resources.

## How alerts behave

- **No noise from blips.** A problem must last before it alerts, and an alert clears only once things have stayed normal for a short while. A value hovering at the limit does not flap.
- **Resolved messages.** When a problem ends you get a "Resolved" message that says how long it lasted. Restarts and out-of-memory kills are one-off notices with no resolved step.
- **Batching.** Events that happen within 10 seconds of each other arrive as one message, so a server reboot does not send twenty emails.
- **Limits.** At most 30 messages per hour, and at most one restart notice per app every 15 minutes. Failed deliveries are retried twice, after 10 seconds and after 1 minute.
- **Restarts are safe.** Firing alerts are saved in `/var/lib/vps-watcher/alerts.json`, so restarting or upgrading vps-watcher does not send them again.
- **Only real changes.** A container alerts only after vps-watcher has seen it running. An old container that exited months ago stays quiet. Units that are *already* failed when you install do alert once, because they usually need attention.
- **Removed things end quietly.** Deleting a monitor or removing a container clears its alert without a message.

## Set up email

Email turns on when both `VPSW_ALERT_EMAIL` (who receives alerts) and `VPSW_SMTP_HOST` are set. Messages are sent as HTML with a plain-text copy for simple mail clients.

| Setting | Meaning |
|---|---|
| `VPSW_ALERT_EMAIL` | Recipients, separated by commas (up to 20) |
| `VPSW_SMTP_HOST` | Mail server, such as `smtp.gmail.com` |
| `VPSW_SMTP_PORT` | Empty means 587. Use 465 for TLS |
| `VPSW_SMTP_SECURITY` | Empty picks `tls` for port 465 and `starttls` otherwise. `none` is only for a relay on this server |
| `VPSW_SMTP_USER`, `VPSW_SMTP_PASSWORD` | Sign-in with a password or app password |
| `VPSW_SMTP_FROM` | Sender address. Empty means `VPSW_SMTP_USER` |
| `VPSW_SMTP_OAUTH_*` | Sign-in with OAuth2 instead of a password (see below) |

vps-watcher always verifies the server's TLS certificate. It refuses to send a password over an unencrypted connection to another machine.

If your password contains spaces, `#` or quotes, wrap it in single quotes: `VPSW_SMTP_PASSWORD='my pass#word'`.

### Gmail with an app password

This is the simplest option for Gmail and Google Workspace.

1. Turn on 2-Step Verification for the Google account.
2. Open <https://myaccount.google.com/apppasswords>, create an app password named "vps-watcher" and copy the 16 letters.
3. Add to the env file:

   ```sh
   VPSW_ALERT_EMAIL=you@example.com
   VPSW_SMTP_HOST=smtp.gmail.com
   VPSW_SMTP_USER=your.account@gmail.com
   VPSW_SMTP_PASSWORD='abcd efgh ijkl mnop'
   ```

4. Restart and send a test: `sudo systemctl restart vps-watcher && sudo vps-watcher test-alert`.

### Microsoft 365 or Outlook with OAuth2

Microsoft is turning off password sign-in for SMTP, so use OAuth2. You register an app once, get a refresh token once, and vps-watcher renews access tokens on its own after that. If Microsoft issues a new refresh token, vps-watcher saves it in `/var/lib/vps-watcher/smtp-oauth.json` (readable only by the service).

1. **Allow SMTP sign-in for the mailbox.** In the Microsoft 365 admin center, open the user, then **Mail** > **Manage email apps**, and tick **Authenticated SMTP**.
2. **Register an app.** In the Azure portal, go to **Microsoft Entra ID** > **App registrations** > **New registration**. Choose "Accounts in this organizational directory only". Then:
   - Under **Authentication**, set **Allow public client flows** to **Yes**.
   - Under **API permissions**, add **Microsoft Graph** > **Delegated** > `SMTP.Send` and `offline_access`. Grant admin consent if asked.
   - Copy the **Application (client) ID** and the **Directory (tenant) ID**.
3. **Get a refresh token** on your own computer. Replace `TENANT` and `CLIENT` with your IDs:

   ```sh
   curl -s https://login.microsoftonline.com/TENANT/oauth2/v2.0/devicecode \
     -d client_id=CLIENT -d scope="https://outlook.office.com/SMTP.Send offline_access"
   ```

   Open the `verification_uri` it prints, enter the `user_code` and sign in as the mailbox that sends alerts. Then exchange the `device_code`:

   ```sh
   curl -s https://login.microsoftonline.com/TENANT/oauth2/v2.0/token \
     -d grant_type=urn:ietf:params:oauth:grant-type:device_code -d client_id=CLIENT -d device_code=DEVICE_CODE
   ```

   The reply contains a `refresh_token`. Treat it like a password.
4. **Add to the env file:**

   ```sh
   VPSW_ALERT_EMAIL=ops@example.com
   VPSW_SMTP_HOST=smtp.office365.com
   VPSW_SMTP_USER=alerts@example.com
   VPSW_SMTP_OAUTH_CLIENT_ID=CLIENT
   VPSW_SMTP_OAUTH_REFRESH_TOKEN='paste the refresh token here'
   VPSW_SMTP_OAUTH_TOKEN_URL=https://login.microsoftonline.com/TENANT/oauth2/v2.0/token
   ```

   Leave `VPSW_SMTP_PASSWORD` empty. A public client like this has no client secret. If your app has one, set `VPSW_SMTP_OAUTH_CLIENT_SECRET`.

### Gmail with OAuth2

An app password is easier. Use OAuth2 if your organization blocks app passwords.

1. In Google Cloud Console, create a project. Configure the OAuth consent screen and **publish** it: refresh tokens of apps left in "Testing" stop working after 7 days.
2. Create an **OAuth client ID** of type **Web application**, with `https://developers.google.com/oauthplayground` as a redirect URI.
3. In the [OAuth 2.0 Playground](https://developers.google.com/oauthplayground), open the settings (gear icon), tick **Use your own OAuth credentials** and enter your client ID and secret. Authorize the scope `https://mail.google.com/`, then click **Exchange authorization code for tokens** and copy the refresh token.
4. Add to the env file:

   ```sh
   VPSW_ALERT_EMAIL=you@example.com
   VPSW_SMTP_HOST=smtp.gmail.com
   VPSW_SMTP_USER=your.account@gmail.com
   VPSW_SMTP_OAUTH_CLIENT_ID=...apps.googleusercontent.com
   VPSW_SMTP_OAUTH_CLIENT_SECRET='...'
   VPSW_SMTP_OAUTH_REFRESH_TOKEN='...'
   ```

   The token URL for Gmail is filled in automatically.

### Other providers and local relays

Any SMTP service works with a user name and password. For example:

| Provider | `VPSW_SMTP_HOST` | Port | User |
|---|---|---|---|
| Amazon SES | `email-smtp.<region>.amazonaws.com` | 587 | SMTP credentials from the SES console |
| Mailgun | `smtp.mailgun.org` | 587 | Your SMTP login |
| Brevo | `smtp-relay.brevo.com` | 587 | Your Brevo login |
| Fastmail | `smtp.fastmail.com` | 465 | Your address plus an app password |
| Postfix on this server | `127.0.0.1` | 25 | none; set `VPSW_SMTP_SECURITY=none` and `VPSW_SMTP_FROM` |

For SES and similar services, the sender in `VPSW_SMTP_FROM` must be a verified address or domain.

## Set up a webhook

Set `VPSW_WEBHOOK_URL`. vps-watcher picks the right format from the URL:

| Service | URL looks like | Format |
|---|---|---|
| Slack | `https://hooks.slack.com/services/...` | `slack` |
| Discord | `https://discord.com/api/webhooks/...` | `discord` (mentions disabled, long text trimmed) |
| ntfy | `https://ntfy.sh/your-topic` | `text`, with title and priority headers |
| Anything else | your endpoint | `json` |

Set `VPSW_WEBHOOK_FORMAT=json|slack|discord|text` to override the choice, for example for a self-hosted ntfy. `VPSW_WEBHOOK_TOKEN` adds `Authorization: Bearer <token>`, which ntfy access tokens and many APIs use.

**The JSON format** posts:

```json
{
  "source": "vps-watcher",
  "host": "web-1",
  "subject": "[web-1] ALERT: api is down",
  "text": "ALERT     api is down\n          HTTP 503 · 2026-11-21 14:03 UTC\n...",
  "link": "https://watch.example.com",
  "events": [
    { "key": "mon:4f2a", "state": "firing", "severity": "critical",
      "title": "api is down", "detail": "HTTP 503", "ts": 1795269789 }
  ]
}
```

`state` is `firing`, `resolved` or `notice`. `severity` is `critical`, `warning` or `info`. `link` is set when `VPSW_DOMAIN` is.

**Signed requests.** Set `VPSW_WEBHOOK_SECRET` and every request carries two headers. `X-Vpsw-Timestamp` holds the unix time. `X-Vpsw-Signature` holds `sha256=` followed by the HMAC-SHA256, keyed with your secret, of `<timestamp>.<raw body>`. To verify, recompute the signature, compare it in constant time, and reject timestamps older than 5 minutes:

```python
import hashlib, hmac, time

def verify(secret: bytes, headers, body: bytes) -> bool:
    ts = headers["X-Vpsw-Timestamp"]
    expected = "sha256=" + hmac.new(secret, ts.encode() + b"." + body, hashlib.sha256).hexdigest()
    fresh = abs(time.time() - int(ts)) < 300
    return fresh and hmac.compare_digest(expected, headers["X-Vpsw-Signature"])
```

The webhook URL itself is treated as a secret: vps-watcher never logs or displays it, only its host.

## Email reports

A report covers averages and peaks for the server, the busiest apps, availability per monitor and a "disk full in N days" forecast. It also opens with plain advice, such as "Memory averaged 88%. The server needs more RAM, or its biggest apps need limits."

- **On demand.** On the **Reports** page, pick a range and click **Email report**.
- **On a schedule.** Set `VPSW_REPORT=daily` (last 24 hours) or `VPSW_REPORT=weekly` (last 7 days). The first report goes out one full period after you turn this on.

Reports go to the same recipients as alerts.

## Check and test

```sh
sudo vps-watcher config        # checks the env file and prints the settings, secrets hidden
sudo systemctl restart vps-watcher
sudo vps-watcher test-alert    # sends a test to every channel and prints the result
```

The **Alerts** page shows the same information for the admin. **Where alerts go** shows each channel's last delivery or last error, and it has a **Send test alert** button.

## Troubleshooting

| Message | What to do |
|---|---|
| `535 ... Username and Password not accepted` | Wrong user name or password. Gmail needs an app password, not your normal password |
| `the mail server does not offer STARTTLS` | You are probably on port 465. Set `VPSW_SMTP_PORT=465` (TLS is then picked automatically) |
| `TLS: invalid peer certificate` | The server's certificate is not trusted, or the host name does not match it. Use the provider's official host name |
| `timed out talking to the mail server` or `cannot connect to ...` | Many cloud providers block outgoing port 25 and sometimes 587. Try 465, or ask your provider to unblock it |
| `OAuth token refresh failed: invalid_grant` | The refresh token expired or was revoked. Get a new one (see the provider steps above) |
| `the mail server rejected the OAuth token` | The mailbox is not allowed to use SMTP sign-in, or the app lacks `SMTP.Send` / `https://mail.google.com/` |
| `alert limit of 30 per hour reached` | Something is flapping. Fix it, or turn off the noisy rule |
| A service alerts every time vps-watcher starts | It is failed. Fix it, or clear it with `sudo systemctl reset-failed <unit>` |

Delivery problems also appear in the service log: `journalctl -u vps-watcher -n 50`.

## Keeping secrets safe

- The env file is owned by root and readable only by the `vps-watcher` group (`0640`). Keep it that way.
- vps-watcher never prints, logs or shows passwords, tokens, webhook URLs or signing secrets, whether in the UI, in errors or in `vps-watcher config`.
- Prefer an app password or OAuth2 over your main account password, and use a dedicated sending account if you can.
- If you let an AI assistant set up your server, point it at [ai-install.md](ai-install.md). It tells the assistant how to configure vps-watcher without ever reading or printing your secrets.
