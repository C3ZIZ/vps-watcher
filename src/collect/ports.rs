//! Listening TCP/UDP sockets mapped to their owning process.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use serde::Serialize;

use super::host::read;

/// One listening socket.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Listener {
    pub proto: &'static str,
    pub addr: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub process: Option<String>,
    /// Container or service that owns the port, filled in by the caller.
    pub app: Option<String>,
    /// True when bound to all interfaces (reachable from outside unless a firewall blocks it).
    pub public: bool,
}

/// Parses /proc/net/{tcp,tcp6,udp,udp6} into (addr, port, inode) for listening sockets.
pub fn parse_proc_net(text: &str, v6: bool, udp: bool) -> Vec<(String, u16, u64)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            let (local, state, inode) = (f.get(1)?, f.get(3)?, f.get(9)?);
            // TCP 0A = LISTEN; UDP 07 = unconnected (bound).
            if *state != if udp { "07" } else { "0A" } {
                return None;
            }
            let (ip_hex, port_hex) = local.split_once(':')?;
            let port = u16::from_str_radix(port_hex, 16).ok()?;
            let addr = if v6 { hex_v6(ip_hex)?.to_string() } else { hex_v4(ip_hex)?.to_string() };
            Some((addr, port, inode.parse().ok()?))
        })
        .filter(|(_, port, _)| *port != 0)
        .collect()
}

/// Kernel prints IPv4 as one little-endian u32.
fn hex_v4(h: &str) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::from(u32::from_str_radix(h, 16).ok()?.swap_bytes()))
}

/// Kernel prints IPv6 as four little-endian u32 words.
fn hex_v6(h: &str) -> Option<Ipv6Addr> {
    if h.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for w in 0..4 {
        let word = u32::from_str_radix(&h[w * 8..w * 8 + 8], 16).ok()?.swap_bytes();
        bytes[w * 4..w * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    Some(Ipv6Addr::from(bytes))
}

/// Maps socket inodes to pids by scanning /proc/*/fd (needs root or CAP_SYS_PTRACE for other users).
fn inode_owners() -> HashMap<u64, u32> {
    let mut map = HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return map };
    for pid in rd.flatten().filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()) {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in fds.flatten() {
            if let Ok(t) = std::fs::read_link(fd.path())
                && let Some(inode) =
                    t.to_str().and_then(|s| s.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok())
            {
                map.entry(inode).or_insert(pid);
            }
        }
    }
    map
}

/// All listening sockets, de-duplicated and sorted by port.
pub fn listeners() -> Vec<Listener> {
    let owners = inode_owners();
    let mut out = Vec::new();
    for (file, v6, udp, proto) in [
        ("tcp", false, false, "tcp"),
        ("tcp6", true, false, "tcp"),
        ("udp", false, true, "udp"),
        ("udp6", true, true, "udp"),
    ] {
        let Some(text) = read(format!("/proc/net/{file}")) else { continue };
        for (addr, port, inode) in parse_proc_net(&text, v6, udp) {
            let pid = owners.get(&inode).copied();
            let process = pid.and_then(|p| read(format!("/proc/{p}/comm"))).map(|s| s.trim().to_string());
            let public = addr == "0.0.0.0" || addr == "::";
            out.push(Listener { proto, addr, port, pid, process, app: None, public });
        }
    }
    out.sort_by(|a, b| (a.port, a.proto, &a.addr).cmp(&(b.port, b.proto, &b.addr)));
    out.dedup_by(|a, b| a.port == b.port && a.proto == b.proto && a.addr == b.addr);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listening_tcp4() {
        let t = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:238C 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1\n   1: 0100007F:1F90 0100007F:D3B2 01 00000000:00000000 00:00000000 00000000     0        0 999 1\n";
        assert_eq!(parse_proc_net(t, false, false), vec![("127.0.0.1".to_string(), 9100, 12345)]);
    }

    #[test]
    fn parses_v6_any_and_udp() {
        let t = "h\n 0: 00000000000000000000000000000000:0050 00000000000000000000000000000000:0000 0A 0:0 0:0 0 0 0 77 1\n";
        assert_eq!(parse_proc_net(t, true, false), vec![("::".to_string(), 80, 77)]);
        let u = "h\n 0: 00000000:0035 00000000:0000 07 0:0 0:0 0 0 0 5 2\n";
        assert_eq!(parse_proc_net(u, false, true), vec![("0.0.0.0".to_string(), 53, 5)]);
        assert!(parse_proc_net("h\n broken\n", false, false).is_empty());
    }

    #[test]
    fn v6_loopback_decodes() {
        assert_eq!(hex_v6("00000000000000000000000001000000"), Some(Ipv6Addr::LOCALHOST));
    }
}
