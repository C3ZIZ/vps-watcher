//! Monitors the user adds by hand: TCP port, HTTP(S) URL, process match and log file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use http_body_util::Empty;
use hyper::body::Bytes;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{self, pki_types::ServerName};

use crate::util::{atomic_write, random_hex};

/// Seconds a single check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);
/// Upper bound on monitors so the config stays small.
pub const MAX_MONITORS: usize = 100;

/// A user-defined monitor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Monitor {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: MonitorKind,
}

/// What a monitor watches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MonitorKind {
    Tcp {
        host: String,
        port: u16,
    },
    Http {
        url: String,
        #[serde(default)]
        expect_status: Option<u16>,
    },
    Process {
        pattern: String,
    },
    Logfile {
        path: String,
    },
}

impl Monitor {
    /// Checks the fields a user typed, returning a message fit for the UI.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err("Name must be 1 to 60 characters.".into());
        }
        match &self.kind {
            MonitorKind::Tcp { host, port } => {
                if *port == 0 {
                    return Err("Port must be between 1 and 65535.".into());
                }
                if !is_valid_host(host) {
                    return Err("Host must be a hostname or IP address.".into());
                }
            }
            MonitorKind::Http { url, expect_status } => {
                parse_url(url)?;
                if expect_status.is_some_and(|s| !(100..=599).contains(&s)) {
                    return Err("Expected status must be between 100 and 599.".into());
                }
            }
            MonitorKind::Process { pattern } => {
                if pattern.trim().is_empty() || pattern.len() > 100 {
                    return Err("Process pattern must be 1 to 100 characters.".into());
                }
            }
            MonitorKind::Logfile { path } => {
                let p = Path::new(path);
                if !p.is_absolute() || path.contains("/../") || path.ends_with("/..") {
                    return Err("Log file path must be an absolute path.".into());
                }
            }
        }
        Ok(())
    }
}

/// Host names and IP literals only; no schemes, paths or spaces.
fn is_valid_host(h: &str) -> bool {
    let h = h.trim_start_matches('[').trim_end_matches(']');
    !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:_".contains(&b))
}

/// Saved list of monitors (monitors.json in the data dir).
#[derive(Debug)]
pub struct MonitorStore {
    path: PathBuf,
    list: Vec<Monitor>,
}

impl MonitorStore {
    /// Loads monitors from `dir`; a missing file means none.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("monitors.json");
        let list = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{} is invalid: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(MonitorStore { path, list })
    }

    /// All monitors.
    pub fn list(&self) -> &[Monitor] {
        &self.list
    }

    /// Validates, assigns an id and saves a new monitor.
    pub fn add(&mut self, mut m: Monitor) -> Result<Monitor, String> {
        m.validate()?;
        if self.list.len() >= MAX_MONITORS {
            return Err(format!("You can have at most {MAX_MONITORS} monitors."));
        }
        m.name = m.name.trim().to_string();
        m.id = random_hex(4);
        self.list.push(m.clone());
        self.save()?;
        Ok(m)
    }

    /// Removes a monitor by id; returns false if it did not exist.
    pub fn remove(&mut self, id: &str) -> Result<bool, String> {
        let before = self.list.len();
        self.list.retain(|m| m.id != id);
        if self.list.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&self.list).map_err(|e| e.to_string())?;
        atomic_write(&self.path, &json, false).map_err(|e| format!("cannot save monitors: {e}"))
    }
}

/// Outcome of one availability check.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CheckResult {
    pub up: bool,
    pub ms: f32,
    pub detail: String,
}

impl CheckResult {
    fn down(started: Instant, detail: impl Into<String>) -> Self {
        CheckResult { up: false, ms: started.elapsed().as_secs_f32() * 1000.0, detail: detail.into() }
    }
}

/// Opens a TCP connection to `host:port`.
pub async fn check_tcp(host: &str, port: u16) -> CheckResult {
    let t = Instant::now();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match tokio::time::timeout(CHECK_TIMEOUT, TcpStream::connect((host, port))).await {
        Ok(Ok(_)) => CheckResult { up: true, ms: t.elapsed().as_secs_f32() * 1000.0, detail: "open".into() },
        Ok(Err(e)) => CheckResult::down(t, e.to_string()),
        Err(_) => CheckResult::down(t, "timed out"),
    }
}

/// Parts of an http(s) URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Parses `http(s)://host[:port][/path]`; IPv6 hosts go in brackets.
pub fn parse_url(s: &str) -> Result<Url, String> {
    let bad = || "URL must look like https://example.com/health".to_string();
    let s = s.trim();
    let (https, rest) = if let Some(r) = s.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(bad());
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.contains('@') {
        return Err("URLs with user:password are not supported.".into());
    }
    let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']')) {
        let host = &authority[..end + 2];
        let after = &authority[end + 2..];
        (host.to_string(), after.strip_prefix(':').map(str::parse::<u16>))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.parse::<u16>())),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        None => {
            if https {
                443
            } else {
                80
            }
        }
        Some(Ok(p)) if p > 0 => p,
        Some(_) => return Err(bad()),
    };
    if !is_valid_host(&host) || path.contains(' ') {
        return Err(bad());
    }
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
    Ok(Url { https, host, port, path })
}

