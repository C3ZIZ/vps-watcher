//! Single admin account: argon2id password, in-memory sessions and login rate limiting.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::util::{atomic_write, now, random_hex};

/// Sessions end after this long without a request.
pub const SESSION_IDLE: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_SESSIONS: usize = 50;
const MAX_FAILS: u32 = 10;
const FAIL_WINDOW: Duration = Duration::from_secs(15 * 60);
pub const MIN_PASSWORD: usize = 12;

/// The stored admin (auth.json, mode 0600).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Admin {
    email: String,
    hash: String,
    updated: u64,
}

struct Session {
    email: String,
    last_seen: Instant,
}

/// Why a login was refused.
#[derive(Debug, PartialEq)]
pub enum LoginError {
    /// Too many failures from this IP; retry after this many seconds.
    Limited(u64),
    /// Too many password checks running at once; retry shortly.
    Busy,
    Invalid,
}

/// Account, sessions and limiter.
pub struct Auth {
    path: PathBuf,
    admin: Mutex<Option<Admin>>,
    sessions: Mutex<HashMap<String, Session>>,
    fails: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    /// At most two argon2 checks run at once, so login floods cannot pile up memory.
    slots: tokio::sync::Semaphore,
}

/// Longest password we will hash; longer input is rejected before any work.
const MAX_PASSWORD_BYTES: usize = 1024;

