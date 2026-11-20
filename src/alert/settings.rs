//! Alert rules and delivery channels, read from `VPSW_ALERT_*`, `VPSW_SMTP_*` and `VPSW_WEBHOOK_*`.

use crate::net::{self, Url};

/// A value that must never reach logs, the UI or the terminal; `Debug` prints a placeholder.
#[derive(Clone, PartialEq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    /// The real value, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

/// When to alert and where to send it.
#[derive(Debug, Clone)]
pub struct AlertConfig {
    /// Percent thresholds; None turns a rule off.
    pub cpu: Option<f32>,
    pub memory: Option<f32>,
    pub disk: Option<f32>,
    /// Minutes CPU or memory must stay high before alerting.
    pub after_mins: u64,
    /// A near-exhaustion level for CPU and memory that alerts after one minute.
    pub spike: Option<f32>,
    /// Load average per core that counts as overloaded.
    pub load_per_core: Option<f32>,
    /// Emailed usage report: every this many days (1 or 7), or never.
    pub report_days: Option<u64>,
    /// Alert on stopped, unhealthy, restarted or failed containers and services.
    pub apps: bool,
    pub email: Option<EmailConfig>,
    pub webhook: Option<WebhookConfig>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        AlertConfig {
            cpu: Some(90.0),
            memory: Some(90.0),
            disk: Some(90.0),
            after_mins: 5,
            spike: Some(98.0),
            load_per_core: Some(2.0),
            report_days: None,
            apps: true,
            email: None,
            webhook: None,
        }
    }
}

/// How the SMTP connection is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Plain connection upgraded with STARTTLS (port 587).
    StartTls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption: only for a relay on this machine.
    None,
}

/// How vps-watcher signs in to the mail server.
#[derive(Debug, Clone, PartialEq)]
pub enum SmtpAuth {
    None,
    Password { user: String, password: Secret },
    OAuth2(OAuthConfig),
}

/// OAuth2 refresh-token settings (Gmail, Microsoft 365 and others).
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthConfig {
    pub user: String,
    pub client_id: String,
    pub client_secret: Option<Secret>,
    pub refresh_token: Secret,
    pub token_url: Url,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmailConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub auth: SmtpAuth,
    pub from: String,
    pub to: Vec<String>,
}

/// Body shape of webhook posts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookFormat {
    /// `{"text", "host", "events": [...]}` for your own receivers.
    Json,
    Slack,
    Discord,
    /// The message as plain text (ntfy and similar).
    Text,
}

