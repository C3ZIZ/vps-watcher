//! Monitors the user adds by hand: TCP port, HTTP(S) URL, process match and log file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;

use crate::net::{self, is_valid_host, parse_url};
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
async fn fetch_status(u: &net::Url) -> Result<u16, String> {
    net::request(hyper::Method::GET, u, &[], None, 0).await.map(|r| r.status)
}

/// Size and age of a log file monitor's target.
pub fn check_logfile(path: &str) -> CheckResult {
    let t = Instant::now();
    // Same safe open as the log viewer: a swapped-in symlink or FIFO can neither leak nor hang us.
    let file = match crate::util::open_regular(Path::new(path)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return CheckResult::down(t, "not readable by vps-watcher");
        }
        Err(e) => return CheckResult::down(t, e.to_string()),
    };
    let Ok(m) = file.metadata() else { return CheckResult::down(t, "cannot read file details") };
    let age = m.modified().ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
    CheckResult {
        up: true,
        ms: t.elapsed().as_secs_f32() * 1000.0,
        detail: format!("{} KB, updated {} ago", m.len() / 1024, crate::util::fmt_age(age)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