/// Locks a mutex, recovering from poisoning.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Auth {
    /// Loads the admin from `dir/auth.json` if it exists.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("auth.json");
        let admin = match std::fs::read(&path) {
            Ok(b) => Some(serde_json::from_slice(&b).map_err(|e| format!("{} is invalid: {e}", path.display()))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(Auth {
            path,
            admin: Mutex::new(admin),
            sessions: Mutex::default(),
            fails: Mutex::default(),
            slots: tokio::sync::Semaphore::new(2),
        })
    }

    /// True until the first admin is created.
    pub fn needs_setup(&self) -> bool {
        lock(&self.admin).is_none()
    }

    /// Creates the first admin; fails if one already exists.
    pub async fn create_admin(&self, email: &str, password: &str) -> Result<(), String> {
        let email = normalize_email(email)?;
        check_password(password)?;
        if !self.needs_setup() {
            return Err("An admin already exists.".into());
        }
        let hash = hash_password(password.to_string()).await?;
        let mut admin = lock(&self.admin);
        // Re-check after the slow hash so two racing setups cannot both win.
        if admin.is_some() {
            return Err("An admin already exists.".into());
        }
        let a = Admin { email, hash, updated: now() };
        self.persist(&a)?;
        *admin = Some(a);
        Ok(())
    }

    /// Verifies credentials and returns a new session token.
    pub async fn login(&self, ip: IpAddr, email: &str, password: &str) -> Result<String, LoginError> {
        let ip = limit_key(ip);
        if let Some(wait) = self.limited(ip) {
            return Err(LoginError::Limited(wait));
        }
        let Ok(_slot) = self.slots.try_acquire() else { return Err(LoginError::Busy) };
        // Count the attempt before the slow hash, so parallel requests cannot skip the limit.
        self.note_failure(ip);
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(LoginError::Invalid);
        }
        let stored = lock(&self.admin).clone();
        let email = email.trim().to_lowercase();
        // Hash even for unknown emails so timing does not reveal the admin address.
        let (hash, email_ok) = match &stored {
            Some(a) => (a.hash.clone(), a.email == email),
            None => (dummy_hash().to_string(), false),
        };
        let pw_ok = verify_password(password.to_string(), hash).await;
        if !(pw_ok && email_ok) {
            return Err(LoginError::Invalid);
        }
        lock(&self.fails).remove(&ip);
        Ok(self.new_session(email))
    }

    /// Email of the signed-in admin for `token`, refreshing its idle timer.
    pub fn session(&self, token: &str) -> Option<String> {
        let mut s = lock(&self.sessions);
        let sess = s.get_mut(token)?;
        if sess.last_seen.elapsed() > SESSION_IDLE {
            s.remove(token);
            return None;
        }
        sess.last_seen = Instant::now();
        Some(sess.email.clone())
    }

    /// Ends one session.
    pub fn logout(&self, token: &str) {
        lock(&self.sessions).remove(token);
    }

    /// Changes email and/or password after checking the current password; signs out other sessions.
    pub async fn change(
        &self,
        token: &str,
        current: &str,
        new_email: Option<&str>,
        new_pw: Option<&str>,
    ) -> Result<(), String> {
        let stored = lock(&self.admin).clone().ok_or("No admin exists.")?;
        if current.len() > MAX_PASSWORD_BYTES {
            return Err("Current password is wrong.".into());
        }
        if !verify_password(current.to_string(), stored.hash.clone()).await {
            return Err("Current password is wrong.".into());
        }
        let email = match new_email.filter(|e| !e.trim().is_empty()) {
            Some(e) => normalize_email(e)?,
            None => stored.email.clone(),
        };
        let hash = match new_pw.filter(|p| !p.is_empty()) {
            Some(p) => {
                check_password(p)?;
                hash_password(p.to_string()).await?
            }
            None => stored.hash.clone(),
        };
        let a = Admin { email: email.clone(), hash, updated: now() };
        self.persist(&a)?;
        *lock(&self.admin) = Some(a);
        let mut s = lock(&self.sessions);
        s.retain(|t, _| t == token);
        if let Some(cur) = s.get_mut(token) {
            cur.email = email;
        }
        Ok(())
    }

    /// Ends every session except `keep`.
    pub fn logout_others(&self, keep: &str) {
        lock(&self.sessions).retain(|t, _| t == keep);
    }

    /// Deletes the admin file so the setup page appears again.
    pub fn reset(dir: &Path) -> Result<bool, String> {
        match std::fs::remove_file(dir.join("auth.json")) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    fn persist(&self, a: &Admin) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(a).map_err(|e| e.to_string())?;
        atomic_write(&self.path, &json, true).map_err(|e| format!("cannot save admin: {e}"))
    }

    fn new_session(&self, email: String) -> String {
        let token = random_hex(32);
        let mut s = lock(&self.sessions);
        s.retain(|_, v| v.last_seen.elapsed() <= SESSION_IDLE);
        if s.len() >= MAX_SESSIONS
            && let Some(oldest) = s.iter().min_by_key(|(_, v)| v.last_seen).map(|(k, _)| k.clone())
        {
            s.remove(&oldest);
        }
        s.insert(token.clone(), Session { email, last_seen: Instant::now() });
        token
    }

    /// Seconds to wait if `ip` has used up its attempts.
    fn limited(&self, ip: IpAddr) -> Option<u64> {
        let f = lock(&self.fails);
        let (n, since) = f.get(&ip)?;
        let left = FAIL_WINDOW.checked_sub(since.elapsed())?;
        (*n >= MAX_FAILS).then(|| left.as_secs().max(1))
    }

    fn note_failure(&self, ip: IpAddr) {
        let mut f = lock(&self.fails);
        if f.len() > 10_000 {
            f.retain(|_, (_, t)| t.elapsed() < FAIL_WINDOW);
        }
        let e = f.entry(ip).or_insert((0, Instant::now()));
        if e.1.elapsed() >= FAIL_WINDOW {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }
}

/// Lowercases and checks an email address.
fn normalize_email(e: &str) -> Result<String, String> {
    let e = e.trim().to_lowercase();
    let bad = || "Enter a valid email address.".to_string();
    let (local, domain) = e.split_once('@').ok_or_else(bad)?;
    if e.len() > 254
        || local.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains('@')
        || e.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad());
    }
    Ok(e)
}

/// Enforces the password length rules.
fn check_password(p: &str) -> Result<(), String> {
    let n = p.chars().count();
    if n < MIN_PASSWORD {
        return Err(format!("Password must be at least {MIN_PASSWORD} characters."));
    }
    if n > 256 {
        return Err("Password must be at most 256 characters.".into());
    }
    Ok(())
}

