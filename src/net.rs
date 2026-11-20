//! Outbound networking shared by monitors and alerts: URL parsing, TLS and a tiny HTTP/1 client.

use std::sync::{Arc, OnceLock};

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, header};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{self, pki_types::ServerName};

/// Parts of an http(s) URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    /// Host without IPv6 brackets, as used for DNS and TLS.
    pub fn bare_host(&self) -> &str {
        self.host.trim_start_matches('[').trim_end_matches(']')
    }

    /// True for localhost and loopback IPs, where plain HTTP cannot be sniffed.
    pub fn is_loopback(&self) -> bool {
        is_loopback_host(self.bare_host())
    }
}

/// True for `localhost` and loopback IP literals.
pub fn is_loopback_host(h: &str) -> bool {
    h.eq_ignore_ascii_case("localhost") || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Host names and IP literals only; no schemes, paths or spaces.
pub fn is_valid_host(h: &str) -> bool {
    let h = h.trim_start_matches('[').trim_end_matches(']');
    !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:_".contains(&b))
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

/// Wraps a connected socket in verified TLS for `host`.
pub async fn tls_connect(host: &str, tcp: TcpStream) -> Result<TlsStream<TcpStream>, String> {
    let name = ServerName::try_from(host.to_string()).map_err(|_| "invalid TLS server name".to_string())?;
    TlsConnector::from(tls_config()).connect(name, tcp).await.map_err(|e| format!("TLS: {e}"))
}

/// An HTTP response; `body` holds at most the requested number of bytes.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// The `Location` header, for following redirects.
    pub location: Option<String>,
}

/// Sends one request and reads up to `max_body` bytes of the reply (0 skips the body).
/// Callers wrap this in a timeout. Errors never include the URL, which may hold a secret.
pub async fn request(
    method: Method,
    url: &Url,
    headers: &[(&str, &str)],
    body: Option<(&str, Vec<u8>)>,
    max_body: usize,
) -> Result<Response, String> {
    // Errors leave out the host: monitor details are public in open mode.
    let tcp = TcpStream::connect((url.bare_host(), url.port)).await.map_err(|e| e.to_string())?;
    if url.https {
        let tls = tls_connect(url.bare_host(), tcp).await?;
        send(tls, method, url, headers, body, max_body).await
    } else {
        send(tcp, method, url, headers, body, max_body).await
    }
}

async fn send<S>(
    io: S,
    method: Method,
    url: &Url,
    headers: &[(&str, &str)],
    body: Option<(&str, Vec<u8>)>,
    max_body: usize,
) -> Result<Response, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let default_port = if url.https { 443 } else { 80 };
    let host = if url.port == default_port { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
    let mut req = Request::builder()
        .method(method)
        .uri(&url.path)
        .header(header::HOST, host)
        .header(header::USER_AGENT, format!("vps-watcher/{}", crate::VERSION))
        .header(header::CONNECTION, "close");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let bytes = match body {
        Some((ctype, data)) => {
            req = req.header(header::CONTENT_TYPE, ctype);
            Bytes::from(data)
        }
        None => Bytes::new(),
    };
    let req = req.body(Full::new(bytes)).map_err(|e| e.to_string())?;
    let res = tx.send_request(req).await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).map(String::from);
    if max_body == 0 {
        return Ok(Response { status, body: Vec::new(), location });
    }
    // A reply larger than the limit is cut off rather than treated as a failure.
    let mut body = res.into_body();
    let mut out = Vec::new();
    while out.len() < max_body {
        let Some(Ok(frame)) = body.frame().await else { break };
        if let Ok(data) = frame.into_data() {
            out.extend_from_slice(&data[..data.len().min(max_body - out.len())]);
        }
    }
    Ok(Response { status, body: out, location })
}

/// Percent-encodes a value for `application/x-www-form-urlencoded`.
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
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
        assert!(u.is_loopback());
        let u = parse_url("http://[::1]:9000/x").unwrap();
        assert_eq!((u.host.as_str(), u.bare_host(), u.port), ("[::1]", "::1", 9000));
        assert!(u.is_loopback());
        assert!(!parse_url("https://hooks.slack.com/x").unwrap().is_loopback());
        assert_eq!(parse_url("http://h?q").unwrap().path, "/?q");
        for bad in ["ftp://x", "http://", "http://a b", "http://u:p@h/", "http://h:0", "http://h:99999", "example.com"]
        {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn form_encoding() {
        assert_eq!(form_encode(&[("a", "x y&z"), ("t", "1/2+=")]), "a=x%20y%26z&t=1%2F2%2B%3D");
    }

    #[tokio::test]
    async fn posts_and_reads_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            s.write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 5\r\n\r\nhello").await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        let url = parse_url(&format!("http://127.0.0.1:{port}/hook")).unwrap();
        let r = request(Method::POST, &url, &[("x-test", "1")], Some(("application/json", b"{}".to_vec())), 3)
            .await
            .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (201, &b"hel"[..]), "body is capped at max_body");
        let req = server.await.unwrap();
        assert!(req.starts_with("POST /hook HTTP/1.1"));
        assert!(req.contains("x-test: 1") && req.contains("content-type: application/json") && req.ends_with("{}"));
    }
}
