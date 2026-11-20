//! Alerts: rules over each snapshot, then batched delivery by email and webhook.

pub mod engine;
mod html;
mod oauth;
pub mod settings;
mod smtp;
mod webhook;

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub use engine::{Active, Engine, Event, Severity, State};
pub use html::Context;
pub use settings::AlertConfig;
use settings::{Security, SmtpAuth, WebhookConfig};

use crate::util::{now, utc_stamp};
use crate::{info, warn};

/// Events arriving this close together go out as one message.
const BATCH_WINDOW: Duration = Duration::from_secs(10);
/// Most events listed in one message; the rest are counted.
const MAX_LISTED: usize = 20;
/// Most notifications per hour, so a flapping service cannot flood an inbox.
const MAX_PER_HOUR: usize = 30;
/// Longest a test send may take.
const TEST_TIMEOUT: Duration = Duration::from_secs(25);
/// Waits before the second and third delivery attempt.
const RETRY_AFTER: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(60)];

/// Events from one sample plus the server state they happened in.
pub struct Batch {
    pub events: Vec<Event>,
    pub ctx: Context,
}

/// Delivery health of one channel, shown on the Alerts page.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChannelStatus {
    pub sent: u64,
    pub failed: u64,
    /// Notifications not sent because of the hourly limit.
    pub skipped: u64,
    pub last_ok: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_at: Option<u64>,
}

/// Sends alerts to every configured channel.
pub struct Notifier {
    host: String,
    link: Option<String>,
    mailer: Option<smtp::Mailer>,
    hook: Option<WebhookConfig>,
    email_status: Mutex<ChannelStatus>,
    hook_status: Mutex<ChannelStatus>,
    sent_at: Mutex<VecDeque<u64>>,
    /// Events dropped by the hourly limit, reported in the next message.
    suppressed: std::sync::atomic::AtomicUsize,
    testing: AtomicBool,
}

impl Notifier {
    /// `data_dir` holds the rotated OAuth token; `persist` is false for one-off CLI runs, which
    /// read it but must not create files the service user cannot read.
    pub fn new(cfg: &AlertConfig, data_dir: Option<&Path>, persist: bool, host: &str, domain: Option<&str>) -> Self {
        let mailer = cfg.email.clone().map(|e| {
            let oauth = match &e.auth {
                SmtpAuth::OAuth2(o) => {
                    Some(oauth::TokenSource::new(o.clone(), data_dir.map(|d| d.join("smtp-oauth.json")), persist))
                }
                _ => None,
            };
            smtp::Mailer::new(e, oauth, host)
        });
        Notifier {
            host: if host.is_empty() { "server".into() } else { host.to_string() },
            link: domain.map(|d| format!("https://{d}")),
            mailer,
            hook: cfg.webhook.clone(),
            email_status: Mutex::default(),
            hook_status: Mutex::default(),
            sent_at: Mutex::default(),
            suppressed: Default::default(),
            testing: AtomicBool::new(false),
        }
    }

    pub fn has_channel(&self) -> bool {
        self.mailer.is_some() || self.hook.is_some()
    }

    /// Channel settings and delivery health for the admin; secrets are never included.
    pub fn describe(&self) -> Value {
        let lock = |m: &Mutex<ChannelStatus>| m.lock().map(|s| s.clone()).unwrap_or_default();
        let email = self.mailer.as_ref().map(|m| {
            let c = &m.cfg;
            json!({
                "host": c.host,
                "port": c.port,
                "security": security_name(c.security),
                "auth": auth_name(&c.auth),
                "from": c.from,
                "to": c.to,
                "status": lock(&self.email_status),
            })
        });
        let webhook = self.hook.as_ref().map(|h| {
            json!({
                "host": h.parsed.host,
                "format": h.format.name(),
                "signed": h.signing_secret.is_some(),
                "token": h.token.is_some(),
                "status": lock(&self.hook_status),
            })
        });
        json!({ "email": email, "webhook": webhook })
    }

