//! A small SMTP client: STARTTLS or implicit TLS, AUTH PLAIN/LOGIN or XOAUTH2, one plain-text message.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::oauth::TokenSource;
use super::settings::{EmailConfig, Security, SmtpAuth};
use crate::net;
use crate::util::{base64, now, random_hex, rfc2822};

/// Whole conversation, from connect to QUIT.
const SEND_TIMEOUT: Duration = Duration::from_secs(45);
/// Longest reply line we accept (RFC 5321 allows 512; be generous).
const MAX_LINE: u64 = 4096;
/// Most lines in one multi-line reply (EHLO lists capabilities).
const MAX_LINES: usize = 100;

/// Sends alert emails with the configured server and sign-in.
pub struct Mailer {
    pub cfg: EmailConfig,
    oauth: Option<TokenSource>,
    helo: String,
}

impl Mailer {
    pub fn new(cfg: EmailConfig, oauth: Option<TokenSource>, hostname: &str) -> Self {
        // EHLO wants a domain; fall back to a literal when the hostname has odd characters.
        let ok = !hostname.is_empty() && hostname.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.".contains(&b));
        Mailer { cfg, oauth, helo: if ok { hostname.to_string() } else { "[127.0.0.1]".into() } }
    }

    /// Sends one message to every recipient; `html` adds a rich version next to the text.
    pub async fn send(&self, subject: &str, body: &str, html: Option<&str>) -> Result<(), String> {
        let msg = build_message(&self.cfg.from, &self.cfg.to, subject, body, html, &self.helo, now());
        tokio::time::timeout(SEND_TIMEOUT, self.deliver(&msg))
            .await
            .map_err(|_| "timed out talking to the mail server".to_string())?
    }

    async fn deliver(&self, msg: &str) -> Result<(), String> {
        let c = &self.cfg;
        let tcp = TcpStream::connect((c.host.as_str(), c.port))
            .await
            .map_err(|e| format!("cannot connect to {}:{}: {e}", c.host, c.port))?;
        match c.security {
            Security::Tls => {
                let tls = net::tls_connect(&c.host, tcp).await?;
                let mut s = Smtp::new(tls);
                s.expect(&[220]).await?;
                let caps = s.ehlo(&self.helo).await?;
                self.transact(&mut s, &caps, msg).await
            }
            Security::StartTls => {
                let mut s = Smtp::new(tcp);
                s.expect(&[220]).await?;
                let caps = s.ehlo(&self.helo).await?;
                if !caps.iter().any(|l| l.eq_ignore_ascii_case("STARTTLS")) {
                    return Err(
                        "the mail server does not offer STARTTLS (for port 465 use VPSW_SMTP_SECURITY=tls)".into()
                    );
                }
                s.cmd("STARTTLS", &[220]).await?;
                let tls = net::tls_connect(&c.host, s.into_plain()?).await?;
                let mut s = Smtp::new(tls);
                let caps = s.ehlo(&self.helo).await?;
                self.transact(&mut s, &caps, msg).await
            }
            Security::None => {
                let mut s = Smtp::new(tcp);
                s.expect(&[220]).await?;
                let caps = s.ehlo(&self.helo).await?;
                self.transact(&mut s, &caps, msg).await
            }
        }
    }

