//! Host-wide readers for /proc and statvfs. Parsers take text so they are easy to test.

use std::ffi::CString;
use std::path::Path;

/// Cumulative CPU jiffies from one `cpu` line of /proc/stat.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    /// All jiffies, guest time excluded (already counted in user).
    pub fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal
    }
}

/// CPU usage between two readings, as percentages of all capacity.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct CpuPct {
    pub total: f32,
    pub user: f32,
    pub system: f32,
    pub iowait: f32,
    pub steal: f32,
}

/// Percent busy from `a` to `b`; None if counters went backwards or nothing elapsed.
pub fn cpu_pct(a: &CpuTimes, b: &CpuTimes) -> Option<CpuPct> {
    let dt = b.total().checked_sub(a.total()).filter(|d| *d > 0)? as f32;
    let d = |x: u64, y: u64| y.saturating_sub(x) as f32 * 100.0 / dt;
    let idle = d(a.idle, b.idle) + d(a.iowait, b.iowait);
    Some(CpuPct {
        total: (100.0 - idle).clamp(0.0, 100.0),
        user: d(a.user + a.nice, b.user + b.nice),
        system: d(a.system + a.irq + a.softirq, b.system + b.irq + b.softirq),
        iowait: d(a.iowait, b.iowait),
        steal: d(a.steal, b.steal),
    })
}

/// Parses the aggregate line and each per-core line of /proc/stat.
pub fn parse_stat(text: &str) -> Option<(CpuTimes, Vec<CpuTimes>)> {
    let mut total = None;
    let mut cores = Vec::new();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        let Some(name) = it.next() else { continue };
        if !name.starts_with("cpu") {
            continue;
        }
        let n: Vec<u64> = it.take(8).map(|v| v.parse().unwrap_or(0)).collect();
        if n.len() < 4 {
            continue;
        }
        let g = |i: usize| n.get(i).copied().unwrap_or(0);
        let t = CpuTimes {
            user: g(0),
            nice: g(1),
            system: g(2),
            idle: g(3),
            iowait: g(4),
            irq: g(5),
            softirq: g(6),
            steal: g(7),
        };
        if name == "cpu" { total = Some(t) } else { cores.push(t) }
    }
    Some((total?, cores))
}

/// Memory figures in bytes from /proc/meminfo.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

/// Parses /proc/meminfo (values are in kB).
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut m = MemInfo::default();
    let (mut free, mut buffers, mut have_avail) = (0, 0, false);
    for line in text.lines() {
        let Some((k, rest)) = line.split_once(':') else { continue };
        let v = rest.split_ascii_whitespace().next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
        match k {
            "MemTotal" => m.total = v,
            "MemAvailable" => {
                m.available = v;
                have_avail = true;
            }
            "MemFree" => free = v,
            "Buffers" => buffers = v,
            "Cached" => m.cached = v,
            "SwapTotal" => m.swap_total = v,
            "SwapFree" => m.swap_free = v,
            _ => {}
        }
    }
    if !have_avail {
        // Kernels before 3.14 lack MemAvailable; this is the classic estimate.
        m.available = free + buffers + m.cached;
    }
    (m.total > 0).then_some(m)
}

/// Load averages from /proc/loadavg.
pub fn parse_loadavg(text: &str) -> Option<[f32; 3]> {
    let mut it = text.split_ascii_whitespace().map(|v| v.parse::<f32>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// Seconds since boot from /proc/uptime.
pub fn parse_uptime(text: &str) -> Option<u64> {
    text.split_ascii_whitespace().next()?.parse::<f64>().ok().map(|v| v as u64)
}

/// Received and sent byte counters of one interface.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetBytes {
    pub rx: u64,
    pub tx: u64,
}

/// Parses /proc/net/dev into (interface, counters).
pub fn parse_net_dev(text: &str) -> Vec<(String, NetBytes)> {
    text.lines()
        .skip(2)
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let f: Vec<u64> = rest.split_ascii_whitespace().filter_map(|v| v.parse().ok()).collect();
            Some((name.trim().to_string(), NetBytes { rx: *f.first()?, tx: *f.get(8)? }))
        })
        .collect()
}

/// True for interfaces that carry real traffic (not loopback, bridges or container veths).
pub fn is_physical_iface(name: &str) -> bool {
    const SKIP: [&str; 12] =
        ["lo", "veth", "docker", "br-", "virbr", "cni", "flannel", "cali", "vxlan", "tun", "podman", "kube"];
    !SKIP.iter().any(|p| name == *p || (p.len() > 2 && name.starts_with(p)))
}

/// Parses /proc/diskstats into (device, sectors read, sectors written).
pub fn parse_diskstats(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            Some((f.get(2)?.to_string(), f.get(5)?.parse().ok()?, f.get(9)?.parse().ok()?))
        })
        .collect()
}

/// True for whole physical disks (they have a `device` link in sysfs; partitions, loop and dm do not).
pub fn is_physical_disk(name: &str) -> bool {
    !name.contains('/') && Path::new("/sys/block").join(name).join("device").exists()
}

/// A mounted filesystem worth showing.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub device: String,
    pub path: String,
    pub fstype: String,
}

/// Parses /proc/self/mountinfo, keeping real filesystems once per device.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    const REAL: [&str; 10] = ["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "vfat", "jfs", "reiserfs"];
    const HIDE: [&str; 5] = ["/var/lib/docker", "/var/lib/containers", "/snap", "/run", "/var/snap"];
    let mut out: Vec<Mount> = Vec::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let path = left.split_ascii_whitespace().nth(4).map(unescape_mount);
        let mut r = right.split_ascii_whitespace();
        let (Some(path), Some(fstype), Some(device)) = (path, r.next(), r.next()) else { continue };
        if !REAL.contains(&fstype) || HIDE.iter().any(|h| path.starts_with(h)) {
            continue;
        }
        // btrfs subvolumes share a device; keep the shortest path for each.
        match out.iter_mut().find(|m| m.device == device) {
            Some(m) if path.len() < m.path.len() => m.path = path,
            Some(_) => {}
            None => out.push(Mount { device: device.into(), path, fstype: fstype.into() }),
        }
    }
    out
}