    /// Sends a test message to every channel right away, reporting each result.
    pub async fn test(&self, ctx: &Context) -> Result<Vec<(&'static str, Result<(), String>)>, String> {
        if !self.has_channel() {
            return Err(
                "No alert channel is set up. Add SMTP or webhook settings to the env file, then restart.".into()
            );
        }
        let _busy = self.busy()?;
        let ev = Event {
            key: "test".into(),
            state: State::Notice,
            severity: Severity::Info,
            title: "Test alert".into(),
            detail: format!("Alerts from {} reach you here.", self.host),
            ts: now(),
        };
        // Stay inside the web server's 30 s request limit.
        tokio::time::timeout(TEST_TIMEOUT, self.deliver(std::slice::from_ref(&ev), ctx, true))
            .await
            .map_err(|_| "The test took too long. Check the server address and port, then try again.".to_string())
    }

    /// Sends one notice right away, within the hourly limit and with retries.
    pub async fn notify(&self, ev: Event) {
        self.deliver(std::slice::from_ref(&ev), &Context::default(), false).await;
    }

    /// Emails a usage report now. Reports go by email only; webhooks are for alerts.
    pub async fn send_report(&self, r: &crate::report::Report) -> Result<(), String> {
        let m = self
            .mailer
            .as_ref()
            .ok_or("Email is not set up. Add VPSW_SMTP_HOST and VPSW_ALERT_EMAIL to the env file, then restart.")?;
        let _busy = self.busy()?;
        let subject = format!("[{}] Report: last {}", self.host, r.range);
        let text = html::report_text(r, self.link.as_deref());
        let html = html::report_html(r, self.link.as_deref());
        tokio::time::timeout(
            TEST_TIMEOUT,
            self.attempt(&self.email_status, true, || m.send(&subject, &text, Some(&html))),
        )
        .await
        .map_err(|_| "Sending took too long. Check the mail server settings.".to_string())?
    }

    /// One manual send at a time; the guard frees the slot even if the request is dropped.
    fn busy(&self) -> Result<impl Drop + '_, String> {
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        if self.testing.swap(true, Ordering::SeqCst) {
            return Err("Another message is being sent right now. Try again in a moment.".into());
        }
        Ok(Done(&self.testing))
    }

    /// Delivers one batch to all channels at once; `test` skips the hourly limit and retries.
    async fn deliver(&self, events: &[Event], ctx: &Context, test: bool) -> Vec<(&'static str, Result<(), String>)> {
        if !test && !self.take_slot() {
            warn!("alert limit of {MAX_PER_HOUR} per hour reached; not sending {} event(s)", events.len());
            self.suppressed.fetch_add(events.len(), Ordering::SeqCst);
            for s in [&self.email_status, &self.hook_status] {
                if let Ok(mut s) = s.lock() {
                    s.skipped += 1;
                }
            }
            return Vec::new();
        }
        // Say what the hourly limit held back, so a recovery or a new failure is not missed silently.
        let held = if test { 0 } else { self.suppressed.swap(0, Ordering::SeqCst) };
        let mut with_note = Vec::new();
        let events = if held > 0 {
            with_note.extend_from_slice(events);
            with_note.push(Event {
                key: "suppressed".into(),
                state: State::Notice,
                severity: Severity::Warning,
                title: format!("{held} earlier alert event(s) were not sent"),
                detail: format!("The limit of {MAX_PER_HOUR} messages per hour was reached. See the Alerts page for the full history."),
                ts: now(),
            });
            &with_note[..]
        } else {
            events
        };
        let subject = subject(&self.host, events);
        let text = text(&self.host, events, self.link.as_deref());
        let html = html::alert_html(&self.host, events, ctx, self.link.as_deref());
        let email = async {
            let m = self.mailer.as_ref()?;
            Some(("email", self.attempt(&self.email_status, test, || m.send(&subject, &text, Some(&html))).await))
        };
        let hook = async {
            let h = self.hook.as_ref()?;
            let send =
                || webhook::post(h, webhook::payload(h, &self.host, &subject, &text, events, self.link.as_deref()));
            Some(("webhook", self.attempt(&self.hook_status, test, send).await))
        };
        let (a, b) = tokio::join!(email, hook);
        a.into_iter().chain(b).collect()
    }

    /// Runs `send` with retries and records the outcome.
    async fn attempt<F, Fut>(&self, status: &Mutex<ChannelStatus>, test: bool, send: F) -> Result<(), String>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        let waits: &[Duration] = if test { &[] } else { &RETRY_AFTER };
        let mut result = send().await;
        for wait in waits {
            if result.is_ok() {
                break;
            }
            tokio::time::sleep(*wait).await;
            result = send().await;
        }
        if let Ok(mut s) = status.lock() {
            match &result {
                Ok(()) => {
                    s.sent += 1;
                    s.last_ok = Some(now());
                }
                Err(e) => {
                    s.failed += 1;
                    s.last_error = Some(e.clone());
                    s.last_error_at = Some(now());
                }
            }
        }
        if let Err(e) = &result {
            warn!("alert delivery failed: {e}");
        }
        result
    }

    /// Takes one slot of the hourly budget, or returns false when it is used up.
    fn take_slot(&self) -> bool {
        let t = now();
        let Ok(mut q) = self.sent_at.lock() else { return true };
        while q.front().is_some_and(|s| t.saturating_sub(*s) >= 3600) {
            q.pop_front();
        }
        if q.len() >= MAX_PER_HOUR {
            return false;
        }
        q.push_back(t);
        true
    }
}