    /// Signs in (if configured), then sends the envelope and the message.
    async fn transact<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        s: &mut Smtp<S>,
        caps: &[String],
        msg: &str,
    ) -> Result<(), String> {
        let mechs: Vec<String> = caps
            .iter()
            .filter_map(|l| l.strip_prefix("AUTH ").or_else(|| l.strip_prefix("AUTH=")))
            .flat_map(|m| m.split_whitespace().map(str::to_ascii_uppercase))
            .collect();
        let has = |m: &str| mechs.iter().any(|x| x == m);
        match &self.cfg.auth {
            SmtpAuth::None => {}
            SmtpAuth::Password { user, password } => {
                if has("PLAIN") {
                    let blob = base64(format!("\0{user}\0{}", password.expose()).as_bytes());
                    s.secret_cmd(&format!("AUTH PLAIN {blob}"), &[235]).await?;
                } else if has("LOGIN") {
                    s.cmd("AUTH LOGIN", &[334]).await?;
                    s.secret_cmd(&base64(user.as_bytes()), &[334]).await?;
                    s.secret_cmd(&base64(password.expose().as_bytes()), &[235]).await?;
                } else {
                    return Err("the mail server offers no password sign-in (PLAIN or LOGIN)".into());
                }
            }
            SmtpAuth::OAuth2(o) => {
                if !has("XOAUTH2") {
                    return Err("the mail server does not support OAuth (XOAUTH2) sign-in".into());
                }
                let token = self.oauth.as_ref().ok_or("OAuth is not set up")?.access_token().await?;
                let blob = base64(format!("user={}\x01auth=Bearer {token}\x01\x01", o.user).as_bytes());
                s.write_line(&format!("AUTH XOAUTH2 {blob}")).await?;
                let (code, text) = s.reply().await?;
                if code == 334 {
                    // The server sent an error challenge; an empty line ends the exchange with the final code.
                    s.write_line("").await?;
                    let (code, text) = s.reply().await?;
                    return Err(format!("the mail server rejected the OAuth token: {code} {}", clip(&text)));
                }
                if code != 235 {
                    return Err(format!("OAuth sign-in failed: {code} {}", clip(&text)));
                }
            }
        }
        s.cmd(&format!("MAIL FROM:<{}>", self.cfg.from), &[250]).await?;
        for rcpt in &self.cfg.to {
            s.cmd(&format!("RCPT TO:<{rcpt}>"), &[250, 251]).await?;
        }
        s.cmd("DATA", &[354]).await?;
        s.write_raw(msg.as_bytes()).await?;
        s.cmd(".", &[250]).await?;
        let _ = s.cmd("QUIT", &[221]).await;
        Ok(())
    }
}

/// One SMTP connection with buffered reads.
struct Smtp<S> {
    r: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Smtp<S> {
    fn new(io: S) -> Self {
        Smtp { r: BufReader::new(io) }
    }

    /// Reads one (possibly multi-line) reply: the code and the text of each line.
    async fn reply(&mut self) -> Result<(u16, String), String> {
        let mut text = Vec::new();
        for _ in 0..MAX_LINES {
            let mut line = Vec::new();
            let n = (&mut self.r).take(MAX_LINE).read_until(b'\n', &mut line).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("the mail server closed the connection".into());
            }
            let line = String::from_utf8_lossy(&line).trim_end().to_string();
            let code =
                line.get(..3).and_then(|c| c.parse::<u16>().ok()).ok_or("the mail server sent a malformed reply")?;
            let more = line.as_bytes().get(3) == Some(&b'-');
            text.push(line.get(4..).unwrap_or("").to_string());
            if !more {
                return Ok((code, text.join("\n")));
            }
        }
        Err("the mail server sent an overly long reply".into())
    }

    async fn expect(&mut self, ok: &[u16]) -> Result<String, String> {
        let (code, text) = self.reply().await?;
        if ok.contains(&code) { Ok(text) } else { Err(format!("the mail server said: {code} {}", clip(&text))) }
    }

    async fn write_line(&mut self, line: &str) -> Result<(), String> {
        self.write_raw(format!("{line}\r\n").as_bytes()).await
    }

    async fn write_raw(&mut self, b: &[u8]) -> Result<(), String> {
        let w = self.r.get_mut();
        w.write_all(b).await.map_err(|e| e.to_string())?;
        w.flush().await.map_err(|e| e.to_string())
    }

    async fn cmd(&mut self, line: &str, ok: &[u16]) -> Result<String, String> {
        self.write_line(line).await?;
        self.expect(ok).await
    }

    /// Like `cmd`, but a failure never repeats what was sent.
    async fn secret_cmd(&mut self, line: &str, ok: &[u16]) -> Result<(), String> {
        self.write_line(line).await?;
        let (code, text) = self.reply().await?;
        if ok.contains(&code) {
            Ok(())
        } else {
            Err(format!("the mail server refused the sign-in: {code} {}", clip(&text)))
        }
    }

