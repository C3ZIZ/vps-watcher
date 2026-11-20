//! Process table from /proc, read only when someone asks (or a process monitor needs it).

use std::collections::HashMap;

use serde::Serialize;

use super::host::read;

/// Fields of /proc/<pid>/stat that we use.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcStat {
    pub name: String,
    pub state: char,
    pub ppid: u32,
    /// utime + stime, in clock ticks.
    pub cpu_ticks: u64,
    pub threads: u32,
    pub rss_pages: u64,
}

/// Parses /proc/<pid>/stat; the name may contain spaces and parentheses.
pub fn parse_stat(text: &str) -> Option<ProcStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let f: Vec<&str> = text.get(close + 2..)?.split_ascii_whitespace().collect();
    // After the name: state(0) ppid(1) ... utime(11) stime(12) ... num_threads(17) ... rss(21)
    let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
    Some(ProcStat {
        name,
        state: f.first()?.chars().next()?,
        ppid: n(1)? as u32,
        cpu_ticks: n(11)? + n(12)?,
        threads: n(17).unwrap_or(1) as u32,
        rss_pages: n(21).unwrap_or(0),
    })
}

/// One row of the process list.
#[derive(Debug, Clone, Serialize)]
pub struct ProcRow {
    pub pid: u32,
    pub name: String,
    pub cmd: String,
    pub user: String,
    pub state: String,
    pub cpu_pct: f32,
    pub mem: u64,
    pub threads: u32,
}

/// Reads all numeric /proc entries as (pid, stat).
pub fn snapshot() -> HashMap<u32, ProcStat> {
    let Ok(rd) = std::fs::read_dir("/proc") else { return HashMap::new() };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| Some((pid, parse_stat(&read(format!("/proc/{pid}/stat"))?)?)))
        .collect()
}

/// CPU percent (of all cores) per pid between two snapshots `secs` apart.
pub fn cpu_between(a: &HashMap<u32, ProcStat>, b: &HashMap<u32, ProcStat>, secs: f64) -> HashMap<u32, f32> {
    let hz = clock_ticks() as f64;
    let cores = cores() as f64;
    b.iter()
        .filter_map(|(pid, s)| {
            let prev = a.get(pid)?;
            let d = s.cpu_ticks.checked_sub(prev.cpu_ticks)? as f64;
            Some((*pid, (d / hz / secs / cores * 100.0) as f32))
        })
        .collect()
}

/// Builds sorted rows (highest CPU, then memory) from two snapshots.
pub fn rows(a: &HashMap<u32, ProcStat>, b: &HashMap<u32, ProcStat>, secs: f64, limit: usize) -> Vec<ProcRow> {
    let cpu = cpu_between(a, b, secs);
    let page = page_size();
    let users = user_names();
    let mut out: Vec<ProcRow> = b
        .iter()
        .map(|(pid, s)| ProcRow {
            pid: *pid,
            name: s.name.clone(),
            cmd: cmdline(*pid).unwrap_or_else(|| format!("[{}]", s.name)),
            user: uid_of(*pid).and_then(|u| users.get(&u).cloned().or(Some(u.to_string()))).unwrap_or_default(),
            state: s.state.to_string(),
            cpu_pct: cpu.get(pid).copied().unwrap_or(0.0),
            mem: s.rss_pages * page,
            threads: s.threads,
        })
        .collect();
    out.sort_by(|x, y| y.cpu_pct.total_cmp(&x.cpu_pct).then(y.mem.cmp(&x.mem)));
    out.truncate(limit);
    out
}

/// Command line with NULs turned into spaces, cut to 300 characters.
pub fn cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let s: String = String::from_utf8_lossy(&raw).replace('\0', " ").trim().chars().take(300).collect();
    (!s.is_empty()).then_some(s)
}

/// Real uid from /proc/<pid>/status.
fn uid_of(pid: u32) -> Option<u32> {
    read(format!("/proc/{pid}/status"))?
        .lines()
        .find_map(|l| l.strip_prefix("Uid:")?.split_ascii_whitespace().next()?.parse().ok())
}

/// uid → name from /etc/passwd.
fn user_names() -> HashMap<u32, String> {
    read("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            Some((f.nth(1)?.parse().ok()?, name.to_string()))
        })
        .collect()
}

/// Kernel clock ticks per second (usually 100).
pub fn clock_ticks() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 { v as u64 } else { 100 }
}

/// Memory page size in bytes.
pub fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 { v as u64 } else { 4096 }
}

/// Online CPUs on the host. (available_parallelism would shrink under our own systemd CPUQuota.)
pub fn cores() -> usize {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 { n as usize } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_tricky_name() {
        let t = "123 (my (weird) app) S 1 123 123 0 -1 4194304 100 0 0 0 250 50 0 0 20 0 7 0 1000 100000 2560 18446744073709551615";
        let s = parse_stat(t).unwrap();
        assert_eq!(s.name, "my (weird) app");
        assert_eq!((s.state, s.ppid, s.cpu_ticks, s.threads, s.rss_pages), ('S', 1, 300, 7, 2560));
        assert!(parse_stat("garbage").is_none());
    }

    #[test]
    fn cpu_between_skips_new_and_reused_pids() {
        let p = |t| ProcStat { name: "x".into(), state: 'R', ppid: 1, cpu_ticks: t, threads: 1, rss_pages: 1 };
        let a = HashMap::from([(1, p(100)), (2, p(500))]);
        let b = HashMap::from([(1, p(100 + clock_ticks())), (2, p(10)), (3, p(1))]);
        let c = cpu_between(&a, &b, 1.0);
        assert!(c[&1] > 0.0);
        assert!(!c.contains_key(&2), "pid reused with lower counter");
        assert!(!c.contains_key(&3), "new pid has no baseline");
    }

    #[test]
    fn cores_matches_proc_stat() {
        let per_core = std::fs::read_to_string("/proc/stat")
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("cpu") && !l.starts_with("cpu "))
            .count();
        assert_eq!(cores(), per_core);
    }

    #[test]
    fn live_snapshot_sees_ourselves() {
        let me = std::process::id();
        assert!(snapshot().contains_key(&me));
        assert!(cmdline(me).is_some());
    }
}