/// Delivers queued events, gathering those that arrive within a few seconds into one message.
pub async fn run(n: std::sync::Arc<Notifier>, mut rx: mpsc::Receiver<Batch>) {
    while let Some(Batch { events: mut batch, mut ctx }) = rx.recv().await {
        let deadline = tokio::time::Instant::now() + BATCH_WINDOW;
        while batch.len() < 50 {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(more)) => {
                    batch.extend(more.events);
                    ctx = more.ctx;
                }
                _ => break,
            }
        }
        let sent = n.deliver(&batch, &ctx, false).await;
        if sent.iter().any(|(_, r)| r.is_ok()) {
            info!("sent {} alert event(s)", batch.len());
        }
    }
}

fn security_name(s: Security) -> &'static str {
    match s {
        Security::StartTls => "starttls",
        Security::Tls => "tls",
        Security::None => "none",
    }
}

fn auth_name(a: &SmtpAuth) -> &'static str {
    match a {
        SmtpAuth::None => "none",
        SmtpAuth::Password { .. } => "password",
        SmtpAuth::OAuth2(_) => "oauth2",
    }
}

/// One line describing the alert rules, for the CLI.
pub fn rules_summary(c: &AlertConfig) -> String {
    let pct = |v: Option<f32>| v.map_or("off".to_string(), |v| format!("{v:.0}%"));
    let load = c.load_per_core.map_or("off".to_string(), |l| format!("{l} per core"));
    let report = match c.report_days {
        Some(1) => "daily",
        Some(_) => "weekly",
        None => "off",
    };
    format!(
        "CPU {} and memory {} for {} min, spikes {} for 1 min, load {load}, disk {}, apps {}, monitors on, report {report}",
        pct(c.cpu),
        pct(c.memory),
        c.after_mins,
        pct(c.spike),
        pct(c.disk),
        if c.apps { "on" } else { "off" }
    )
}

/// Channel lines for the CLI; secrets show only as "set".
pub fn channels_summary(c: &AlertConfig) -> Vec<(&'static str, String)> {
    let mut v = Vec::new();
    match &c.email {
        Some(e) => {
            let auth = match &e.auth {
                SmtpAuth::None => "no sign-in".to_string(),
                SmtpAuth::Password { user, .. } => format!("password sign-in as {user} (password set, hidden)"),
                SmtpAuth::OAuth2(o) => format!("OAuth2 as {} via {} (tokens set, hidden)", o.user, o.token_url.host),
            };
            let to = e.to.join(", ");
            v.push((
                "Email",
                format!("{}:{} {}, {auth}, from {} to {to}", e.host, e.port, security_name(e.security), e.from),
            ));
        }
        None => v.push(("Email", "off (set VPSW_SMTP_HOST and VPSW_ALERT_EMAIL)".into())),
    }
    match &c.webhook {
        Some(w) => {
            let extra = [w.signing_secret.as_ref().map(|_| "signed"), w.token.as_ref().map(|_| "bearer token")];
            let extra: Vec<&str> = extra.into_iter().flatten().collect();
            let extra = if extra.is_empty() { String::new() } else { format!(", {}", extra.join(", ")) };
            v.push(("Webhook", format!("{} ({} format{extra}); URL hidden", w.parsed.host, w.format.name())));
        }
        None => v.push(("Webhook", "off (set VPSW_WEBHOOK_URL)".into())),
    }
    v
}

