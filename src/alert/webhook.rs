//! Webhook delivery: Slack, Discord, plain text or a JSON body, optionally HMAC-signed.

use std::time::Duration;

use serde_json::json;

use super::engine::Event;
use super::settings::{WebhookConfig, WebhookFormat};
use crate::net;
use crate::util::now;

const POST_TIMEOUT: Duration = Duration::from_secs(15);
/// Discord rejects messages over 2000 characters.
const DISCORD_MAX: usize = 1900;

/// A request ready to send: content type, extra headers and body.
pub struct Payload {
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// Builds the request body in the configured format.
pub fn payload(
    cfg: &WebhookConfig,
    host: &str,
    subject: &str,
    text: &str,
    events: &[Event],
    link: Option<&str>,
) -> Payload {
    let json_body = |v: serde_json::Value| serde_json::to_vec(&v).unwrap_or_default();
    let mut headers = Vec::new();
    let (content_type, body) = match cfg.format {
        WebhookFormat::Slack => {
            // Slack reads <...> as links and mentions, so a name like <!channel> must not ping anyone.
            let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            ("application/json", json_body(json!({ "text": format!("*{}*\n{}", esc(subject), esc(text)) })))
        }
        WebhookFormat::Discord => {
            let mut content = format!("**{subject}**\n{text}");
            if content.chars().count() > DISCORD_MAX {
                content = content.chars().take(DISCORD_MAX).collect::<String>() + "…";
            }
            ("application/json", json_body(json!({ "content": content, "allowed_mentions": { "parse": [] } })))
        }
        WebhookFormat::Text => {
            // ntfy reads these headers; other receivers ignore them. Header values must be ASCII.
            if subject.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                headers.push(("x-title", subject.to_string()));
            }
            if events.iter().any(|e| e.state == super::engine::State::Firing) {
                headers.push(("x-priority", "high".into()));
            }
            ("text/plain; charset=utf-8", text.as_bytes().to_vec())
        }
        WebhookFormat::Json => (
            "application/json",
            json_body(json!({
                "source": "vps-watcher",
                "host": host,
                "subject": subject,
                "text": text,
                "link": link,
                "events": events,
            })),
        ),
    };
    if let Some(secret) = &cfg.signing_secret {
        let ts = now().to_string();
        headers.push(("x-vpsw-timestamp", ts.clone()));
        headers.push(("x-vpsw-signature", format!("sha256={}", sign(secret.expose(), &ts, &body))));
    }
    if let Some(t) = &cfg.token {
        headers.push(("authorization", format!("Bearer {}", t.expose())));
    }
    Payload { content_type, headers, body }
}

