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

/// Standard base64 with padding (RFC 4648).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Decodes standard base64 (padding optional, whitespace ignored); None on any other character.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let clean = clean.strip_suffix(b"==").or_else(|| clean.strip_suffix(b"=")).unwrap_or(&clean);
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in clean {
        acc = acc << 6 | val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Calendar date and time in UTC: (year, month, day, hour, minute, second, weekday 0=Sunday).
fn civil(ts: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let days = (ts / 86_400) as i64;
    let secs = ts % 86_400;
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    let wd = (days + 4).rem_euclid(7) as u32;
    (y, m, d, (secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32, wd)
}

/// `2026-11-21 14:03 UTC`, for alert messages.
pub fn utc_stamp(ts: u64) -> String {
    let (y, mo, d, h, mi, _, _) = civil(ts);
    format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

/// RFC 2822 date for email headers, such as `Sat, 21 Nov 2026 14:03:09 +0000`.
pub fn rfc2822(ts: u64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, mo, d, h, mi, s, wd) = civil(ts);
    format!("{}, {d:02} {} {y} {h:02}:{mi:02}:{s:02} +0000", DAYS[wd as usize], MONTHS[mo as usize - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        let cases =
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")];
        for (i, o) in cases {
            assert_eq!(base64(i.as_bytes()), o);
        }
        assert_eq!(base64(&[0xff, 0xfe]), "//4=");
        for (i, o) in cases {
            assert_eq!(base64_decode(o).unwrap(), i.as_bytes());
        }
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert!(base64_decode("Zm9v!").is_none());
    }

    #[test]
    fn dates() {
        assert_eq!(rfc2822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(rfc2822(1_795_269_789), "Sat, 21 Nov 2026 14:03:09 +0000");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29 00:00 UTC", "leap day");
    }

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