/// `[host] ALERT: api is down`, or a count when several events go together.
pub fn subject(host: &str, events: &[Event]) -> String {
    match events {
        [e] => format!("[{host}] {}: {}", e.state.label(), e.title),
        _ => {
            let new = events.iter().filter(|e| e.state != State::Resolved).count();
            let resolved = events.len() - new;
            let parts: Vec<String> = [(new, "new"), (resolved, "resolved")]
                .into_iter()
                .filter(|(n, _)| *n > 0)
                .map(|(n, w)| format!("{n} {w}"))
                .collect();
            format!("[{host}] {} alerts: {}", events.len(), parts.join(", "))
        }
    }
}

/// Plain-text body shared by email and webhooks.
pub fn text(host: &str, events: &[Event], link: Option<&str>) -> String {
    let mut out = String::new();
    for e in events.iter().take(MAX_LISTED) {
        out.push_str(&format!("{:<9} {}\n", e.state.label(), e.title));
        let detail = if e.detail.is_empty() { String::new() } else { format!("{} · ", e.detail) };
        out.push_str(&format!("          {detail}{}\n\n", utc_stamp(e.ts)));
    }
    if events.len() > MAX_LISTED {
        out.push_str(&format!("…and {} more on the Alerts page.\n\n", events.len() - MAX_LISTED));
    }
    out.push_str(&format!("Server: {host}\n"));
    if let Some(l) = link {
        out.push_str(&format!("Dashboard: {l}/#/alerts\n"));
    }
    out.push_str("--\nSent by vps-watcher. Alert rules are set in /etc/vps-watcher/vps-watcher.env.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(state: State, title: &str) -> Event {
        Event { key: title.into(), state, severity: Severity::Critical, title: title.into(), detail: "d".into(), ts: 0 }
    }

    #[test]
    fn subjects() {
        assert_eq!(subject("web-1", &[ev(State::Firing, "api is down")]), "[web-1] ALERT: api is down");
        let many = [ev(State::Firing, "a"), ev(State::Notice, "b"), ev(State::Resolved, "c")];
        assert_eq!(subject("h", &many), "[h] 3 alerts: 2 new, 1 resolved");
        assert_eq!(subject("h", &[many[2].clone(), many[2].clone()]), "[h] 2 alerts: 2 resolved");
    }

    #[test]
    fn body_lists_events_and_link() {
        let t = text("web-1", &[ev(State::Firing, "api is down")], Some("https://watch.example.com"));
        assert!(t.starts_with("ALERT     api is down\n          d · 1970-01-01 00:00 UTC\n"));
        assert!(t.contains("Dashboard: https://watch.example.com/#/alerts"));
        let many: Vec<Event> = (0..25).map(|i| ev(State::Firing, &format!("e{i}"))).collect();
        assert!(text("h", &many, None).contains("…and 5 more"));
    }

    #[test]
    fn hourly_limit() {
        let n = Notifier::new(&AlertConfig::default(), None, false, "h", None);
        assert!((0..MAX_PER_HOUR).all(|_| n.take_slot()));
        assert!(!n.take_slot());
    }

    #[tokio::test]
    async fn test_without_channels_explains_setup() {
        let n = Notifier::new(&AlertConfig::default(), None, false, "h", None);
        assert!(n.test(&Context::default()).await.unwrap_err().contains("No alert channel"));
    }

    #[test]
    fn summaries_hide_secrets() {
        let m: std::collections::HashMap<&str, &str> = [
            ("VPSW_SMTP_HOST", "smtp.gmail.com"),
            ("VPSW_SMTP_USER", "me@gmail.com"),
            ("VPSW_SMTP_PASSWORD", "app-password-xyz"),
            ("VPSW_ALERT_EMAIL", "me@gmail.com"),
            ("VPSW_WEBHOOK_URL", "https://hooks.slack.com/services/T/B/secretpath"),
            ("VPSW_WEBHOOK_SECRET", "hmac-key"),
        ]
        .into_iter()
        .collect();
        let c = AlertConfig::from_vals(&|k| m.get(k).map(|v| v.to_string())).unwrap();
        let all = format!("{:?} {}", channels_summary(&c), Notifier::new(&c, None, false, "h", None).describe());
        for secret in ["app-password-xyz", "secretpath", "hmac-key"] {
            assert!(!all.contains(secret), "{secret} leaked: {all}");
        }
        assert!(all.contains("hooks.slack.com") && all.contains("password set, hidden"));
    }
}