/// Turns mountinfo octal escapes such as `\040` back into characters.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Space and inode usage of one filesystem.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FsUsage {
    pub total: u64,
    pub used: u64,
    pub avail: u64,
    pub inodes_total: u64,
    pub inodes_used: u64,
}

/// Calls statvfs on `path`.
pub fn statvfs(path: &str) -> Option<FsUsage> {
    let c = CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated string and `s` is a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frs = s.f_frsize as u64;
    let total = s.f_blocks as u64 * frs;
    let free = s.f_bfree as u64 * frs;
    Some(FsUsage {
        total,
        used: total.saturating_sub(free),
        avail: s.f_bavail as u64 * frs,
        inodes_total: s.f_files as u64,
        inodes_used: (s.f_files as u64).saturating_sub(s.f_ffree as u64),
    })
}

/// Counter difference, or None when the counter reset or wrapped.
pub fn delta(prev: u64, cur: u64) -> Option<u64> {
    cur.checked_sub(prev)
}

/// Reads a file to a string, or None if it is missing or unreadable.
pub fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str =
        "cpu  100 0 50 800 20 0 10 20 0 0\ncpu0 50 0 25 400 10 0 5 10 0 0\ncpu1 50 0 25 400 10 0 5 10 0 0\nintr 1 2\n";

    #[test]
    fn stat_and_cpu_pct() {
        let (a, cores) = parse_stat(STAT).unwrap();
        assert_eq!(cores.len(), 2);
        assert_eq!(a.total(), 1000);
        let mut b = a;
        b.user += 50;
        b.idle += 40;
        b.steal += 10;
        let p = cpu_pct(&a, &b).unwrap();
        assert!((p.total - 60.0).abs() < 0.01);
        assert!((p.steal - 10.0).abs() < 0.01);
        assert!(cpu_pct(&b, &a).is_none(), "counters going back are rejected");
        assert!(cpu_pct(&a, &a).is_none());
    }

    #[test]
    fn meminfo_with_and_without_available() {
        let m = parse_meminfo("MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 600 kB\nCached: 300 kB\nSwapTotal: 10 kB\nSwapFree: 4 kB\n").unwrap();
        assert_eq!(m.available, 600 * 1024);
        assert_eq!(m.swap_free, 4096);
        let old = parse_meminfo("MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 300 kB\n").unwrap();
        assert_eq!(old.available, 450 * 1024);
        assert!(parse_meminfo("garbage").is_none());
    }

    #[test]
    fn small_files() {
        assert_eq!(parse_loadavg("0.50 1.00 2.25 1/200 999\n"), Some([0.5, 1.0, 2.25]));
        assert_eq!(parse_loadavg("x"), None);
        assert_eq!(parse_uptime("12345.67 999.0\n"), Some(12345));
    }

    #[test]
    fn net_dev_and_filter() {
        let t = "Inter-|   Receive\n face |bytes\n    lo: 10 1 0 0 0 0 0 0 20 1 0 0 0 0 0 0\n  eth0: 1000 5 0 0 0 0 0 0 2000 6 0 0 0 0 0 0\nveth12ab: 1 1 0 0 0 0 0 0 1 1 0 0 0 0 0 0\n";
        let v = parse_net_dev(t);
        assert_eq!(v.len(), 3);
        assert_eq!(v[1], ("eth0".into(), NetBytes { rx: 1000, tx: 2000 }));
        let phys: Vec<_> = v.iter().filter(|(n, _)| is_physical_iface(n)).map(|(n, _)| n.as_str()).collect();
        assert_eq!(phys, ["eth0"]);
        assert!(is_physical_iface("ens3") && is_physical_iface("wlan0"));
        assert!(!is_physical_iface("br-1a2b") && !is_physical_iface("docker0"));
    }

    #[test]
    fn diskstats() {
        let v = parse_diskstats("   8       0 sda 100 0 2000 0 50 0 4000 0 0 0 0\n 7 0 loop0 1\n");
        assert_eq!(v, vec![("sda".to_string(), 2000, 4000)]);
    }

    #[test]
    fn mountinfo_keeps_real_and_dedups() {
        let t = "\
22 1 8:1 / / rw - ext4 /dev/sda1 rw
23 1 0:5 / /proc rw - proc proc rw
24 1 8:1 /var/lib/docker /var/lib/docker rw - ext4 /dev/sda1 rw
25 1 8:2 / /mnt/my\\040disk rw - xfs /dev/sdb rw
26 1 0:40 / /data rw - btrfs /dev/sdc rw
27 1 0:40 /sub /data/sub rw - btrfs /dev/sdc rw
28 1 0:41 / /x rw - overlay overlay rw";
        let m = parse_mountinfo(t);
        let paths: Vec<_> = m.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["/", "/mnt/my disk", "/data"]);
    }

    #[test]
    fn delta_detects_reset() {
        assert_eq!(delta(10, 15), Some(5));
        assert_eq!(delta(15, 10), None);
    }

    #[test]
    fn statvfs_root_works() {
        let u = statvfs("/").unwrap();
        assert!(u.total > 0 && u.used <= u.total);
        assert!(statvfs("/definitely/not/here").is_none());
    }
}
