//! Small shared helpers.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in seconds.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Short human age such as `45s`, `12m`, `3h` or `2d`.
pub fn fmt_age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// `n` random bytes from the OS, hex encoded.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    // The OS RNG only fails on broken systems; refusing to run is the safe answer there.
    getrandom::fill(&mut buf).expect("OS random number generator is unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `data` via a temp file and rename; `private` makes the file 0600.
pub fn atomic_write(path: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(if private { 0o600 } else { 0o644 })
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_is_unique_and_sized() {
        let a = random_hex(16);
        assert_eq!(a.len(), 32);
        assert_ne!(a, random_hex(16));
    }

    #[test]
    fn atomic_write_sets_private_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("secret.json");
        atomic_write(&p, b"{}", true).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&p).unwrap(), b"{}");
    }
}
