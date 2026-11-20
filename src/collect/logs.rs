//! Log lines from files and journald, with hard caps so huge logs stay cheap.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

/// Most lines one request may return.
pub const MAX_LINES: usize = 1000;
/// Most bytes read from the end of a file per request.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Longer lines are cut so one runaway line cannot fill the page.
const MAX_LINE_CHARS: usize = 4000;

/// One log line as the UI shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogLine {
    /// Unix seconds when known.
    pub ts: Option<u64>,
    pub stream: String,
    pub text: String,
}

impl LogLine {
    /// Builds a line, trimming overly long text.
    pub fn new(ts: Option<u64>, stream: &str, text: &str) -> Self {
        let text = match text.char_indices().nth(MAX_LINE_CHARS) {
            Some((i, _)) => format!("{}…", &text[..i]),
            None => text.to_string(),
        };
        LogLine { ts, stream: stream.to_string(), text }
    }
}

/// Parses the `YYYY-MM-DDTHH:MM:SS` start of an RFC 3339 UTC time into unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + se).ok()
}

/// Returns up to `lines` lines from the end of `path`, reading at most [`MAX_BYTES`].
pub fn tail_file(path: &Path, lines: usize) -> Result<Vec<LogLine>, String> {
    let mut f = crate::util::open_regular(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(MAX_BYTES);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.take(MAX_BYTES).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        all.remove(0); // first line is probably cut in half
    }
    let keep = lines.min(MAX_LINES);
    Ok(all[all.len().saturating_sub(keep)..].iter().map(|l| LogLine::new(None, "file", l)).collect())
}

/// Reads the last `lines` journal entries of a systemd unit via journalctl.
pub async fn journal(unit: &str, lines: usize, since: Option<u64>) -> Result<Vec<LogLine>, String> {
    if !is_safe_unit(unit) {
        return Err("invalid unit name".into());
    }
    let mut cmd = tokio::process::Command::new("journalctl");
    cmd.args(["--no-pager", "-o", "json", "--output-fields=MESSAGE,PRIORITY", "-u", unit])
        .arg(format!("-n{}", lines.min(MAX_LINES)))
        .kill_on_drop(true);
    if let Some(s) = since {
        cmd.arg(format!("--since=@{s}"));
    }
    let out = tokio::time::timeout(Duration::from_secs(8), cmd.output())
        .await
        .map_err(|_| "journalctl timed out".to_string())?
        .map_err(|e| format!("cannot run journalctl: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().filter_map(parse_journal_json).collect())
}

/// Converts one `journalctl -o json` line to a [`LogLine`].
fn parse_journal_json(line: &str) -> Option<LogLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let ts = v["__REALTIME_TIMESTAMP"].as_str().and_then(|t| t.parse::<u64>().ok()).map(|us| us / 1_000_000);
    let text = match &v["MESSAGE"] {
        serde_json::Value::String(s) => s.clone(),
        // Non-UTF-8 messages arrive as byte arrays.
        serde_json::Value::Array(a) => {
            String::from_utf8_lossy(&a.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect::<Vec<_>>())
                .into_owned()
        }
        _ => return None,
    };
    let stream = match v["PRIORITY"].as_str().and_then(|p| p.parse::<u8>().ok()) {
        Some(0..=3) => "stderr",
        _ => "stdout",
    };
    Some(LogLine::new(ts, stream, &text))
}

/// Unit names: letters, digits and `@._-:` only, ending in .service.
pub fn is_safe_unit(unit: &str) -> bool {
    unit.len() <= 200
        && unit.ends_with(".service")
        && !unit.starts_with('-')
        && unit.bytes().all(|b| b.is_ascii_alphanumeric() || b"@._-:\\".contains(&b))
}

/// Finds readable `*.log` files (and syslog-style files) under /var/log, two levels deep.
pub fn discover_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walk = |dir: &Path, out: &mut Vec<PathBuf>| {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut sub = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() && !matches!(name.as_str(), "journal" | "private") {
                sub.push(p);
            } else if ft.is_file() && is_text_log(&name) {
                out.push(p);
            }
        }
        sub
    };
    for d in walk(root, &mut out) {
        walk(&d, &mut out);
    }
    out.sort();
    out.truncate(200);
    out
}

/// Plain-text logs: `*.log`, syslog, messages; never rotated `.gz` or numbered copies.
fn is_text_log(name: &str) -> bool {
    name.ends_with(".log") || matches!(name, "syslog" | "messages" | "auth.log" | "kern.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-01-02T03:04:05.99Z"), Some(1_767_323_045));
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(parse_rfc3339("nope"), None);
        assert_eq!(parse_rfc3339("2026-13-02T03:04:05Z"), None);
    }

    #[test]
    fn tail_reads_only_the_end_of_huge_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.log");
        let mut f = std::fs::File::create(&p).unwrap();
        let line = "x".repeat(99) + "\n";
        for _ in 0..30_000 {
            f.write_all(line.as_bytes()).unwrap(); // ~3 MB, above MAX_BYTES
        }
        writeln!(f, "last").unwrap();
        let out = tail_file(&p, 5000).unwrap();
        assert_eq!(out.len(), MAX_LINES);
        assert_eq!(out.last().unwrap().text, "last");
        assert!(tail_file(&dir.path().join("none"), 5).is_err());
    }

    #[test]
    fn long_lines_are_cut() {
        let l = LogLine::new(None, "x", &"é".repeat(10_000));
        assert_eq!(l.text.chars().count(), MAX_LINE_CHARS + 1);
    }

    #[test]
    fn journal_json() {
        let l =
            parse_journal_json(r#"{"__REALTIME_TIMESTAMP":"1700000000123456","MESSAGE":"hi","PRIORITY":"3"}"#).unwrap();
        assert_eq!((l.ts, l.stream.as_str(), l.text.as_str()), (Some(1_700_000_000), "stderr", "hi"));
        let b = parse_journal_json(r#"{"MESSAGE":[104,105]}"#).unwrap();
        assert_eq!(b.text, "hi");
        assert!(parse_journal_json("not json").is_none());
    }

    #[test]
    fn unit_names() {
        assert!(is_safe_unit("nginx.service"));
        assert!(is_safe_unit("getty@tty1.service"));
        assert!(!is_safe_unit("-x.service"));
        assert!(!is_safe_unit("a b.service"));
        assert!(!is_safe_unit("nginx"));
    }

    #[test]
    fn discovers_log_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("nginx")).unwrap();
        std::fs::write(d.path().join("syslog"), "").unwrap();
        std::fs::write(d.path().join("syslog.2.gz"), "").unwrap();
        std::fs::write(d.path().join("nginx/access.log"), "").unwrap();
        let found: Vec<_> =
            discover_files(d.path()).iter().map(|p| p.strip_prefix(d.path()).unwrap().to_owned()).collect();
        assert_eq!(found, [PathBuf::from("nginx/access.log"), PathBuf::from("syslog")]);
    }
}