/// argon2id hash on a blocking thread (it takes ~50 ms and 19 MiB).
async fn hash_password(pw: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let out = Argon2::default().hash_password(pw.as_bytes()).map(|h| h.to_string()).map_err(|e| e.to_string());
        release_memory();
        out
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Checks `pw` against a PHC hash string on a blocking thread.
async fn verify_password(pw: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let ok = Argon2::default().verify_password(pw.as_bytes(), hash.as_str()).is_ok();
        release_memory();
        ok
    })
    .await
    .unwrap_or(false)
}

/// Hands argon2's 19 MiB work buffer back to the OS; glibc would otherwise keep it cached.
fn release_memory() {
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim only releases free heap memory; it has no preconditions.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// A fixed hash used to keep failed-login timing constant.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| {
        Argon2::default().hash_password(random_hex(16).as_bytes()).map(|h| h.to_string()).unwrap_or_default()
    })
}

/// Headers that mean a request came through a proxy.
const PROXY_HEADERS: [&str; 4] = ["x-forwarded-for", "forwarded", "x-real-ip", "x-forwarded-proto"];

/// True only for requests made straight to the loopback port (no proxy in between),
/// addressed to a loopback name so DNS rebinding through an SSH tunnel cannot pass.
pub fn is_direct_loopback(peer: SocketAddr, headers: &HeaderMap) -> bool {
    let host = headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    peer.ip().to_canonical().is_loopback()
        && !PROXY_HEADERS.iter().any(|h| headers.contains_key(*h))
        && is_loopback_host(host)
}

/// `localhost`, `127.x.x.x` or `[::1]`, with or without a port.
fn is_loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Rate-limit key: the address itself, or its /64 network for IPv6 (one host usually owns a whole /64).
fn limit_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mut seg = v6.segments();
            seg[4..].fill(0);
            IpAddr::V6(seg.into())
        }
        v4 => v4,
    }
}

/// Real client IP: the right-most untrusted X-Forwarded-For entry when the peer is a trusted proxy.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpAddr]) -> IpAddr {
    let peer_ip = peer.ip().to_canonical();
    if !trusted.contains(&peer_ip) {
        return peer_ip;
    }
    let xff = headers.get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(",");
    xff.rsplit(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
        .find(|ip| !trusted.contains(ip))
        .or_else(|| headers.get("x-real-ip")?.to_str().ok()?.trim().parse().ok())
        .unwrap_or(peer_ip)
}

