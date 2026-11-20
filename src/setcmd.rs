//! `vps-watcher set KEY [VALUE]`: edits one setting in the env file. Secrets are read from a
//! hidden prompt or stdin, never from arguments, so they stay out of shell history and AI transcripts.

use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::config::{Config, parse_env_file};

/// Every setting vps-watcher reads.
pub const KEYS: [&str; 31] = [
    "VPSW_LISTEN",
    "VPSW_AUTH",
    "VPSW_OPEN_LOGS",
    "VPSW_DOMAIN",
    "VPSW_INTERVAL",
    "VPSW_TRUSTED_PROXIES",
    "VPSW_DATA_DIR",
    "VPSW_DOCKER_SOCKET",
    "VPSW_ALERT_CPU",
    "VPSW_ALERT_MEMORY",
    "VPSW_ALERT_AFTER",
    "VPSW_ALERT_SPIKE",
    "VPSW_ALERT_LOAD",
    "VPSW_ALERT_DISK",
    "VPSW_ALERT_APPS",
    "VPSW_ALERT_EMAIL",
    "VPSW_REPORT",
    "VPSW_SMTP_HOST",
    "VPSW_SMTP_PORT",
    "VPSW_SMTP_SECURITY",
    "VPSW_SMTP_USER",
    "VPSW_SMTP_PASSWORD",
    "VPSW_SMTP_FROM",
    "VPSW_SMTP_OAUTH_CLIENT_ID",
    "VPSW_SMTP_OAUTH_CLIENT_SECRET",
    "VPSW_SMTP_OAUTH_REFRESH_TOKEN",
    "VPSW_SMTP_OAUTH_TOKEN_URL",
    "VPSW_WEBHOOK_URL",
    "VPSW_WEBHOOK_FORMAT",
    "VPSW_WEBHOOK_SECRET",
    "VPSW_WEBHOOK_TOKEN",
];

/// Settings whose values must never be shown or passed as arguments.
pub const SECRET_KEYS: [&str; 6] = [
    "VPSW_SMTP_PASSWORD",
    "VPSW_SMTP_OAUTH_CLIENT_SECRET",
    "VPSW_SMTP_OAUTH_REFRESH_TOKEN",
    "VPSW_WEBHOOK_URL",
    "VPSW_WEBHOOK_SECRET",
    "VPSW_WEBHOOK_TOKEN",
];

pub fn is_secret(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

/// Runs the command against `file` and returns the lines to print.
pub fn run(file: &Path, key: &str, arg: Option<&str>) -> Result<Vec<String>, String> {
    if !KEYS.contains(&key) {
        return Err(format!("unknown setting '{key}'. Settings are listed in {}", file.display()));
    }
    let secret = is_secret(key);
    let value = match arg {
        Some(_) if secret => {
            return Err(format!(
                "{key} is a secret, so it is not accepted as an argument (it would be saved in shell history).\n\
                 Run 'sudo vps-watcher set {key}' and type it at the hidden prompt, or pipe it in on stdin."
            ));
        }
        Some(v) => v.to_string(),
        None => read_value(key, secret)?,
    };
    let text = std::fs::read_to_string(file).map_err(|e| {
        format!("cannot read {}: {e} (is vps-watcher installed, and did you use sudo?)", file.display())
    })?;
    let updated = set_line(&text, key, &quote(&value)?);
    write_keeping_owner(file, &updated)?;

    let mut out = vec![if secret {
        format!("Saved {key} (value hidden).")
    } else if value.is_empty() {
        format!("Cleared {key}.")
    } else {
        format!("Saved {key}={value}.")
    }];
    // Validate what the service will see; error messages never contain secret values.
    let vars: std::collections::HashMap<String, String> = parse_env_file(&updated).into_iter().collect();
    match Config::from_lookup(|k| vars.get(k).cloned()) {
        Ok(_) => out.push("Settings are valid. Apply them with: sudo systemctl restart vps-watcher".into()),
        Err(e) => out.push(format!("Not complete yet: {e}")),
    }
    Ok(out)
}

/// Reads a value from a hidden terminal prompt, or from stdin when piped.
fn read_value(key: &str, secret: bool) -> Result<String, String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
        return Ok(line.trim_end_matches(['\n', '\r']).to_string());
    }
    eprint!("{key}{}: ", if secret { " (typing is hidden)" } else { "" });
    let _ = std::io::stderr().flush();
    let _echo = if secret { Some(EchoOff::new()) } else { None };
    let mut line = String::new();
    stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
    if secret {
        eprintln!();
    }
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Turns terminal echo off until dropped.
struct EchoOff(Option<libc::termios>);

impl EchoOff {
    fn new() -> Self {
        // SAFETY: termios is plain data; tcgetattr fills it for fd 0 and we only use it on success.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return EchoOff(None);
            }
            let saved = t;
            t.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            EchoOff(Some(saved))
        }
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            // SAFETY: restores the attributes read in `new` on the same fd.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, t);
            }
        }
    }
}

