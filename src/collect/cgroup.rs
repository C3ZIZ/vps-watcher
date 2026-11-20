//! Per-app usage read straight from cgroup files (v2 first, v1 as a fallback).

use std::path::{Path, PathBuf};

use super::host::read;

/// Cumulative counters and gauges of one cgroup.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CgStats {
    /// CPU time used, microseconds.
    pub cpu_usec: u64,
    /// Memory in use minus reclaimable page cache, bytes.
    pub mem: u64,
    /// Memory limit, if one is set.
    pub mem_limit: Option<u64>,
    pub io_read: u64,
    pub io_write: u64,
    pub pids: u64,
    /// Processes the kernel killed for exceeding memory (oom_kill in memory.events).
    pub oom_kills: Option<u64>,
}

/// Where cgroups live and which layout the host uses.
#[derive(Debug, Clone)]
pub struct Cgroups {
    root: PathBuf,
    v2: bool,
}

impl Cgroups {
    /// Detects the layout under `root` (normally /sys/fs/cgroup).
    pub fn detect(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let v2 = root.join("cgroup.controllers").exists();
        Cgroups { root, v2 }
    }

    /// True on unified (v2) hosts.
    pub fn is_v2(&self) -> bool {
        self.v2
    }

    /// Reads stats for a cgroup path relative to the root, e.g. `system.slice/nginx.service`.
    pub fn stats(&self, rel: &str) -> Option<CgStats> {
        let rel = rel.trim_start_matches('/');
        if self.v2 { read_v2(&self.root.join(rel)) } else { read_v1(&self.root, rel) }
    }

    /// Service units (`*.service`) that currently have processes, from the v2 or v1 tree.
    pub fn running_services(&self) -> Vec<String> {
        let base = if self.v2 { self.root.join("system.slice") } else { self.root.join("systemd/system.slice") };
        let Ok(dir) = std::fs::read_dir(&base) else { return Vec::new() };
        let mut out: Vec<String> = dir
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".service"))
            .filter(|n| self.populated(&base.join(n)))
            .collect();
        out.sort();
        out
    }

    /// True if the cgroup directory holds at least one process.
    fn populated(&self, dir: &Path) -> bool {
        if let Some(ev) = read(dir.join("cgroup.events")) {
            return ev.lines().any(|l| l.trim() == "populated 1");
        }
        read(dir.join("cgroup.procs")).is_some_and(|p| !p.trim().is_empty())
    }
}

/// Finds the cgroup path of a process from /proc/<pid>/cgroup.
pub fn path_of_pid(pid: u32) -> Option<String> {
    parse_proc_cgroup(&read(format!("/proc/{pid}/cgroup"))?)
}

/// Picks the unified (`0::`) path, or the v1 cpuacct/memory path.
pub fn parse_proc_cgroup(text: &str) -> Option<String> {
    let mut v1 = None;
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(id), Some(ctrl), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if id == "0" && ctrl.is_empty() {
            return Some(path.to_string());
        }
        if ctrl.split(',').any(|c| c == "cpuacct" || c == "memory") {
            v1 = Some(path.to_string());
        }
    }
    v1
}

/// Reads the v2 interface files in `dir`.
fn read_v2(dir: &Path) -> Option<CgStats> {
    let cpu = read(dir.join("cpu.stat"))?;
    let cpu_usec = keyed(&cpu, "usage_usec").unwrap_or(0);
    let current: u64 = read(dir.join("memory.current")).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let inactive = read(dir.join("memory.stat")).and_then(|s| keyed(&s, "inactive_file")).unwrap_or(0);
    let mem_limit = read(dir.join("memory.max")).and_then(|s| s.trim().parse().ok());
    let (io_read, io_write) = read(dir.join("io.stat")).map(|s| parse_io_stat(&s)).unwrap_or((0, 0));
    let pids = read(dir.join("pids.current")).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let oom_kills = read(dir.join("memory.events")).and_then(|s| keyed(&s, "oom_kill"));
    Some(CgStats { cpu_usec, mem: current.saturating_sub(inactive), mem_limit, io_read, io_write, pids, oom_kills })
}