/// True when a trusted proxy says the browser used HTTPS.
pub fn is_https(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpAddr]) -> bool {
    trusted.contains(&peer.ip().to_canonical())
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn setup_login_change_flow() {
        let d = tempfile::tempdir().unwrap();
        let a = Auth::load(d.path()).unwrap();
        assert!(a.needs_setup());
        assert!(a.create_admin("bad", "longenoughpass").await.is_err());
        assert!(a.create_admin("me@x.io", "short").await.is_err());
        a.create_admin(" Me@X.io ", "correct horse battery").await.unwrap();
        assert!(!a.needs_setup());
        assert!(a.create_admin("other@x.io", "correct horse battery").await.is_err(), "only once");

        let reloaded = Auth::load(d.path()).unwrap();
        assert!(!reloaded.needs_setup(), "admin persists");

        let me = ip("1.2.3.4");
        assert_eq!(a.login(me, "me@x.io", "wrong password!!").await, Err(LoginError::Invalid));
        let t = a.login(me, "ME@x.io", "correct horse battery").await.unwrap();
        assert_eq!(a.session(&t).as_deref(), Some("me@x.io"));
        let t2 = a.login(me, "me@x.io", "correct horse battery").await.unwrap();

        assert!(a.change(&t, "wrong", None, Some("new password 123")).await.is_err());
        a.change(&t, "correct horse battery", Some("new@x.io"), Some("new password 123")).await.unwrap();
        assert!(a.session(&t2).is_none(), "other sessions are revoked");
        assert_eq!(a.session(&t).as_deref(), Some("new@x.io"));
        assert!(a.login(me, "new@x.io", "new password 123").await.is_ok());

        a.logout(&t);
        assert!(a.session(&t).is_none());
        assert!(Auth::reset(d.path()).unwrap());
        assert!(Auth::load(d.path()).unwrap().needs_setup());
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_ten_failures() {
        let d = tempfile::tempdir().unwrap();
        let a = Auth::load(d.path()).unwrap();
        a.create_admin("me@x.io", "correct horse battery").await.unwrap();
        let bad = ip("5.6.7.8");
        for _ in 0..MAX_FAILS {
            assert_eq!(a.login(bad, "me@x.io", "nope nope nope").await, Err(LoginError::Invalid));
        }
        assert!(matches!(a.login(bad, "me@x.io", "correct horse battery").await, Err(LoginError::Limited(_))));
        assert!(a.login(ip("9.9.9.9"), "me@x.io", "correct horse battery").await.is_ok(), "other IPs unaffected");
    }

    #[test]
    fn loopback_detection_rejects_proxied_requests() {
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5000".parse().unwrap();
        let remote: SocketAddr = "8.8.8.8:5000".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:9100".parse().unwrap());
        assert!(is_direct_loopback(local, &h));
        assert!(is_direct_loopback(mapped, &h));
        assert!(!is_direct_loopback(remote, &h));
        for ok in ["localhost:9100", "[::1]:9100", "LOCALHOST"] {
            h.insert("host", ok.parse().unwrap());
            assert!(is_direct_loopback(local, &h), "{ok}");
        }
        h.insert("host", "evil.com:9100".parse().unwrap());
        assert!(!is_direct_loopback(local, &h), "DNS rebinding through a tunnel");
        h.insert("host", "localhost:9100".parse().unwrap());
        h.insert("x-forwarded-for", "8.8.8.8".parse().unwrap());
        assert!(!is_direct_loopback(local, &h), "came through a proxy");
    }

    #[tokio::test]
    async fn parallel_attempts_cannot_skip_the_limit() {
        let d = tempfile::tempdir().unwrap();
        let a = std::sync::Arc::new(Auth::load(d.path()).unwrap());
        a.create_admin("me@x.io", "correct horse battery").await.unwrap();
        let bad = ip("5.6.7.9");
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..40 {
            let a = a.clone();
            set.spawn(async move { a.login(bad, "me@x.io", "nope nope nope").await });
        }
        let mut checked = 0;
        while let Some(r) = set.join_next().await {
            if r.unwrap() == Err(LoginError::Invalid) {
                checked += 1;
            }
        }
        assert!(checked <= MAX_FAILS as usize, "{checked} passwords were checked");
    }

    #[test]
    fn ipv6_limited_per_64() {
        assert_eq!(limit_key(ip("2001:db8:1:2:aaaa::1")), limit_key(ip("2001:db8:1:2:bbbb::9")));
        assert_ne!(limit_key(ip("2001:db8:1:2::1")), limit_key(ip("2001:db8:1:3::1")));
        assert_eq!(limit_key(ip("1.2.3.4")), ip("1.2.3.4"));
    }

    #[test]
    fn client_ip_trusts_only_known_proxies() {
        let trusted = vec![ip("127.0.0.1")];
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 1.1.1.1".parse().unwrap());
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        let via_proxy: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let direct: SocketAddr = "2.2.2.2:1".parse().unwrap();
        assert_eq!(client_ip(via_proxy, &h, &trusted), ip("1.1.1.1"), "right-most untrusted hop");
        assert_eq!(client_ip(direct, &h, &trusted), ip("2.2.2.2"), "spoofed header ignored");
        assert!(is_https(via_proxy, &h, &trusted));
        assert!(!is_https(direct, &h, &trusted));
    }
}
