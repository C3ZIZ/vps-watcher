//! Custom branding: an organization's logo and name, shown next to the vps-watcher logo.
//! Stored as one file in the data dir (no database) and cached in memory; a cheap `stat` per read
//! notices changes made by the CLI or another process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::util::{atomic_write, base64, base64_decode};

/// Largest logo accepted; a header logo needs far less.
pub const MAX_LOGO: usize = 256 * 1024;
/// Longest organization name.
const MAX_NAME: usize = 60;

/// The logo as served: bytes, content type and a content hash for caching.
#[derive(Debug, Clone)]
pub struct Logo {
    pub bytes: Arc<[u8]>,
    pub mime: &'static str,
    pub etag: String,
}

/// What the dashboard needs to know.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub logo: Option<Logo>,
    pub name: Option<String>,
}

/// The file on disk: one JSON document, so logo and name always change together.
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mime: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

/// Branding store with an in-memory cache keyed by the file's modification time and size.
pub struct Brand {
    path: PathBuf,
    cache: Mutex<(Option<(SystemTime, u64)>, Info)>,
}

impl Brand {
    pub fn new(data_dir: &Path) -> Self {
        Brand { path: data_dir.join("brand.json"), cache: Mutex::new((None, Info::default())) }
    }

    /// Current branding; reads the file only when it changed since the last call.
    pub fn get(&self) -> Info {
        let stamp =
            std::fs::metadata(&self.path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.0 != stamp {
            c.1 = match stamp {
                Some(_) => load(&self.path).unwrap_or_else(|e| {
                    crate::warn!("ignoring {}: {e}", self.path.display());
                    Info::default()
                }),
                None => Info::default(),
            };
            c.0 = stamp;
        }
        c.1.clone()
    }

    /// Replaces the logo (None removes it), keeping the name.
    pub fn set_logo(&self, bytes: Option<&[u8]>) -> Result<(), String> {
        let logo = bytes.map(check_logo).transpose()?;
        let name = self.get().name;
        self.save(name, logo)
    }

    /// Replaces the organization name (empty removes it), keeping the logo.
    pub fn set_name(&self, name: &str) -> Result<(), String> {
        let name = check_name(name)?;
        let logo = self.get().logo.map(|l| (l.bytes.to_vec(), l.mime));
        self.save(name, logo)
    }

    /// Writes the whole document atomically; an empty document deletes the file.
    fn save(&self, name: Option<String>, logo: Option<(Vec<u8>, &'static str)>) -> Result<(), String> {
        if name.is_none() && logo.is_none() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove branding: {e}")),
                _ => Ok(()),
            };
        }
        let (mime, data) = match logo {
            Some((b, m)) => (Some(m.to_string()), Some(base64(&b))),
            None => (None, None),
        };
        let json = serde_json::to_vec(&Stored { name, mime, data }).map_err(|e| e.to_string())?;
        // World-readable is fine: the logo and name are shown on the public sign-in page anyway.
        atomic_write(&self.path, &json, false).map_err(|e| format!("cannot save branding: {e}"))
    }
}

/// Reads and re-validates the stored document, so a hand-edited file cannot serve anything unsafe.
fn load(path: &Path) -> Result<Info, String> {
    let b = std::fs::read(path).map_err(|e| e.to_string())?;
    if b.len() > MAX_LOGO * 2 {
        return Err("file is too large".into());
    }
    let s: Stored = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    let logo = match s.data {
        Some(d) => {
            let bytes = base64_decode(&d).ok_or("logo is not valid base64")?;
            let (bytes, mime) = check_logo(&bytes)?;
            Some(Logo { etag: etag(&bytes), bytes: bytes.into(), mime })
        }
        None => None,
    };
    Ok(Info { logo, name: check_name(s.name.as_deref().unwrap_or(""))? })
}

/// Accepts PNG, JPEG, WebP or SVG up to MAX_LOGO, identified by content, not by file name.
pub fn check_logo(b: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    if b.is_empty() {
        return Err("The file is empty.".into());
    }
    if b.len() > MAX_LOGO {
        return Err(format!("The logo is {} KB; the limit is {} KB.", b.len() / 1024, MAX_LOGO / 1024));
    }
    let mime = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else if is_svg(b) {
        "image/svg+xml"
    } else {
        return Err("Use a PNG, JPEG, WebP or SVG image.".into());
    };
    Ok((b.to_vec(), mime))
}