/// Requests `url` and reports up when the status matches (default: below 400).
pub async fn check_http(url: &str, expect: Option<u16>) -> CheckResult {
    let t = Instant::now();
    let u = match parse_url(url) {
        Ok(u) => u,
        Err(e) => return CheckResult::down(t, e),
    };
    match tokio::time::timeout(CHECK_TIMEOUT, fetch_status(&u)).await {
        Ok(Ok(code)) => {
            let up = match expect {
                Some(e) => code == e,
                None => code < 400,
            };
            CheckResult { up, ms: t.elapsed().as_secs_f32() * 1000.0, detail: format!("HTTP {code}") }
        }
        Ok(Err(e)) => CheckResult::down(t, e),
        Err(_) => CheckResult::down(t, "timed out"),
    }
}

/// Connects (with TLS for https) and returns the response status code.
async fn fetch_status(u: &Url) -> Result<u16, String> {
    let host = u.host.trim_start_matches('[').trim_end_matches(']').to_string();
    let tcp = TcpStream::connect((host.as_str(), u.port)).await.map_err(|e| e.to_string())?;
    let default_port = if u.https { 443 } else { 80 };
    let host_header = if u.port == default_port { u.host.clone() } else { format!("{}:{}", u.host, u.port) };
    if u.https {
        let name = ServerName::try_from(host).map_err(|_| "invalid TLS server name".to_string())?;
        let tls = TlsConnector::from(tls_config()).connect(name, tcp).await.map_err(|e| format!("TLS: {e}"))?;
        send_get(tls, &host_header, &u.path).await
    } else {
        send_get(tcp, &host_header, &u.path).await
    }
}

/// Sends a GET over an established stream and returns only the status.
async fn send_get<S>(io: S, host: &str, path: &str) -> Result<u16, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = Request::get(path)
        .header(header::HOST, host)
        .header(header::USER_AGENT, concat!("vps-watcher/", env!("CARGO_PKG_VERSION")))
        .header(header::CONNECTION, "close")
        .body(Empty::<Bytes>::new())
        .map_err(|e| e.to_string())?;
    let res = tx.send_request(req).await.map_err(|e| e.to_string())?;
    Ok(res.status().as_u16())
}

/// Shared TLS client settings with the Mozilla root store, built once.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

/// Size and age of a log file monitor's target.
pub fn check_logfile(path: &str) -> CheckResult {
    let t = Instant::now();
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {
            let age = m.modified().ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
            let readable = std::fs::File::open(path).is_ok();
            CheckResult {
                up: readable,
                ms: t.elapsed().as_secs_f32() * 1000.0,
                detail: if readable {
                    format!("{} KB, updated {} ago", m.len() / 1024, crate::util::fmt_age(age))
                } else {
                    "not readable by vps-watcher".into()
                },
            }
        }
        Ok(_) => CheckResult::down(t, "not a regular file"),
        Err(e) => CheckResult::down(t, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let u = parse_url("https://example.com/health?x=1").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.path.as_str()), (true, "example.com", 443, "/health?x=1"));
        let u = parse_url("http://127.0.0.1:8080").unwrap();
        assert_eq!((u.port, u.path.as_str()), (8080, "/"));
        let u = parse_url("http://[::1]:9000/x").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("[::1]", 9000));
        assert_eq!(parse_url("http://h?q").unwrap().path, "/?q");
        for bad in ["ftp://x", "http://", "http://a b", "http://u:p@h/", "http://h:0", "http://h:99999", "example.com"]
        {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validation_messages() {
        let m = |kind| Monitor { id: String::new(), name: "x".into(), kind };
        assert!(m(MonitorKind::Tcp { host: "db".into(), port: 5432 }).validate().is_ok());
        assert!(m(MonitorKind::Tcp { host: "a/b".into(), port: 1 }).validate().is_err());
        assert!(m(MonitorKind::Tcp { host: "db".into(), port: 0 }).validate().is_err());
        assert!(m(MonitorKind::Logfile { path: "rel/x.log".into() }).validate().is_err());
        assert!(m(MonitorKind::Logfile { path: "/var/log/../etc/shadow".into() }).validate().is_err());
        assert!(m(MonitorKind::Http { url: "https://x.io".into(), expect_status: Some(99) }).validate().is_err());
        let mut long = m(MonitorKind::Process { pattern: "node".into() });
        long.name = "n".repeat(61);
        assert!(long.validate().is_err());
    }

    #[test]
    fn json_shape() {
        let m: Monitor = serde_json::from_str(r#"{"name":"api","type":"http","url":"http://x"}"#).unwrap();
        assert_eq!(m.kind, MonitorKind::Http { url: "http://x".into(), expect_status: None });
    }

    #[test]
    fn store_add_remove_persist() {
        let d = tempfile::tempdir().unwrap();
        let mut s = MonitorStore::load(d.path()).unwrap();
        let m = s
            .add(Monitor {
                id: "ignored".into(),
                name: " db ".into(),
                kind: MonitorKind::Tcp { host: "localhost".into(), port: 5432 },
            })
            .unwrap();
        assert_ne!(m.id, "ignored");
        assert_eq!(m.name, "db");
        assert_eq!(MonitorStore::load(d.path()).unwrap().list().len(), 1);
        assert!(s.remove(&m.id).unwrap());
        assert!(!s.remove(&m.id).unwrap());
    }

    #[tokio::test]
    async fn tcp_check_up_and_down() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(check_tcp("127.0.0.1", port).await.up);
        drop(l);
        assert!(!check_tcp("127.0.0.1", port).await.up);
    }

    #[tokio::test]
    async fn http_check_reads_status() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n").await;
        });
        let r = check_http(&format!("http://127.0.0.1:{port}/"), None).await;
        assert!(!r.up);
        assert_eq!(r.detail, "HTTP 503");
    }
}