impl WebhookFormat {
    pub fn name(self) -> &'static str {
        match self {
            WebhookFormat::Json => "json",
            WebhookFormat::Slack => "slack",
            WebhookFormat::Discord => "discord",
            WebhookFormat::Text => "text",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct WebhookConfig {
    /// The whole URL is secret: Slack and Discord put the key in the path.
    pub url: Secret,
    pub parsed: Url,
    pub format: WebhookFormat,
    /// Signs JSON bodies with HMAC-SHA256 when set.
    pub signing_secret: Option<Secret>,
    /// Sent as `Authorization: Bearer <token>` when set.
    pub token: Option<Secret>,
}

impl std::fmt::Debug for WebhookConfig {
    // The parsed URL holds the secret path, so only the host is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookConfig")
            .field("host", &self.parsed.host)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl AlertConfig {
    /// Reads alert settings with `val` (already trimmed; empty means unset).
    pub fn from_vals(val: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let after_mins = match val("VPSW_ALERT_AFTER") {
            None => 5,
            Some(v) => match v.parse::<u64>() {
                Ok(n) if (1..=120).contains(&n) => n,
                _ => return Err(format!("VPSW_ALERT_AFTER '{v}' must be minutes from 1 to 120")),
            },
        };
        let load_per_core = match val("VPSW_ALERT_LOAD").map(|v| v.to_ascii_lowercase()).as_deref() {
            None => Some(2.0),
            Some("off" | "false" | "no" | "0" | "none") => None,
            Some(v) => match v.parse::<f32>() {
                Ok(n) if (0.5..=50.0).contains(&n) => Some(n),
                _ => return Err(format!("VPSW_ALERT_LOAD '{v}' must be load per core from 0.5 to 50, or off")),
            },
        };
        let report_days = match val("VPSW_REPORT").map(|v| v.to_ascii_lowercase()).as_deref() {
            None | Some("off" | "false" | "no" | "none") => None,
            Some("daily") => Some(1),
            Some("weekly") => Some(7),
            Some(o) => return Err(format!("VPSW_REPORT '{o}' must be off, daily or weekly")),
        };
        Ok(AlertConfig {
            spike: threshold("VPSW_ALERT_SPIKE", val("VPSW_ALERT_SPIKE"), 98.0)?,
            load_per_core,
            report_days,
            cpu: threshold("VPSW_ALERT_CPU", val("VPSW_ALERT_CPU"), 90.0)?,
            memory: threshold("VPSW_ALERT_MEMORY", val("VPSW_ALERT_MEMORY"), 90.0)?,
            disk: threshold("VPSW_ALERT_DISK", val("VPSW_ALERT_DISK"), 90.0)?,
            after_mins,
            apps: !matches!(
                val("VPSW_ALERT_APPS").map(|v| v.to_ascii_lowercase()).as_deref(),
                Some("false" | "0" | "no" | "off")
            ),
            email: email(val)?,
            webhook: webhook(val)?,
        })
    }
}

/// A percent from 1 to 100, `off` to disable, or `default` when unset.
fn threshold(key: &str, v: Option<String>, default: f32) -> Result<Option<f32>, String> {
    match v.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => Ok(Some(default)),
        Some("off" | "false" | "no" | "0" | "none") => Ok(None),
        Some(s) => match s.trim_end_matches('%').parse::<f32>() {
            Ok(n) if (1.0..=100.0).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{key} '{s}' must be a percent from 1 to 100, or off")),
        },
    }
}

/// Loose email check: one `@`, a dotted or local domain, no spaces or header characters.
pub fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty()
        && !domain.is_empty()
        && s.len() <= 254
        && !domain.contains('@')
        && s.chars().all(|c| c.is_ascii_graphic() && !"<>,;:\"()[]\\".contains(c))
}

fn email(val: &dyn Fn(&str) -> Option<String>) -> Result<Option<EmailConfig>, String> {
    let host = val("VPSW_SMTP_HOST");
    let to = val("VPSW_ALERT_EMAIL");
    let (host, to) = match (host, to) {
        (None, None) => return Ok(None),
        (Some(_), None) => return Err("VPSW_ALERT_EMAIL (who receives alerts) is required with VPSW_SMTP_HOST".into()),
        (None, Some(_)) => return Err("VPSW_SMTP_HOST is required to send alerts to VPSW_ALERT_EMAIL".into()),
        (Some(h), Some(t)) => (h, t),
    };
    if !net::is_valid_host(&host) {
        return Err(format!("VPSW_SMTP_HOST '{host}' must be a host name like smtp.gmail.com"));
    }
    let to: Vec<String> = to.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    if to.is_empty() || to.len() > 20 {
        return Err("VPSW_ALERT_EMAIL needs 1 to 20 addresses separated by commas".into());
    }
    if let Some(bad) = to.iter().find(|a| !is_email(a)) {
        return Err(format!("VPSW_ALERT_EMAIL has an invalid address: '{bad}'"));
    }

    let security = match val("VPSW_SMTP_SECURITY").map(|v| v.to_ascii_lowercase()).as_deref() {
        None => None,
        Some("starttls") => Some(Security::StartTls),
        Some("tls" | "ssl") => Some(Security::Tls),
        Some("none" | "plain") => Some(Security::None),
        Some(o) => return Err(format!("VPSW_SMTP_SECURITY '{o}' must be starttls, tls or none")),
    };
    let port = match val("VPSW_SMTP_PORT") {
        Some(p) => p.parse::<u16>().ok().filter(|p| *p > 0).ok_or(format!("VPSW_SMTP_PORT '{p}' is not a port"))?,
        None => match security {
            Some(Security::Tls) => 465,
            Some(Security::None) => 25,
            _ => 587,
        },
    };
    let security = security.unwrap_or(if port == 465 { Security::Tls } else { Security::StartTls });

    let user = val("VPSW_SMTP_USER");
    let password = val("VPSW_SMTP_PASSWORD");
    let refresh = val("VPSW_SMTP_OAUTH_REFRESH_TOKEN");
    let auth = match (password, refresh) {
        (Some(_), Some(_)) => {
            return Err("set VPSW_SMTP_PASSWORD or VPSW_SMTP_OAUTH_REFRESH_TOKEN, not both".into());
        }
        (Some(p), None) => SmtpAuth::Password {
            user: user.clone().ok_or("VPSW_SMTP_USER is required with VPSW_SMTP_PASSWORD")?,
            password: Secret::new(p),
        },
        (None, Some(r)) => {
            let token_url = match val("VPSW_SMTP_OAUTH_TOKEN_URL").or_else(|| default_token_url(&host)) {
                Some(u) => u,
                None => return Err("VPSW_SMTP_OAUTH_TOKEN_URL is required for this mail provider".into()),
            };
            let token_url = net::parse_url(&token_url).map_err(|_| "VPSW_SMTP_OAUTH_TOKEN_URL is not a valid URL")?;
            if !token_url.https && !token_url.is_loopback() {
                return Err("VPSW_SMTP_OAUTH_TOKEN_URL must use https".into());
            }
            SmtpAuth::OAuth2(OAuthConfig {
                user: user.clone().ok_or("VPSW_SMTP_USER is required with OAuth")?,
                client_id: val("VPSW_SMTP_OAUTH_CLIENT_ID")
                    .ok_or("VPSW_SMTP_OAUTH_CLIENT_ID is required with OAuth")?,
                client_secret: val("VPSW_SMTP_OAUTH_CLIENT_SECRET").map(Secret::new),
                refresh_token: Secret::new(r),
                token_url,
            })
        }
        (None, None) => SmtpAuth::None,
    };
    if security == Security::None && auth != SmtpAuth::None && !net::is_loopback_host(&host) {
        return Err(
            "refusing to send SMTP credentials without encryption; use VPSW_SMTP_SECURITY=starttls or tls".into()
        );
    }
    let from = match val("VPSW_SMTP_FROM").or_else(|| user.filter(|u| is_email(u))) {
        Some(f) if is_email(&f) => f,
        Some(f) => return Err(format!("VPSW_SMTP_FROM '{f}' is not an email address")),
        None => return Err("VPSW_SMTP_FROM (the sender address) is required".into()),
    };
    Ok(Some(EmailConfig { host, port, security, auth, from, to }))
}

/// Token endpoints of the big providers, so most users only paste a refresh token.
fn default_token_url(smtp_host: &str) -> Option<String> {
    let h = smtp_host.to_ascii_lowercase();
    if h.ends_with("gmail.com") || h.ends_with("googlemail.com") || h.ends_with("google.com") {
        Some("https://oauth2.googleapis.com/token".into())
    } else if h.ends_with("office365.com") || h.ends_with("outlook.com") || h.ends_with("office.com") {
        Some("https://login.microsoftonline.com/common/oauth2/v2.0/token".into())
    } else {
        None
    }
}

fn webhook(val: &dyn Fn(&str) -> Option<String>) -> Result<Option<WebhookConfig>, String> {
    let Some(url) = val("VPSW_WEBHOOK_URL") else { return Ok(None) };
    // Never echo the URL: it usually contains the webhook's key.
    let parsed = net::parse_url(&url).map_err(|_| "VPSW_WEBHOOK_URL must be an http(s) URL".to_string())?;
    // The URL and any token are secrets; plain HTTP would send them in the clear.
    if !parsed.https && !parsed.is_loopback() {
        return Err("VPSW_WEBHOOK_URL must use https (plain http is allowed only to this server itself)".into());
    }
    let host = parsed.bare_host().to_ascii_lowercase();
    let format = match val("VPSW_WEBHOOK_FORMAT").map(|v| v.to_ascii_lowercase()).as_deref() {
        None | Some("auto") => {
            if host == "hooks.slack.com" {
                WebhookFormat::Slack
            } else if (host == "discord.com" || host == "discordapp.com") && parsed.path.starts_with("/api/webhooks/") {
                WebhookFormat::Discord
            } else if host == "ntfy.sh" {
                WebhookFormat::Text
            } else {
                WebhookFormat::Json
            }
        }
        Some("json") => WebhookFormat::Json,
        Some("slack") => WebhookFormat::Slack,
        Some("discord") => WebhookFormat::Discord,
        Some("text") => WebhookFormat::Text,
        Some(o) => return Err(format!("VPSW_WEBHOOK_FORMAT '{o}' must be auto, json, slack, discord or text")),
    };
    Ok(Some(WebhookConfig {
        url: Secret::new(url),
        parsed,
        format,
        signing_secret: val("VPSW_WEBHOOK_SECRET").map(Secret::new),
        token: val("VPSW_WEBHOOK_TOKEN").map(Secret::new),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(pairs: &[(&str, &str)]) -> Result<AlertConfig, String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        AlertConfig::from_vals(&|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_alert_without_channels() {
        let c = load(&[]).unwrap();
        assert_eq!((c.cpu, c.memory, c.disk, c.after_mins, c.apps), (Some(90.0), Some(90.0), Some(90.0), 5, true));
        assert!(c.email.is_none() && c.webhook.is_none());
    }

    #[test]
    fn thresholds() {
        let c = load(&[("VPSW_ALERT_CPU", "off"), ("VPSW_ALERT_DISK", "85%"), ("VPSW_ALERT_APPS", "no")]).unwrap();
        assert_eq!((c.cpu, c.disk, c.apps), (None, Some(85.0), false));
        assert!(load(&[("VPSW_ALERT_MEMORY", "150")]).is_err());
        assert!(load(&[("VPSW_ALERT_AFTER", "0")]).is_err());
        let c = load(&[("VPSW_ALERT_SPIKE", "95"), ("VPSW_ALERT_LOAD", "off"), ("VPSW_REPORT", "weekly")]).unwrap();
        assert_eq!((c.spike, c.load_per_core, c.report_days), (Some(95.0), None, Some(7)));
        let d = load(&[]).unwrap();
        assert_eq!((d.spike, d.load_per_core, d.report_days), (Some(98.0), Some(2.0), None));
        assert!(load(&[("VPSW_REPORT", "hourly")]).is_err());
    }

    #[test]
    fn gmail_with_app_password() {
        let c = load(&[
            ("VPSW_SMTP_HOST", "smtp.gmail.com"),
            ("VPSW_SMTP_USER", "me@gmail.com"),
            ("VPSW_SMTP_PASSWORD", "abcd efgh ijkl mnop"),
            ("VPSW_ALERT_EMAIL", "me@gmail.com, ops@example.com"),
        ])
        .unwrap();
        let e = c.email.unwrap();
        assert_eq!((e.port, e.security, e.from.as_str()), (587, Security::StartTls, "me@gmail.com"));
        assert_eq!(e.to, vec!["me@gmail.com", "ops@example.com"]);
        assert!(!format!("{:?}", e.auth).contains("abcd"), "Debug must not print the password");
    }

    #[test]
    fn port_465_means_tls_and_oauth_defaults() {
        let c = load(&[
            ("VPSW_SMTP_HOST", "smtp.office365.com"),
            ("VPSW_SMTP_PORT", "465"),
            ("VPSW_SMTP_USER", "alerts@contoso.com"),
            ("VPSW_SMTP_OAUTH_CLIENT_ID", "abc"),
            ("VPSW_SMTP_OAUTH_REFRESH_TOKEN", "rt"),
            ("VPSW_ALERT_EMAIL", "ops@contoso.com"),
        ])
        .unwrap();
        let e = c.email.unwrap();
        assert_eq!(e.security, Security::Tls);
        let SmtpAuth::OAuth2(o) = e.auth else { panic!("expected OAuth") };
        assert_eq!(o.token_url.host, "login.microsoftonline.com");
    }

    #[test]
    fn email_errors() {
        let base = [("VPSW_SMTP_HOST", "mail.example.com"), ("VPSW_ALERT_EMAIL", "ops@example.com")];
        assert!(load(&base[..1]).is_err(), "recipients missing");
        assert!(load(&base[1..]).is_err(), "host missing");
        assert!(load(&base).is_err(), "sender missing");
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut v = base.to_vec();
            v.extend_from_slice(extra);
            load(&v)
        };
        assert!(with(&[("VPSW_SMTP_FROM", "vpsw@example.com")]).is_ok(), "an unauthenticated relay is fine");
        assert!(with(&[("VPSW_SMTP_FROM", "x@y"), ("VPSW_ALERT_EMAIL", "a@b\r\nBcc: c@d")]).is_err());
        assert!(
            with(&[("VPSW_SMTP_USER", "u@example.com"), ("VPSW_SMTP_PASSWORD", "p"), ("VPSW_SMTP_SECURITY", "none")])
                .is_err(),
            "no plain-text passwords to remote hosts"
        );
        assert!(
            with(&[("VPSW_SMTP_USER", "u@x.io"), ("VPSW_SMTP_PASSWORD", "p"), ("VPSW_SMTP_OAUTH_REFRESH_TOKEN", "r")])
                .is_err()
        );
        assert!(
            with(&[
                ("VPSW_SMTP_USER", "u@x.io"),
                ("VPSW_SMTP_OAUTH_REFRESH_TOKEN", "r"),
                ("VPSW_SMTP_OAUTH_CLIENT_ID", "c")
            ])
            .is_err(),
            "unknown provider needs a token URL"
        );
    }

    #[test]
    fn webhook_formats_and_secrecy() {
        let fmt = |u: &'static str| load(&[("VPSW_WEBHOOK_URL", u)]).unwrap().webhook.unwrap().format;
        assert_eq!(fmt("https://hooks.slack.com/services/T/B/x"), WebhookFormat::Slack);
        assert_eq!(fmt("https://discord.com/api/webhooks/1/abc"), WebhookFormat::Discord);
        assert_eq!(fmt("https://ntfy.sh/my-topic"), WebhookFormat::Text);
        assert_eq!(fmt("https://example.com/hook"), WebhookFormat::Json);
        let err = load(&[("VPSW_WEBHOOK_URL", "ftp://secret-key@x")]).unwrap_err();
        assert!(!err.contains("secret-key"), "errors must not echo the URL");
        assert!(load(&[("VPSW_WEBHOOK_URL", "http://hooks.example.com/k")]).is_err(), "no secrets over plain http");
        let w = load(&[("VPSW_WEBHOOK_URL", "http://127.0.0.1:8099/key-123")]).unwrap().webhook.unwrap();
        assert!(!format!("{w:?}").contains("key-123"), "Debug hides the path");
    }
}