    /// EHLO, returning one capability per line.
    async fn ehlo(&mut self, name: &str) -> Result<Vec<String>, String> {
        let text = self.cmd(&format!("EHLO {name}"), &[250]).await?;
        Ok(text.lines().skip(1).map(|l| l.trim().to_string()).collect())
    }
}

impl Smtp<TcpStream> {
    /// The raw socket for the TLS upgrade. Bytes the server sent early would be read as if
    /// encrypted (a known STARTTLS injection), so any leftover input aborts.
    fn into_plain(self) -> Result<TcpStream, String> {
        if !self.r.buffer().is_empty() {
            return Err("the mail server sent unexpected data before TLS started".into());
        }
        Ok(self.r.into_inner())
    }
}

/// Shortens server text for error messages.
fn clip(s: &str) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() > 200 { s.chars().take(200).collect::<String>() + "…" } else { s }
}

/// Encodes a header value as RFC 2047 UTF-8 words when it is not plain ASCII.
fn encode_header(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    if s.is_ascii() {
        return s;
    }
    // Encoded words stay under 75 characters: 45 bytes become 60 base64 characters.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in s.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
    }
    words.join("\r\n ")
}

/// Base64 wrapped at 76 characters, so no line can be a lone "." that ends DATA early.
fn b64_lines(text: &str) -> String {
    let b64 = base64(text.as_bytes());
    b64.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join("\r\n")
}

