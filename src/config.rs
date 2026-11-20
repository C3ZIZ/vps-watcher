//! Runtime configuration, read from environment variables and an optional env file.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default env file the installer creates.
pub const DEFAULT_ENV_FILE: &str = "/etc/vps-watcher/vps-watcher.env";

/// Who may view the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Every page needs the admin to be signed in.
    Login,
    /// Anyone can view; only the admin can change things.
    Open,
}

/// Fully validated settings for one run.
#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub interval: Duration,
    pub auth: AuthMode,
    pub open_logs: bool,
    pub domain: Option<String>,
    pub trusted_proxies: Vec<IpAddr>,
    pub docker_socket: PathBuf,
}

impl Config {
    /// Loads settings from the process env, falling back to values in `env_file`.
    pub fn load(env_file: Option<&Path>) -> Result<Self, String> {
        let file_vars = read_env_file(env_file)?;
        Self::from_lookup(|key| std::env::var(key).ok().or_else(|| file_vars.get(key).cloned()))
    }

    /// Builds a config from any key lookup, so tests need no real environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let val = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());

        let listen = val("VPSW_LISTEN").unwrap_or_else(|| "127.0.0.1:9100".into());
        let listen: SocketAddr =
            listen.parse().map_err(|_| format!("VPSW_LISTEN '{listen}' is not an address like 127.0.0.1:9100"))?;

        let interval = match val("VPSW_INTERVAL") {
            None => 5,
            Some(v) => match v.parse::<u64>() {
                Ok(n) if (1..=60).contains(&n) => n,
                _ => return Err(format!("VPSW_INTERVAL '{v}' must be a whole number from 1 to 60")),
            },
        };

        let auth = match val("VPSW_AUTH").as_deref().map(str::to_ascii_lowercase).as_deref() {
            None | Some("login") => AuthMode::Login,
            Some("open") => AuthMode::Open,
            Some(other) => return Err(format!("VPSW_AUTH '{other}' must be 'login' or 'open'")),
        };

        let open_logs = parse_bool("VPSW_OPEN_LOGS", val("VPSW_OPEN_LOGS"))?;

        let trusted_proxies = val("VPSW_TRUSTED_PROXIES")
            .unwrap_or_else(|| "127.0.0.1,::1".into())
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().map_err(|_| format!("VPSW_TRUSTED_PROXIES has a bad IP: '{s}'")))
            .collect::<Result<Vec<IpAddr>, String>>()?;

        let domain = val("VPSW_DOMAIN").map(|d| d.trim_end_matches('/').to_string());
        if let Some(d) = &domain
            && (d.contains("://") || d.contains(' ') || d.contains('/'))
        {
            return Err(format!("VPSW_DOMAIN '{d}' should be a bare host like watch.example.com"));
        }

        Ok(Config {
            listen,
            data_dir: val("VPSW_DATA_DIR").unwrap_or_else(|| "/var/lib/vps-watcher".into()).into(),
            interval: Duration::from_secs(interval),
            auth,
            open_logs,
            domain,
            trusted_proxies,
            docker_socket: val("VPSW_DOCKER_SOCKET").unwrap_or_else(|| "/var/run/docker.sock".into()).into(),
        })
    }
}

/// Accepts the usual spellings of yes and no.
fn parse_bool(key: &str, v: Option<String>) -> Result<bool, String> {
    match v.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None | Some("false" | "0" | "no" | "off") => Ok(false),
        Some("true" | "1" | "yes" | "on") => Ok(true),
        Some(other) => Err(format!("{key} '{other}' must be true or false")),
    }
}

/// Reads an env file if given (or the default one if it exists and is readable).
fn read_env_file(path: Option<&Path>) -> Result<HashMap<String, String>, String> {
    let (path, required) = match path {
        Some(p) => (p.to_path_buf(), true),
        None => (PathBuf::from(DEFAULT_ENV_FILE), false),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse_env_file(&text).into_iter().collect()),
        Err(_) if !required => Ok(HashMap::new()),
        Err(e) => Err(format!("cannot read env file {}: {e}", path.display())),
    }
}

/// Parses `KEY=value` lines; skips blanks and `#` comments and strips matching quotes.
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = ['"', '\''].iter().find_map(|q| v.strip_prefix(*q).and_then(|s| s.strip_suffix(*q))).unwrap_or(v);
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults_are_safe() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.listen.to_string(), "127.0.0.1:9100");
        assert_eq!(c.auth, AuthMode::Login);
        assert!(!c.open_logs);
        assert_eq!(c.interval, Duration::from_secs(5));
    }

    #[test]
    fn rejects_bad_values() {
        assert!(cfg(&[("VPSW_INTERVAL", "0")]).is_err());
        assert!(cfg(&[("VPSW_INTERVAL", "abc")]).is_err());
        assert!(cfg(&[("VPSW_AUTH", "none")]).is_err());
        assert!(cfg(&[("VPSW_LISTEN", "9100")]).is_err());
        assert!(cfg(&[("VPSW_DOMAIN", "https://x.com")]).is_err());
        assert!(cfg(&[("VPSW_TRUSTED_PROXIES", "1.2.3")]).is_err());
    }

    #[test]
    fn empty_values_mean_default() {
        let c = cfg(&[("VPSW_AUTH", ""), ("VPSW_INTERVAL", " ")]).unwrap();
        assert_eq!(c.auth, AuthMode::Login);
    }

    #[test]
    fn env_file_parsing() {
        let vars = parse_env_file("# c\n\nVPSW_AUTH=open\nexport VPSW_DOMAIN=\"a.b\"\nX='y z'\nbad\n");
        assert_eq!(
            vars,
            vec![("VPSW_AUTH".into(), "open".into()), ("VPSW_DOMAIN".into(), "a.b".into()), ("X".into(), "y z".into())]
        );
    }
}
