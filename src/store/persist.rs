//! Compact binary snapshot of the 1-minute and 15-minute tiers.
//!
//! Layout: b"VPSW", u8 version, u32 count, then per series:
//! u16 key len, key, u64 last_ts, and for tiers 1..=2: u64 newest (u64::MAX = none), u32 len, len × f32.

use std::io::Write;
use std::path::Path;

use super::{History, Ring, Series, TIERS};

const MAGIC: &[u8; 4] = b"VPSW";
const VERSION: u8 = 1;

/// Serializes history; call while holding the lock, then [`write`] without it.
pub fn encode(h: &History) -> Vec<u8> {
    let mut buf = Vec::with_capacity(h.series.len() * 2 * 2880 * 4 + 64);
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&(h.series.len() as u32).to_le_bytes());
    for (key, s) in &h.series {
        let k = key.as_bytes();
        buf.extend_from_slice(&(k.len().min(u16::MAX as usize) as u16).to_le_bytes());
        buf.extend_from_slice(&k[..k.len().min(u16::MAX as usize)]);
        buf.extend_from_slice(&s.last_ts.to_le_bytes());
        for t in &s.tiers[1..] {
            buf.extend_from_slice(&t.ring.newest().unwrap_or(u64::MAX).to_le_bytes());
            let vals = t.ring.ordered();
            buf.extend_from_slice(&(vals.len() as u32).to_le_bytes());
            for v in vals {
                buf.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    buf
}

/// Writes encoded history to `path` atomically (temp file + rename).
pub fn write(buf: &[u8], path: &Path) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Reads history saved by [`save`]; a missing file gives empty history, a corrupt one an error.
pub fn load(path: &Path) -> Result<History, String> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(History::default()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    decode(&data).ok_or_else(|| format!("{} is corrupt; starting with empty history", path.display()))
}

/// Byte cursor that returns None instead of panicking on short input.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

fn decode(data: &[u8]) -> Option<History> {
    let mut r = Reader(data);
    if r.take(4)? != MAGIC || r.take(1)? != [VERSION] {
        return None;
    }
    let count = r.u32()? as usize;
    let mut h = History::default();
    for _ in 0..count {
        let klen = r.u16()? as usize;
        let key = String::from_utf8(r.take(klen)?.to_vec()).ok()?;
        let mut s = Series::new();
        s.last_ts = r.u64()?;
        for (i, t) in s.tiers.iter_mut().enumerate().skip(1) {
            let newest = r.u64()?;
            let len = r.u32()? as usize;
            let bytes = r.take(len.checked_mul(4)?)?;
            let (step, cap) = TIERS[i];
            if newest != u64::MAX && len == cap {
                let vals: Vec<f32> =
                    bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                t.ring = Ring::restore(step, cap, newest, &vals);
            }
        }
        if h.series.len() < super::MAX_SERIES {
            h.series.insert(key, s);
        }
    }
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::super::Range;
    use super::*;

    #[test]
    fn roundtrip_keeps_minute_and_quarter_tiers() {
        let mut h = History::default();
        for i in 0..200u64 {
            h.record("host.cpu", 1_700_000_000 + i * 30, i as f64);
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("history.bin");
        write(&encode(&h), &p).unwrap();
        let back = load(&p).unwrap();
        let now = 1_700_000_000 + 199 * 30;
        let a = h.query("host.cpu", Range::D7, now).unwrap();
        let b = back.query("host.cpu", Range::D7, now).unwrap();
        // Only flushed buckets persist; the in-progress one may differ.
        assert_eq!(a.values[..a.values.len() - 1], b.values[..b.values.len() - 1]);
    }

    #[test]
    fn missing_is_empty_and_garbage_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("none")).unwrap().len(), 0);
        let p = dir.path().join("bad");
        std::fs::write(&p, b"VPSW\x01\xff\xff\xff\x7f").unwrap();
        assert!(load(&p).is_err());
    }
}
