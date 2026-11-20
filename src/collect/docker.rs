//! Read-only Docker Engine API client over the Unix socket (GET requests only).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::UnixStream;

use super::logs::{LogLine, parse_rfc3339};

/// Largest response we will buffer from the Docker API.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Handle to the Docker socket.
#[derive(Debug, Clone)]
pub struct Docker {
    socket: PathBuf,
}

/// One row of `GET /containers/json`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct ContainerSummary {
    pub id: String,
    pub names: Vec<String>,
    pub image: String,
    pub state: String,
    pub status: String,
    pub labels: HashMap<String, String>,
    pub ports: Vec<PortMap>,
}

impl ContainerSummary {
    /// Name without Docker's leading slash.
    pub fn name(&self) -> String {
        self.names.first().map(|n| n.trim_start_matches('/').to_string()).unwrap_or_else(|| self.short_id())
    }

    /// First 12 characters of the id.
    pub fn short_id(&self) -> String {
        self.id.chars().take(12).collect()
    }

    /// Docker Compose project, if any.
    pub fn project(&self) -> Option<String> {
        self.labels.get("com.docker.compose.project").cloned()
    }
}

/// A published or exposed port.
#[derive(Debug, Clone, Deserialize, Default, serde::Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct PortMap {
    #[serde(rename = "IP")]
    pub ip: Option<String>,
    pub private_port: u16,
    pub public_port: Option<u16>,
    #[serde(rename = "Type")]
    pub proto: String,
}

/// The parts of `GET /containers/{id}/json` we use.
#[derive(Debug, Clone, Default)]
pub struct ContainerInfo {
    pub pid: u32,
    pub restarts: u64,
    pub health: Option<String>,
    pub host_network: bool,
    pub tty: bool,
    /// The last exit was the kernel OOM killer.
    pub oom_killed: bool,
}

impl Docker {
    /// Creates a client for the socket at `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Docker { socket: socket.into() }
    }

    /// True if the socket file exists at all.
    pub fn present(&self) -> bool {
        self.socket.exists()
    }

    /// Sends a GET and returns the body, failing on non-2xx, oversize or a 5s timeout.
    async fn get(&self, path: &str) -> Result<Bytes, String> {
        let fut = async {
            let stream = UnixStream::connect(&self.socket)
                .await
                .map_err(|e| format!("cannot connect to {}: {e}", self.socket.display()))?;
            let (mut tx, conn) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            let req = Request::get(path)
                .header(header::HOST, "docker")
                .body(Empty::<Bytes>::new())
                .map_err(|e| e.to_string())?;
            let res = tx.send_request(req).await.map_err(|e| e.to_string())?;
            let status = res.status();
            let body = Limited::new(res.into_body(), MAX_BODY)
                .collect()
                .await
                .map_err(|e| format!("docker response: {e}"))?
                .to_bytes();
            if !status.is_success() {
                let msg = String::from_utf8_lossy(&body[..body.len().min(200)]).trim().to_string();
                return Err(format!("docker API {status}: {msg}"));
            }
            Ok(body)
        };
        tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| "docker API timed out".to_string())?
    }

    /// Lists all containers, running or not.
    pub async fn list(&self) -> Result<Vec<ContainerSummary>, String> {
        let body = self.get("/containers/json?all=1").await?;
        serde_json::from_slice(&body).map_err(|e| format!("docker list: {e}"))
    }

    /// Fetches runtime details of one container.
    pub async fn inspect(&self, id: &str) -> Result<ContainerInfo, String> {
        let body = self.get(&format!("/containers/{}/json", safe_id(id)?)).await?;
        let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("docker inspect: {e}"))?;
        Ok(parse_inspect(&v))
    }

    /// Returns the last `tail` log lines, optionally only those after unix time `since`.
    pub async fn logs(&self, id: &str, tail: usize, since: Option<u64>, tty: bool) -> Result<Vec<LogLine>, String> {
        let mut path = format!("/containers/{}/logs?stdout=1&stderr=1&timestamps=1&tail={tail}", safe_id(id)?);
        if let Some(s) = since {
            path.push_str(&format!("&since={s}"));
        }
        let body = self.get(&path).await?;
        Ok(if tty { split_plain(&body, "stdout") } else { demux(&body) })
    }
}

