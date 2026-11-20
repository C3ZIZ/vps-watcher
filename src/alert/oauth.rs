//! OAuth2 access tokens for SMTP (XOAUTH2), refreshed from a long-lived refresh token.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::settings::OAuthConfig;
use crate::net;
use crate::util::atomic_write;

const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Hands out a cached access token, refreshing it shortly before it expires.
pub struct TokenSource {
    cfg: OAuthConfig,
    /// Where a rotated refresh token is kept.
    store: Option<PathBuf>,
    /// False for one-off CLI runs: they read the saved token but never write files the service owns.
    persist: bool,
    refresh: Mutex<String>,
    cache: tokio::sync::Mutex<Option<(String, Instant)>>,
}

/// A refresh token the provider rotated, tied to the env value it replaced.
#[derive(Serialize, Deserialize)]
struct Stored {
    replaces: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

impl TokenSource {
    /// `store` is the data-dir file for rotated refresh tokens (Microsoft rotates them).
    pub fn new(cfg: OAuthConfig, store: Option<PathBuf>, persist: bool) -> Self {
        let env_token = cfg.refresh_token.expose().to_string();
        // A saved token only counts while the env still holds the token it replaced.
        let saved = store
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Stored>(&b).ok())
            .filter(|s| s.replaces == fingerprint(&env_token))
            .map(|s| s.refresh_token);
        TokenSource { cfg, store, persist, refresh: Mutex::new(saved.unwrap_or(env_token)), cache: Default::default() }
    }

    /// A valid access token, from cache when possible.
    pub async fn access_token(&self) -> Result<String, String> {
        let mut cache = self.cache.lock().await;
        if let Some((t, exp)) = cache.as_ref()
            && Instant::now() < *exp
        {
            return Ok(t.clone());
        }
        let (token, ttl, rotated) =
            tokio::time::timeout(TOKEN_TIMEOUT, self.fetch()).await.map_err(|_| "OAuth token request timed out")??;
        if let Some(new) = rotated {
            self.remember(new);
        }
        // Refresh a minute early so a token never expires mid-send.
        *cache = Some((token.clone(), Instant::now() + Duration::from_secs(ttl.saturating_sub(60).max(30))));
        Ok(token)
    }

    async fn fetch(&self) -> Result<(String, u64, Option<String>), String> {
        let refresh = self.refresh.lock().map(|r| r.clone()).unwrap_or_default();
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", self.cfg.client_id.as_str()),
        ];
        if let Some(s) = &self.cfg.client_secret {
            form.push(("client_secret", s.expose()));
        }
        let body = net::form_encode(&form).into_bytes();
        let headers = [("accept", "application/json")];
        let res = net::request(
            hyper::Method::POST,
            &self.cfg.token_url,
            &headers,
            Some(("application/x-www-form-urlencoded", body)),
            64 * 1024,
        )
        .await?;
        let reply: TokenReply = serde_json::from_slice(&res.body)
            .map_err(|_| format!("OAuth token endpoint returned HTTP {} without JSON", res.status))?;
        match (res.status, reply.access_token) {
            (200, Some(t)) if !t.is_empty() => {
                let rotated = reply.refresh_token.filter(|r| !r.is_empty() && *r != refresh);
                Ok((t, reply.expires_in.unwrap_or(3600), rotated))
            }
            (code, _) => {
                let why = reply.error_description.or(reply.error).unwrap_or_else(|| format!("HTTP {code}"));
                let why: String = why.chars().take(200).collect();
                Err(format!("OAuth token refresh failed: {why}"))
            }
        }
    }

    /// Keeps a rotated refresh token in memory and, for the service, on disk (mode 0600).
    fn remember(&self, new: String) {
        if let Ok(mut r) = self.refresh.lock() {
            *r = new.clone();
        }
        let Some(path) = self.store.as_ref().filter(|_| self.persist) else { return };
        let stored = Stored { replaces: fingerprint(self.cfg.refresh_token.expose()), refresh_token: new };
        if let Ok(json) = serde_json::to_vec(&stored)
            && let Err(e) = atomic_write(path, &json, true)
        {
            crate::warn!("could not save the rotated OAuth refresh token: {e}");
        }
    }
}

/// SHA-256 of a token, so the state file never holds the env value itself.
fn fingerprint(s: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, s.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::settings::Secret;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves canned token replies on loopback, returning each request it saw.
    async fn token_server(replies: Vec<&'static str>) -> (u16, tokio::task::JoinHandle<Vec<String>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let mut seen = Vec::new();
            for body in replies {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let code = if body.contains("error") { "400 Bad Request" } else { "200 OK" };
                let res = format!("HTTP/1.1 {code}\r\ncontent-length: {}\r\n\r\n{body}", body.len());
                s.write_all(res.as_bytes()).await.unwrap();
            }
            seen
        });
        (port, h)
    }

    fn cfg(port: u16) -> OAuthConfig {
        OAuthConfig {
            user: "me@x.io".into(),
            client_id: "cid".into(),
            client_secret: Some(Secret::new("csecret")),
            refresh_token: Secret::new("rt-env"),
            token_url: net::parse_url(&format!("http://127.0.0.1:{port}/token")).unwrap(),
        }
    }

    #[tokio::test]
    async fn refreshes_caches_and_keeps_rotated_token() {
        let (port, server) =
            token_server(vec![r#"{"access_token":"at1","expires_in":3600,"refresh_token":"rt-new"}"#]).await;
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("smtp-oauth.json");
        let ts = TokenSource::new(cfg(port), Some(store.clone()), true);
        assert_eq!(ts.access_token().await.unwrap(), "at1");
        assert_eq!(ts.access_token().await.unwrap(), "at1", "second call comes from cache");
        let reqs = server.await.unwrap();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].contains("grant_type=refresh_token&refresh_token=rt-env&client_id=cid&client_secret=csecret"));

        let saved = std::fs::read_to_string(&store).unwrap();
        assert!(saved.contains("rt-new") && !saved.contains("rt-env"), "env token is stored only as a hash");
        let again = TokenSource::new(cfg(port), Some(store.clone()), true);
        assert_eq!(*again.refresh.lock().unwrap(), "rt-new", "the rotated token is used after a restart");
        let mut changed = cfg(port);
        changed.refresh_token = Secret::new("rt-pasted-later");
        assert_eq!(*TokenSource::new(changed, Some(store), true).refresh.lock().unwrap(), "rt-pasted-later");
    }

    #[tokio::test]
    async fn provider_error_is_readable() {
        let (port, _s) =
            token_server(vec![r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#])
                .await;
        let e = TokenSource::new(cfg(port), None, true).access_token().await.unwrap_err();
        assert_eq!(e, "OAuth token refresh failed: Token has been expired or revoked.");
    }
}