/// Reads the v1 per-controller hierarchies for `rel`.
fn read_v1(root: &Path, rel: &str) -> Option<CgStats> {
    let num = |ctrl: &str, file: &str| -> Option<u64> {
        read(root.join(ctrl).join(rel).join(file)).and_then(|s| s.trim().parse().ok())
    };
    let cpu_ns = num("cpuacct", "cpuacct.usage").or_else(|| num("cpu,cpuacct", "cpuacct.usage"))?;
    let usage = num("memory", "memory.usage_in_bytes").unwrap_or(0);
    let inactive = read(root.join("memory").join(rel).join("memory.stat"))
        .and_then(|s| keyed(&s, "total_inactive_file"))
        .unwrap_or(0);
    // v1 reports "no limit" as a huge page-aligned number.
    let mem_limit = num("memory", "memory.limit_in_bytes").filter(|v| *v < (1 << 60));
    let (io_read, io_write) = read(root.join("blkio").join(rel).join("blkio.throttle.io_service_bytes"))
        .map(|s| parse_blkio(&s))
        .unwrap_or((0, 0));
    Some(CgStats {
        cpu_usec: cpu_ns / 1000,
        mem: usage.saturating_sub(inactive),
        mem_limit,
        io_read,
        io_write,
        pids: num("pids", "pids.current").unwrap_or(0),
        // Kernels from 4.13 list oom_kill in memory.oom_control.
        oom_kills: read(root.join("memory").join(rel).join("memory.oom_control")).and_then(|s| keyed(&s, "oom_kill")),
    })
}

/// Value of `key value` lines such as cpu.stat and memory.stat.
fn keyed(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ')?.trim().parse().ok())
}

/// Sums rbytes and wbytes over all devices in a v2 io.stat.
pub fn parse_io_stat(text: &str) -> (u64, u64) {
    let (mut r, mut w) = (0u64, 0u64);
    for tok in text.split_ascii_whitespace() {
        if let Some(v) = tok.strip_prefix("rbytes=") {
            r = r.saturating_add(v.parse().unwrap_or(0));
        } else if let Some(v) = tok.strip_prefix("wbytes=") {
            w = w.saturating_add(v.parse().unwrap_or(0));
        }
    }
    (r, w)
}

/// Sums Read and Write lines of a v1 blkio file.
fn parse_blkio(text: &str) -> (u64, u64) {
    let (mut r, mut w) = (0u64, 0u64);
    for l in text.lines() {
        let f: Vec<&str> = l.split_ascii_whitespace().collect();
        if let [_, op, v] = f[..] {
            let v: u64 = v.parse().unwrap_or(0);
            match op {
                "Read" => r = r.saturating_add(v),
                "Write" => w = w.saturating_add(v),
                _ => {}
            }
        }
    }
    (r, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_cgroup_v2_and_v1() {
        assert_eq!(
            parse_proc_cgroup("0::/system.slice/docker-abc.scope\n").as_deref(),
            Some("/system.slice/docker-abc.scope")
        );
        let v1 = "12:pids:/docker/abc\n4:cpu,cpuacct:/docker/abc\n1:name=systemd:/docker/abc\n";
        assert_eq!(parse_proc_cgroup(v1).as_deref(), Some("/docker/abc"));
        assert_eq!(parse_proc_cgroup(""), None);
    }

    #[test]
    fn io_stat_sums_devices() {
        let t = "8:0 rbytes=100 wbytes=200 rios=1 wios=2\n253:0 rbytes=1 wbytes=2\n";
        assert_eq!(parse_io_stat(t), (101, 202));
        assert_eq!(parse_blkio("8:0 Read 10\n8:0 Write 20\n8:0 Total 30\nTotal 30\n"), (10, 20));
    }

    #[test]
    fn reads_v2_tree() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::write(root.join("cgroup.controllers"), "cpu memory").unwrap();
        let svc = root.join("system.slice/web.service");
        std::fs::create_dir_all(&svc).unwrap();
        std::fs::write(svc.join("cpu.stat"), "usage_usec 5000\nuser_usec 1\n").unwrap();
        std::fs::write(svc.join("memory.current"), "1000\n").unwrap();
        std::fs::write(svc.join("memory.stat"), "anon 1\ninactive_file 300\n").unwrap();
        std::fs::write(svc.join("memory.max"), "max\n").unwrap();
        std::fs::write(svc.join("pids.current"), "3\n").unwrap();
        std::fs::write(svc.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        let idle = root.join("system.slice/idle.service");
        std::fs::create_dir_all(&idle).unwrap();
        std::fs::write(idle.join("cgroup.events"), "populated 0\n").unwrap();

        let cg = Cgroups::detect(root);
        assert!(cg.is_v2());
        let s = cg.stats("/system.slice/web.service").unwrap();
        assert_eq!((s.cpu_usec, s.mem, s.mem_limit, s.pids), (5000, 700, None, 3));
        assert_eq!(cg.running_services(), ["web.service"]);
        assert!(cg.stats("system.slice/missing.service").is_none());
    }
}
