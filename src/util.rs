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

/// Opens a log file safely: no symlinks anywhere in the path, no FIFOs or devices, never blocks.
pub fn open_regular(path: &Path) -> std::io::Result<std::fs::File> {
    use std::io::{Error, ErrorKind};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => Error::new(ErrorKind::InvalidInput, "is a symbolic link, which is refused for safety"),
            _ => e,
        })?;
    if !f.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    // O_NOFOLLOW only covers the last component; compare the real path to catch symlinked folders.
    let real = std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd()))?;
    if real != path {
        return Err(Error::new(ErrorKind::InvalidInput, "path goes through a symbolic link"));
    }
    Ok(f)
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
    fn open_regular_rejects_links_fifos_and_linked_dirs() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("app.log");
        std::fs::write(&real, "ok").unwrap();
        assert!(open_regular(&real).is_ok());

        let link = d.path().join("link.log");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        assert!(open_regular(&link).is_err(), "symlinked file");

        let sub = d.path().join("sub");
        std::os::unix::fs::symlink(d.path(), &sub).unwrap();
        assert!(open_regular(&sub.join("app.log")).is_err(), "symlinked folder");

        let fifo = d.path().join("fifo.log");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: valid path string; mkfifo has no other preconditions.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(open_regular(&fifo).is_err(), "FIFO must neither open nor block");
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