/// Container ids are hex; reject anything else before it reaches a URL.
fn safe_id(id: &str) -> Result<&str, String> {
    if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err("invalid container id".into())
    }
}

/// Maps inspect JSON to [`ContainerInfo`], tolerating missing fields.
fn parse_inspect(v: &serde_json::Value) -> ContainerInfo {
    let st = &v["State"];
    ContainerInfo {
        pid: st["Pid"].as_u64().unwrap_or(0) as u32,
        restarts: v["RestartCount"].as_u64().unwrap_or(0),
        health: st["Health"]["Status"].as_str().map(str::to_string),
        host_network: v["HostConfig"]["NetworkMode"].as_str() == Some("host"),
        tty: v["Config"]["Tty"].as_bool().unwrap_or(false),
        oom_killed: st["OOMKilled"].as_bool().unwrap_or(false),
    }
}

/// Splits Docker's multiplexed log stream (8-byte frame headers) into lines.
pub fn demux(buf: &[u8]) -> Vec<LogLine> {
    let mut out = Vec::new();
    let mut rest = buf;
    while rest.len() >= 8 {
        let stream = match rest[0] {
            2 => "stderr",
            _ => "stdout",
        };
        let len = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let end = (8 + len).min(rest.len());
        out.extend(split_plain(&rest[8..end], stream));
        rest = &rest[end..];
    }
    out
}

/// Splits raw log bytes into timestamped lines.
fn split_plain(buf: &[u8], stream: &str) -> Vec<LogLine> {
    String::from_utf8_lossy(buf)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (ts, text) = match l.split_once(' ').and_then(|(t, rest)| Some((parse_rfc3339(t)?, rest))) {
                Some((t, rest)) => (Some(t), rest),
                None => (None, l),
            };
            LogLine::new(ts, stream, text)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(stream: u8, s: &str) -> Vec<u8> {
        let mut v = vec![stream, 0, 0, 0];
        v.extend_from_slice(&(s.len() as u32).to_be_bytes());
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn demux_frames_and_timestamps() {
        let mut buf = frame(1, "2026-01-02T03:04:05.123456789Z hello\n");
        buf.extend(frame(2, "2026-01-02T03:04:06.0Z boom\nno-ts line\n"));
        buf.extend_from_slice(&[1, 0, 0]); // truncated tail is ignored
        let lines = demux(&buf);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].ts, Some(1_767_323_045));
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].stream, "stderr");
        assert_eq!(lines[2].ts, None);
    }

    #[test]
    fn oversized_frame_length_is_clamped() {
        let mut buf = vec![1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
        buf.extend_from_slice(b"short\n");
        assert_eq!(demux(&buf).len(), 1);
    }

    #[test]
    fn ids_are_validated() {
        assert!(safe_id("abc123").is_ok());
        assert!(safe_id("../../etc").is_err());
        assert!(safe_id("").is_err());
    }

    #[test]
    fn inspect_mapping_tolerates_gaps() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"State":{"Pid":42,"Health":{"Status":"healthy"}},"RestartCount":3,"HostConfig":{"NetworkMode":"host"}}"#,
        )
        .unwrap();
        let i = parse_inspect(&v);
        assert_eq!((i.pid, i.restarts, i.host_network), (42, 3, true));
        assert_eq!(i.health.as_deref(), Some("healthy"));
        assert_eq!(parse_inspect(&serde_json::json!({})).pid, 0);
    }

    #[tokio::test]
    async fn missing_socket_is_an_error_not_a_hang() {
        let d = Docker::new("/nonexistent/docker.sock");
        assert!(!d.present());
        assert!(d.list().await.is_err());
    }

    #[test]
    fn summary_helpers() {
        let c: ContainerSummary = serde_json::from_str(
            r#"{"Id":"0123456789abcdef","Names":["/web"],"Labels":{"com.docker.compose.project":"shop"},"Ports":[{"PrivatePort":80,"PublicPort":8080,"Type":"tcp"}]}"#,
        )
        .unwrap();
        assert_eq!(c.name(), "web");
        assert_eq!(c.short_id(), "0123456789ab");
        assert_eq!(c.project().as_deref(), Some("shop"));
        assert_eq!(c.ports[0].public_port, Some(8080));
    }
}