/// An SVG document: UTF-8 text whose first element is `<svg`, after an optional XML prolog and comments.
fn is_svg(b: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(&b[..b.len().min(4096)]).or_else(|e| std::str::from_utf8(&b[..e.valid_up_to()]))
    else {
        return false;
    };
    let mut t = text.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(rest) = t.strip_prefix("<?") {
            t = rest.split_once("?>").map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = t.strip_prefix("<!--") {
            t = rest.split_once("-->").map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = t.strip_prefix("<!DOCTYPE") {
            t = rest.split_once('>').map_or("", |(_, r)| r).trim_start();
        } else {
            break;
        }
    }
    t.starts_with("<svg") && t[4..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
}

/// Trims the name; empty means none. Control characters and overlong names are refused.
fn check_name(name: &str) -> Result<Option<String>, String> {
    let n = name.trim();
    if n.is_empty() {
        return Ok(None);
    }
    if n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Err(format!("The name must be 1 to {MAX_NAME} characters, without line breaks."));
    }
    Ok(Some(n.to_string()))
}

/// Short content hash, used as the ETag and to bust caches when the logo changes.
fn etag(b: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, b);
    d.as_ref()[..8].iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const SVG: &[u8] = br#"<?xml version="1.0"?><!-- logo --><svg xmlns="http://www.w3.org/2000/svg"/>"#;

    #[test]
    fn detects_formats_by_content() {
        assert_eq!(check_logo(PNG).unwrap().1, "image/png");
        assert_eq!(check_logo(&[0xff, 0xd8, 0xff, 0xe0]).unwrap().1, "image/jpeg");
        assert_eq!(check_logo(b"RIFF\0\0\0\0WEBPVP8 ").unwrap().1, "image/webp");
        assert_eq!(check_logo(SVG).unwrap().1, "image/svg+xml");
        assert_eq!(check_logo(b"\xef\xbb\xbf  <svg>").unwrap().1, "image/svg+xml", "BOM and spaces");
        for bad in [&b"<html><svg>"[..], b"<svgx>", b"GIF89a", b"#!/bin/sh", b""] {
            assert!(check_logo(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        assert!(check_logo(&vec![0xff; MAX_LOGO + 1]).unwrap_err().contains("limit"));
    }

    #[test]
    fn names() {
        assert_eq!(check_name("  Acme Corp ").unwrap().as_deref(), Some("Acme Corp"));
        assert_eq!(check_name("").unwrap(), None);
        assert!(check_name("a\nb").is_err());
        assert!(check_name(&"n".repeat(61)).is_err());
    }

    #[test]
    fn stores_caches_and_sees_outside_changes() {
        let d = tempfile::tempdir().unwrap();
        let b = Brand::new(d.path());
        assert!(b.get().logo.is_none());
        b.set_name("Acme").unwrap();
        b.set_logo(Some(SVG)).unwrap();
        let i = b.get();
        assert_eq!((i.name.as_deref(), i.logo.as_ref().unwrap().mime), (Some("Acme"), "image/svg+xml"));
        assert_eq!(&*i.logo.unwrap().bytes, SVG);

        // Another process (the CLI) writes the file; the running service notices.
        let other = Brand::new(d.path());
        other.set_logo(Some(PNG)).unwrap();
        assert_eq!(b.get().logo.unwrap().mime, "image/png");
        assert_eq!(b.get().name.as_deref(), Some("Acme"), "name kept");

        b.set_logo(None).unwrap();
        b.set_name("").unwrap();
        assert!(!d.path().join("brand.json").exists(), "nothing left means no file");
    }

    #[test]
    fn hand_edited_files_are_revalidated() {
        let d = tempfile::tempdir().unwrap();
        let script = base64(b"<html><script>alert(1)</script>");
        std::fs::write(d.path().join("brand.json"), format!(r#"{{"mime":"image/svg+xml","data":"{script}"}}"#))
            .unwrap();
        assert!(Brand::new(d.path()).get().logo.is_none(), "not an image, so not served");
    }
}
