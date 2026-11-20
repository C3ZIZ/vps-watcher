//! Runtime configuration, read from environment variables and an optional env file.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::alert::AlertConfig;

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
    pub alerts: AlertConfig,
    /// Check once a day whether a newer release exists.
    pub update_check: bool,
    /// Let the daily timer install signed releases by itself.
    pub auto_update: bool,
    /// Another release server (a mirror, or tests); signatures are still required.
    pub update_url: Option<String>,
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

        let alerts = AlertConfig::from_vals(&val)?;
        let update_check = val("VPSW_UPDATE_CHECK").map_or(Ok(true), |v| parse_bool("VPSW_UPDATE_CHECK", Some(v)))?;
        let auto_update = parse_bool("VPSW_AUTO_UPDATE", val("VPSW_AUTO_UPDATE"))?;
        let update_url = val("VPSW_UPDATE_URL").map(|u| u.trim_end_matches('/').to_string());
        if let Some(u) = &update_url {
            let p = crate::net::parse_url(u).map_err(|_| format!("VPSW_UPDATE_URL '{u}' is not a valid URL"))?;
            if !p.https && !p.is_loopback() {
                return Err("VPSW_UPDATE_URL must use https".into());
            }
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
            alerts,
            update_check,
            auto_update,
            update_url,
        })
    }

    /// Effective settings for `vps-watcher config`. Secrets appear only as "set (hidden)".
    pub fn summary(&self) -> Vec<(&'static str, String)> {
        let mut v = vec![
            ("Listen", self.listen.to_string()),
            ("Access", if self.auth == AuthMode::Open { "open (public view)".into() } else { "login".into() }),
            ("Open logs", self.open_logs.to_string()),
            ("Domain", self.domain.clone().unwrap_or_else(|| "(none)".into())),
            ("Interval", format!("{} s", self.interval.as_secs())),
            ("Data dir", self.data_dir.display().to_string()),
            ("Docker socket", self.docker_socket.display().to_string()),
            ("Alert rules", crate::alert::rules_summary(&self.alerts)),
            (
                "Updates",
                format!(
                    "check {}, automatic install {}{}",
                    if self.update_check { "daily" } else { "off" },
                    if self.auto_update { "on (signed releases only)" } else { "off" },
                    self.update_url.as_ref().map_or(String::new(), |u| format!(", from {u}"))
                ),
            ),
        ];
        v.extend(crate::alert::channels_summary(&self.alerts));
        v
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

/// Parses an env file the way systemd's `EnvironmentFile=` does, so the CLI and the service agree:
/// `#`/`;` comments, single quotes taken literally, backslash escapes outside them, `\` line
/// continuation, and adjacent quoted parts joined. A leading `export ` is also accepted.
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    #[derive(PartialEq)]
    enum St {
        PreKey,
        Key,
        PreValue,
        Value,
        ValueEscape,
        Single,
        Double,
        DoubleEscape,
        Comment,
        CommentEscape,
    }
    let mut out = Vec::new();
    let (mut st, mut key, mut val) = (St::PreKey, String::new(), String::new());
    // Byte length of `val` before unescaped trailing whitespace, which is trimmed.
    let mut keep = 0usize;
    let emit = |key: &mut String, val: &mut String, keep: usize, out: &mut Vec<(String, String)>| {
        val.truncate(keep.min(val.len()));
        let k = key.trim();
        let k = k.strip_prefix("export ").map_or(k, str::trim);
        if !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            out.push((k.to_string(), std::mem::take(val)));
        }
        key.clear();
        val.clear();
    };
    for c in text.chars() {
        let nl = c == '\n' || c == '\r';
        match st {
            St::PreKey => {
                if c == '#' || c == ';' {
                    st = St::Comment;
                } else if !c.is_whitespace() {
                    st = St::Key;
                    key.push(c);
                }
            }
            St::Key => {
                if nl {
                    st = St::PreKey;
                    key.clear();
                } else if c == '=' {
                    st = St::PreValue;
                    keep = 0;
                } else {
                    key.push(c);
                }
            }
            St::PreValue => {
                if nl {
                    st = St::PreKey;
                    emit(&mut key, &mut val, keep, &mut out);
                } else if c == '\'' {
                    st = St::Single;
                } else if c == '"' {
                    st = St::Double;
                } else if c == '\\' {
                    st = St::ValueEscape;
                } else if !c.is_whitespace() {
                    st = St::Value;
                    val.push(c);
                    keep = val.len();
                }
            }
            St::Value => {
                if nl {
                    st = St::PreKey;
                    emit(&mut key, &mut val, keep, &mut out);
                } else if c == '\\' {
                    st = St::ValueEscape;
                } else {
                    val.push(c);
                    if !c.is_whitespace() {
                        keep = val.len();
                    }
                }
            }
            St::ValueEscape => {
                st = St::Value;
                if !nl {
                    val.push(c);
                    keep = val.len();
                }
            }
            St::Single => {
                if c == '\'' {
                    st = St::PreValue;
                } else {
                    val.push(c);
                    keep = val.len();
                }
            }
            St::Double => {
                if c == '"' {
                    st = St::PreValue;
                } else if c == '\\' {
                    st = St::DoubleEscape;
                } else {
                    val.push(c);
                    keep = val.len();
                }
            }
            St::DoubleEscape => {
                st = St::Double;
                if "\"\\`$".contains(c) {
                    val.push(c);
                } else if !nl {
                    val.push('\\');
                    val.push(c);
                }
                keep = val.len();
            }
            St::Comment => {
                if c == '\\' {
                    st = St::CommentEscape;
                } else if nl {
                    st = St::PreKey;
                }
            }
            St::CommentEscape => st = St::Comment,
        }
    }
    if matches!(st, St::PreValue | St::Value | St::ValueEscape | St::Single | St::Double | St::DoubleEscape) {
        emit(&mut key, &mut val, keep, &mut out);
    }
    out
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
    fn update_settings() {
        let c = cfg(&[]).unwrap();
        assert!(c.update_check && !c.auto_update && c.update_url.is_none());
        let c = cfg(&[("VPSW_UPDATE_CHECK", "off"), ("VPSW_AUTO_UPDATE", "yes")]).unwrap();
        assert!(!c.update_check && c.auto_update);
        assert!(cfg(&[("VPSW_UPDATE_URL", "http://mirror.example.com")]).is_err());
        assert!(cfg(&[("VPSW_UPDATE_URL", "http://127.0.0.1:8098/")]).unwrap().update_url.unwrap().ends_with("8098"));
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

    #[test]
    fn env_file_follows_systemd_rules() {
        let get = |line: &str| parse_env_file(line).first().map(|(_, v)| v.clone());
        assert_eq!(get(r"P=pa\ss").as_deref(), Some("pass"), "unquoted backslash escapes");
        assert_eq!(get(r"P='pa\ss #1'").as_deref(), Some(r"pa\ss #1"), "single quotes are literal");
        assert_eq!(get(r#"P="a\"b\\c\$d\x""#).as_deref(), Some(r#"a"b\c$d\x"#));
        assert_eq!(get("P='ab'cd").as_deref(), Some("abcd"), "adjacent parts join");
        assert_eq!(get("P=  a b  ").as_deref(), Some("a b"));
        assert_eq!(get("P=1 \\\n2").as_deref(), Some("1 2"), "line continuation");
        assert_eq!(get("P=").as_deref(), Some(""));
        assert_eq!(parse_env_file("; c\n# c \\\nstill comment\nA=1").len(), 1);
        assert!(parse_env_file("BAD KEY=1\n").is_empty());
    }
}