/// A complete message: text/plain, or multipart/alternative with an HTML part.
fn build_message(
    from: &str,
    to: &[String],
    subject: &str,
    text: &str,
    html: Option<&str>,
    domain: &str,
    ts: u64,
) -> String {
    let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
    let msg_domain = from.rsplit_once('@').map_or(domain, |(_, d)| d);
    let head = format!(
        "Date: {date}\r\nFrom: vps-watcher <{from}>\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{id}@{msg_domain}>\r\n\
         MIME-Version: 1.0\r\nAuto-Submitted: auto-generated\r\nX-Mailer: vps-watcher\r\n",
        date = rfc2822(ts),
        to = to.join(", "),
        subject = encode_header(subject),
        id = random_hex(12),
    );
    let part = |ctype: &str, body: &str| {
        format!(
            "Content-Type: {ctype}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64_lines(&crlf(body))
        )
    };
    match html {
        None => format!("{head}{}", part("text/plain", text)),
        Some(html) => {
            // A random boundary cannot appear inside base64 parts.
            let b = format!("vpsw-{}", random_hex(12));
            format!(
                "{head}Content-Type: multipart/alternative; boundary=\"{b}\"\r\n\r\n--{b}\r\n{}--{b}\r\n{}--{b}--\r\n",
                part("text/plain", text),
                part("text/html", html)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::settings::Secret;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn message_is_safe_and_well_formed() {
        let m =
            build_message("a@x.io", &["b@y.io".into()], "Disk / is 95% full\r\nBcc: evil@z", "\n.\nline", None, "h", 0);
        assert!(m.contains("Subject: Disk / is 95% fullBcc: evil@z\r\n"), "CR/LF stripped, no header injection");
        assert!(m.contains("Date: Thu, 01 Jan 1970 00:00:00 +0000\r\n"));
        assert!(m.contains("Message-ID: <") && m.contains("@x.io>"));
        assert!(!m.contains("\r\n.\r\n"), "a lone dot cannot end DATA early");
        assert!(m.contains(&base64(b"\r\n.\r\nline")));
        assert!(m.contains("Content-Type: text/plain; charset=utf-8"));
    }

    #[test]
    fn html_goes_in_a_multipart_alternative() {
        let m = build_message("a@x.io", &["b@y.io".into()], "s", "plain", Some("<b>rich</b>"), "h", 0);
        let b = m.split("boundary=\"").nth(1).unwrap().split('"').next().unwrap();
        assert_eq!(m.matches(&format!("--{b}\r\n")).count(), 2);
        assert!(m.ends_with(&format!("--{b}--\r\n")));
        let text_at = m.find("text/plain").unwrap();
        assert!(text_at < m.find("text/html").unwrap(), "text first, so clients prefer HTML");
        assert!(m.contains(&base64(b"<b>rich</b>")));
    }

    #[test]
    fn non_ascii_subject_is_encoded() {
        let s = encode_header("خادم: القرص ممتلئ تقريبا وهذا عنوان طويل جدا");
        assert!(s.starts_with("=?UTF-8?B?") && s.contains("\r\n =?UTF-8?B?"));
        assert!(s.split("\r\n ").all(|w| w.len() <= 75));
    }

    /// Runs a scripted SMTP server: each entry is (expected command prefix, reply).
    async fn fake_server(script: Vec<(&'static str, &'static str)>) -> (u16, tokio::task::JoinHandle<Vec<String>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let (r, mut w) = s.into_split();
            let mut r = BufReader::new(r);
            w.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            let mut seen = Vec::new();
            let mut in_data = false;
            for (want, reply) in script {
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap() == 0 {
                        return seen;
                    }
                    if !in_data || line == ".\r\n" {
                        break;
                    }
                }
                in_data = want == "DATA";
                assert!(line.starts_with(want), "expected {want:?}, got {line:?}");
                seen.push(line.trim_end().to_string());
                w.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });
        (port, h)
    }

    fn mailer(port: u16, auth: SmtpAuth) -> Mailer {
        let cfg = EmailConfig {
            host: "127.0.0.1".into(),
            port,
            security: Security::None,
            auth,
            from: "vpsw@example.com".into(),
            to: vec!["ops@example.com".into(), "me@example.com".into()],
        };
        Mailer::new(cfg, None, "web-1")
    }

    #[tokio::test]
    async fn sends_with_auth_plain() {
        let (port, server) = fake_server(vec![
            ("EHLO web-1", "250-fake\r\n250-AUTH LOGIN PLAIN\r\n250 8BITMIME\r\n"),
            ("AUTH PLAIN ", "235 ok\r\n"),
            ("MAIL FROM:<vpsw@example.com>", "250 ok\r\n"),
            ("RCPT TO:<ops@example.com>", "250 ok\r\n"),
            ("RCPT TO:<me@example.com>", "251 forwarded\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "250 queued\r\n"),
            ("QUIT", "221 bye\r\n"),
        ])
        .await;
        let auth = SmtpAuth::Password { user: "u".into(), password: Secret::new("p@ss") };
        mailer(port, auth).send("hi", "body", None).await.unwrap();
        let seen = server.await.unwrap();
        assert_eq!(seen[1], format!("AUTH PLAIN {}", base64(b"\0u\0p@ss")));
    }

    #[tokio::test]
    async fn auth_failure_never_echoes_the_password() {
        let (port, _server) = fake_server(vec![
            ("EHLO", "250-fake\r\n250 AUTH LOGIN\r\n"),
            ("AUTH LOGIN", "334 VXNlcm5hbWU6\r\n"),
            ("", "334 UGFzc3dvcmQ6\r\n"),
            ("", "535 5.7.8 Username and Password not accepted\r\n"),
        ])
        .await;
        let auth = SmtpAuth::Password { user: "u".into(), password: Secret::new("hunter2-secret") };
        let e = mailer(port, auth).send("hi", "body", None).await.unwrap_err();
        assert!(e.contains("535") && !e.contains("hunter2") && !e.contains(&base64(b"hunter2-secret")), "{e}");
    }

    #[tokio::test]
    async fn rejected_recipient_is_reported() {
        let (port, _server) = fake_server(vec![
            ("EHLO", "250 fake\r\n"),
            ("MAIL FROM", "250 ok\r\n"),
            ("RCPT TO", "550 5.1.1 no such user\r\n"),
        ])
        .await;
        let e = mailer(port, SmtpAuth::None).send("hi", "body", None).await.unwrap_err();
        assert!(e.contains("550 5.1.1 no such user"), "{e}");
    }

    #[tokio::test]
    async fn starttls_missing_is_a_clear_error() {
        let (port, _server) = fake_server(vec![("EHLO", "250-fake\r\n250 SIZE 1000\r\n")]).await;
        let mut m = mailer(port, SmtpAuth::None);
        m.cfg.security = Security::StartTls;
        let e = m.send("hi", "body", None).await.unwrap_err();
        assert!(e.contains("does not offer STARTTLS"), "{e}");
    }
}