/// Quotes a value so systemd and vps-watcher read it the same way (neither expands it).
fn quote(v: &str) -> Result<String, String> {
    if v.contains(['\n', '\r', '\0']) {
        return Err("a value cannot contain line breaks".into());
    }
    if v.chars().all(|c| c.is_ascii_alphanumeric() || "@._-:/,+%=".contains(c)) {
        Ok(v.to_string())
    } else if !v.contains('\'') {
        Ok(format!("'{v}'"))
    } else if !v.contains(['"', '\\', '$', '`']) {
        Ok(format!("\"{v}\""))
    } else {
        Err("this value mixes quote characters; edit the file with: sudoedit /etc/vps-watcher/vps-watcher.env".into())
    }
}

/// Replaces the first `KEY=` line (dropping later duplicates), or appends one.
fn set_line(text: &str, key: &str, quoted: &str) -> String {
    let is_key = |l: &str| {
        let l = l.trim_start();
        let l = l.strip_prefix("export ").unwrap_or(l);
        l.split_once('=').is_some_and(|(k, _)| k.trim() == key)
    };
    let mut done = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if is_key(line) {
            if !done {
                out.push(format!("{key}={quoted}"));
                done = true;
            }
        } else {
            out.push(line.to_string());
        }
    }
    if !done {
        out.push(format!("{key}={quoted}"));
    }
    out.join("\n") + "\n"
}

/// Writes through a temp file and rename, keeping the file's owner, group and mode (0640 root:vps-watcher).
fn write_keeping_owner(path: &Path, text: &str) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("env.tmp");
    let err = |e: std::io::Error| format!("cannot write {}: {e} (use sudo)", path.display());
    {
        let mut f =
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(err)?;
        f.write_all(text.as_bytes()).map_err(err)?;
        f.sync_all().map_err(err)?;
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    // Only root can hand the file back to root:vps-watcher; a non-root owner editing its own file is fine.
    if let Err(e) = std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()))
        && meta.uid() != me
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(e));
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(meta.mode() & 0o777)).map_err(err)?;
    std::fs::rename(&tmp, path).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_cover_every_setting_the_code_reads() {
        for src in [include_str!("config.rs"), include_str!("alert/settings.rs")] {
            for part in src.split("val(\"").skip(1) {
                let key = part.split('"').next().unwrap();
                assert!(KEYS.contains(&key), "{key} is missing from setcmd::KEYS");
            }
        }
    }

    #[test]
    fn quoting_matches_both_parsers() {
        assert_eq!(quote("smtp.gmail.com").unwrap(), "smtp.gmail.com");
        assert_eq!(quote("abcd efgh#1").unwrap(), "'abcd efgh#1'");
        assert_eq!(quote("it's").unwrap(), "\"it's\"");
        assert!(quote("it's \"both\"").is_err());
        assert!(quote("a\nVPSW_AUTH=open").is_err(), "no injected lines");
        for v in ["abcd efgh#1", "it's", "https://x.io/h?a=1&b=2", "p$ss"] {
            let parsed = parse_env_file(&format!("K={}", quote(v).unwrap()));
            assert_eq!(parsed[0].1, v);
        }
    }

    #[test]
    fn replaces_first_line_and_drops_duplicates() {
        let t = "# c\nVPSW_SMTP_HOST=\nVPSW_AUTH=login\nexport VPSW_SMTP_HOST=old\n";
        assert_eq!(set_line(t, "VPSW_SMTP_HOST", "smtp.x.io"), "# c\nVPSW_SMTP_HOST=smtp.x.io\nVPSW_AUTH=login\n");
        assert_eq!(set_line("A=1", "VPSW_DOMAIN", "w.io"), "A=1\nVPSW_DOMAIN=w.io\n");
        assert!(!set_line("# VPSW_DOMAIN=x\n", "VPSW_DOMAIN", "y").starts_with("VPSW_DOMAIN=y"), "comments stay");
    }

    #[test]
    fn secrets_are_refused_as_arguments_and_never_echoed() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("v.env");
        std::fs::write(&f, "VPSW_SMTP_PASSWORD=\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
        let e = run(&f, "VPSW_SMTP_PASSWORD", Some("hunter2")).unwrap_err();
        assert!(e.contains("not accepted as an argument") && !e.contains("hunter2"));
        assert!(run(&f, "VPSW_NOPE", Some("x")).is_err());

        let out = run(&f, "VPSW_DOMAIN", Some("watch.example.com")).unwrap();
        assert_eq!(out[0], "Saved VPSW_DOMAIN=watch.example.com.");
        assert!(out[1].starts_with("Settings are valid"));
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o640, "mode kept");
        let out = run(&f, "VPSW_SMTP_HOST", Some("smtp.gmail.com")).unwrap();
        assert!(out[1].starts_with("Not complete yet: VPSW_ALERT_EMAIL"), "{out:?}");
    }
}
