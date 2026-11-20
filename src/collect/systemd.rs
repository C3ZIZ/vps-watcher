//! systemd helpers: failed units and telling OS plumbing apart from user apps.

use std::time::Duration;

/// Units that are part of the base OS; the UI hides them unless asked.
const SYSTEM_PREFIXES: [&str; 31] = [
    "systemd-",
    "dbus",
    "cron",
    "ssh",
    "sshd",
    "rsyslog",
    "polkit",
    "networkd-dispatcher",
    "unattended-upgrades",
    "snapd",
    "getty@",
    "serial-getty@",
    "user@",
    "multipathd",
    "irqbalance",
    "chrony",
    "ntp",
    "accounts-daemon",
    "udisks2",
    "ModemManager",
    "NetworkManager",
    "wpa_supplicant",
    "atd",
    "qemu-guest-agent",
    "containerd",
    "packagekit",
    "upower",
    "rtkit",
    "avahi",
    "cloud-",
    "apport",
];

/// True for base-OS services, so the app list can focus on what the user deployed.
pub fn is_system_unit(unit: &str) -> bool {
    SYSTEM_PREFIXES.iter().any(|p| unit.starts_with(p)) || unit == "docker.service" || unit == "vps-watcher.service"
}

/// Names of service units systemd reports as failed; empty if systemctl is unavailable.
pub async fn failed_units() -> Vec<String> {
    let cmd = tokio::process::Command::new("systemctl")
        .args(["list-units", "--failed", "--type=service", "--plain", "--no-legend", "--no-pager"])
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(5), cmd).await {
        Ok(Ok(out)) => parse_failed(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// First column of each `systemctl list-units --plain` line.
fn parse_failed(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split_ascii_whitespace().next())
        .filter(|u| u.ends_with(".service"))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_units() {
        assert!(is_system_unit("systemd-journald.service"));
        assert!(is_system_unit("ssh.service"));
        assert!(!is_system_unit("nginx.service"));
        assert!(!is_system_unit("myapp.service"));
    }

    #[test]
    fn parses_failed_list() {
        let t = "app.service loaded failed failed My App\nx.socket loaded failed failed X\n";
        assert_eq!(parse_failed(t), ["app.service"]);
        assert!(parse_failed("").is_empty());
    }
}