/// HMAC-SHA256 over `"<timestamp>.<body>"`, hex encoded; the timestamp stops replays.
pub fn sign(secret: &str, ts: &str, body: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let mut ctx = ring::hmac::Context::with_key(&key);
    ctx.update(ts.as_bytes());
    ctx.update(b".");
    ctx.update(body);
    ctx.sign().as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Posts the payload; any 2xx is success. Errors name the host only, never the secret URL.
pub async fn post(cfg: &WebhookConfig, p: Payload) -> Result<(), String> {
    let headers: Vec<(&str, &str)> = p.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let req = net::request(hyper::Method::POST, &cfg.parsed, &headers, Some((p.content_type, p.body)), 2048);
    let res = tokio::time::timeout(POST_TIMEOUT, req)
        .await
        .map_err(|_| format!("{} did not answer in time", cfg.parsed.host))??;
    if (200..300).contains(&res.status) {
        return Ok(());
    }
    // Receivers often echo the request path (the secret key) in error pages; hide it.
    let mut snippet: String =
        String::from_utf8_lossy(&res.body).chars().filter(|c| !c.is_control()).take(150).collect();
    let path = cfg.parsed.path.split('?').next().unwrap_or("");
    for secret in [path, cfg.token.as_ref().map_or("", |t| t.expose())] {
        if secret.len() > 1 {
            snippet = snippet.replace(secret, "[hidden]");
        }
    }
    Err(format!("{} answered HTTP {} {}", cfg.parsed.host, res.status, snippet).trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::super::engine::{Severity, State};
    use super::*;
    use crate::alert::settings::Secret;

    fn hook(url: &str, format: WebhookFormat) -> WebhookConfig {
        WebhookConfig {
            url: Secret::new(url),
            parsed: net::parse_url(url).unwrap(),
            format,
            signing_secret: None,
            token: None,
        }
    }

    fn ev() -> Vec<Event> {
        vec![Event {
            key: "mon:1".into(),
            state: State::Firing,
            severity: Severity::Critical,
            title: "api is down".into(),
            detail: "HTTP 503".into(),
            ts: 1,
        }]
    }

    #[test]
    fn formats() {
        let body = |f| {
            let p = payload(&hook("https://x.io/h", f), "web-1", "[web-1] api is down", "line", &ev(), None);
            serde_json::from_slice::<serde_json::Value>(&p.body).ok()
        };
        assert_eq!(body(WebhookFormat::Slack).unwrap()["text"], "*[web-1] api is down*\nline");
        assert_eq!(body(WebhookFormat::Discord).unwrap()["content"], "**[web-1] api is down**\nline");
        let j = body(WebhookFormat::Json).unwrap();
        assert_eq!((j["host"].as_str(), j["events"][0]["state"].as_str()), (Some("web-1"), Some("firing")));
        let t = payload(&hook("https://ntfy.sh/t", WebhookFormat::Text), "h", "Subj", "plain", &ev(), None);
        assert_eq!((t.body.as_slice(), t.content_type), (&b"plain"[..], "text/plain; charset=utf-8"));
        assert!(t.headers.contains(&("x-priority", "high".into())));
    }

    #[test]
    fn slack_text_cannot_mention_or_link() {
        let p = payload(
            &hook("https://x.io/h", WebhookFormat::Slack),
            "h",
            "<!channel> & co",
            "<https://evil|click>",
            &ev(),
            None,
        );
        let v: serde_json::Value = serde_json::from_slice(&p.body).unwrap();
        assert_eq!(v["text"], "*&lt;!channel&gt; &amp; co*\n&lt;https://evil|click&gt;");
    }

    #[test]
    fn discord_is_truncated_and_mentions_disabled() {
        let long = "x".repeat(5000);
        let p = payload(
            &hook("https://discord.com/api/webhooks/1/a", WebhookFormat::Discord),
            "h",
            "s",
            &long,
            &ev(),
            None,
        );
        let v: serde_json::Value = serde_json::from_slice(&p.body).unwrap();
        assert!(v["content"].as_str().unwrap().chars().count() <= DISCORD_MAX + 1);
        assert_eq!(v["allowed_mentions"]["parse"], json!([]));
    }

    #[test]
    fn signature_matches_known_hmac() {
        // Python: hmac.new(b's', b'1.{}', hashlib.sha256).hexdigest()
        assert_eq!(sign("s", "1", b"{}"), "b253639bcd705ed1cda3073f66104f16988bd40272584dea7d0d345ae93e61cc");
        assert_ne!(sign("s", "1", b"{}"), sign("s", "2", b"{}"), "timestamp is covered");
    }

    #[tokio::test]
    async fn error_names_host_not_url() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b).await;
            let _ = s
                .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 34\r\n\r\nCannot POST /secret-key-123 (gone)")
                .await;
        });
        let cfg = hook(&format!("http://127.0.0.1:{port}/secret-key-123"), WebhookFormat::Json);
        let p = payload(&cfg, "h", "s", "t", &ev(), None);
        let e = post(&cfg, p).await.unwrap_err();
        assert!(e.contains("HTTP 404 Cannot POST [hidden]") && !e.contains("secret-key-123"), "{e}");
    }
}
