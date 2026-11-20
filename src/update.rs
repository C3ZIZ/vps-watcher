//! Updates: a daily check for new releases, and `vps-watcher upgrade`, which installs only releases
//! whose file list is signed with the maintainer's key. GitHub alone cannot produce that signature.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::alert::{self, Event, Severity, State};
use crate::collect::Shared;
use crate::config::Config;
use crate::net;
use crate::util::{atomic_write, base64_decode, now};
use crate::{VERSION, info, warn};

/// Ed25519 public key of the release signing key (private half stays offline with the maintainer).
const RELEASE_KEY: [u8; 32] = [
    0xe8, 0x75, 0x58, 0x71, 0xd6, 0x8f, 0xf3, 0x1d, 0x1e, 0xfa, 0xc3, 0x71, 0xd1, 0x8e, 0x0e, 0xaa, 0x12, 0x62, 0x65,
    0x05, 0x72, 0xf6, 0xb3, 0x0f, 0x0e, 0xf3, 0x17, 0x4e, 0x3e, 0xe8, 0xe3, 0xde,
];
const GITHUB_LATEST: &str = "https://api.github.com/repos/C3ZIZ/vps-watcher/releases/latest";
const GITHUB_DOWNLOAD: &str = "https://github.com/C3ZIZ/vps-watcher/releases/download";
pub const RELEASES_PAGE: &str = "https://github.com/C3ZIZ/vps-watcher/releases";
/// Signed list of release files and their SHA-256 hashes.
const MANIFEST: &str = "SHA256SUMS";
const BIN: &str = "/usr/local/bin/vps-watcher";
const UNIT_DIR: &str = "/etc/systemd/system";
const CONF_DIR: &str = "/etc/vps-watcher";
/// Largest download accepted (binaries are about 4 MB).
const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;

/// True for release tags such as `v26.11.21.01.38`.
pub fn is_release_tag(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 15
        && b[0] == b'v'
        && b[1..].iter().enumerate().all(|(i, c)| if i % 3 == 2 { *c == b'.' } else { c.is_ascii_digit() })
}

/// True when `latest` is a newer release than `current`. Tags have fixed widths, so text order is
/// time order. Development builds never compare as older, so they are never replaced by surprise.
pub fn is_newer(latest: &str, current: &str) -> bool {
    is_release_tag(latest) && is_release_tag(current) && latest > current
}

/// Where releases come from.
#[derive(Debug, Clone)]
pub struct Source {
    latest: String,
    download: String,
}

impl Source {
    /// GitHub, or `base` (a mirror) serving `/releases/latest` and `/releases/download/<tag>/<file>`.
    pub fn new(base: Option<&str>) -> Self {
        match base {
            Some(b) => Source { latest: format!("{b}/releases/latest"), download: format!("{b}/releases/download") },
            None => Source { latest: GITHUB_LATEST.into(), download: GITHUB_DOWNLOAD.into() },
        }
    }

    fn file(&self, tag: &str, name: &str) -> String {
        format!("{}/{tag}/{name}", self.download)
    }
}

/// GET with up to five redirects. Plain HTTP is used only when the original address was this
/// machine, and a redirect may never leave HTTPS.
async fn get(url: &str, max: usize) -> Result<Vec<u8>, String> {
    let first = net::parse_url(url).map_err(|_| "the release address is not valid".to_string())?;
    let local = first.is_loopback() && !first.https;
    let mut url = url.to_string();
    for _ in 0..5 {
        let u = net::parse_url(&url).map_err(|_| "the release server sent a bad address".to_string())?;
        if !u.https && !(local && u.is_loopback()) {
            return Err("refusing to download over plain http".into());
        }
        let headers = [("accept", "application/vnd.github+json, application/octet-stream")];
        let res =
            tokio::time::timeout(Duration::from_secs(120), net::request(hyper::Method::GET, &u, &headers, None, max))
                .await
                .map_err(|_| format!("{} did not answer in time", u.host))??;
        match res.status {
            200 => return Ok(res.body),
            301 | 302 | 303 | 307 | 308 => url = resolve(&u, &res.location.ok_or("a redirect without a target")?),
            404 => return Err(format!("{} has no such file (HTTP 404)", u.host)),
            s => return Err(format!("{} answered HTTP {s}", u.host)),
        }
    }
    Err("too many redirects".into())
}

/// Turns a redirect target into an absolute URL relative to `base`.
fn resolve(base: &net::Url, loc: &str) -> String {
    let scheme = if base.https { "https" } else { "http" };
    if loc.contains("://") {
        loc.to_string()
    } else if let Some(rest) = loc.strip_prefix("//") {
        format!("{scheme}://{rest}")
    } else if loc.starts_with('/') {
        format!("{scheme}://{}{loc}", host_port(base))
    } else {
        let dir = base.path.split('?').next().unwrap_or("/");
        let dir = &dir[..dir.rfind('/').map_or(0, |i| i + 1)];
        format!("{scheme}://{}{}{loc}", host_port(base), if dir.is_empty() { "/" } else { dir })
    }
}

/// `host` or `host:port` when the port is not the scheme's default.
fn host_port(u: &net::Url) -> String {
    let default = if u.https { 443 } else { 80 };
    if u.port == default { u.host.clone() } else { format!("{}:{}", u.host, u.port) }
}

/// Tag of the newest published release.
pub async fn latest(src: &Source) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }
    let body = get(&src.latest, 1024 * 1024).await?;
    let r: Release =
        serde_json::from_slice(&body).map_err(|_| "unexpected reply from the release server".to_string())?;
    if !is_release_tag(&r.tag_name) {
        return Err(format!(
            "the latest release has an unexpected name '{}'",
            r.tag_name.chars().take(40).collect::<String>()
        ));
    }
    Ok(r.tag_name)
}

/// A verified file list: the release tag and each file's SHA-256 (hex).
#[derive(Debug, PartialEq)]
pub struct Manifest {
    pub tag: String,
    pub files: HashMap<String, String>,
}

/// Checks the signature over the exact manifest bytes, then parses it.
pub fn verify_manifest(text: &[u8], sig: &str, key: &[u8]) -> Result<Manifest, String> {
    let sig = base64_decode(sig).filter(|s| s.len() == 64).ok_or("the release signature is malformed")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(text, &sig)
        .map_err(|_| "the release signature is not valid; refusing to install it".to_string())?;
    let text = std::str::from_utf8(text).map_err(|_| "the signed file list is not text")?;
    let mut lines = text.lines();
    let tag = lines
        .next()
        .and_then(|l| l.strip_prefix("vps-watcher "))
        .filter(|t| is_release_tag(t))
        .ok_or("the signed file list has no release name")?
        .to_string();
    let mut files = HashMap::new();
    for l in lines.filter(|l| !l.trim().is_empty()) {
        let (hash, name) = l.split_once("  ").ok_or("the signed file list is malformed")?;
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || name.contains('/') {
            return Err("the signed file list is malformed".into());
        }
        files.insert(name.to_string(), hash.to_ascii_lowercase());
    }
    Ok(Manifest { tag, files })
}

fn sha256_hex(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads `name` and checks it against the signed list.
async fn fetch_verified(src: &Source, m: &Manifest, name: &str) -> Result<Vec<u8>, String> {
    let want = m.files.get(name).ok_or_else(|| format!("{name} is not in the signed file list"))?;
    let data = get(&src.file(&m.tag, name), MAX_DOWNLOAD).await?;
    if &sha256_hex(&data) != want {
        return Err(format!("{name} does not match its signed checksum; refusing to install it"));
    }
    Ok(data)
}

/// Marks the error for a release whose signature is not uploaded yet (it is signed after the build).
const NOT_SIGNED: &str = "is not signed yet";

/// Downloads and verifies the signed manifest of `tag`.
async fn fetch_manifest(src: &Source, tag: &str) -> Result<Manifest, String> {
    let missing = |e: String| {
        if e.contains("HTTP 404") {
            format!("{tag} {NOT_SIGNED}; upgrades install only signed releases. Try again later.")
        } else {
            e
        }
    };
    let text = get(&src.file(tag, MANIFEST), 64 * 1024).await.map_err(missing)?;
    let sig = get(&src.file(tag, &format!("{MANIFEST}.sig")), 4096).await.map_err(missing)?;
    let m = verify_manifest(&text, &String::from_utf8_lossy(&sig), &RELEASE_KEY)?;
    if m.tag != tag {
        return Err(format!("the signed file list is for {}, not {tag}; refusing to install it", m.tag));
    }
    Ok(m)
}

// ---------- the service's daily check ----------

/// What the dashboard shows about updates.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub current: &'static str,
    pub latest: Option<String>,
    pub available: bool,
    pub checked: Option<u64>,
    pub error: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// Newest release already announced, so each one is announced once.
    notified: Option<String>,
}

/// Checks for a new release two minutes after start, then about once a day.
pub async fn run(sh: Arc<Shared>) {
    if !sh.cfg.update_check {
        return;
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
    loop {
        check_now(&sh).await;
        // Spread checks out so many servers do not ask GitHub at the same moment.
        let jitter = u64::from_str_radix(&crate::util::random_hex(2), 16).unwrap_or(0) % 3600;
        tokio::time::sleep(Duration::from_secs(86_400 + jitter)).await;
    }
}

/// Asks the release server now, records the result and announces a new release once.
/// One check runs at a time; a failed check keeps the last good answer.
pub async fn check_now(sh: &Shared) -> Status {
    static CHECKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let current = || sh.update.lock().map(|s| s.clone()).unwrap_or_default();
    let Ok(_one) = CHECKING.try_lock() else { return current() };
    let src = Source::new(sh.cfg.update_url.as_deref());
    let result = latest(&src).await;
    let mut st = current();
    st.current = VERSION;
    st.checked = Some(now());
    match result {
        Ok(tag) => {
            st.available = is_newer(&tag, VERSION);
            st.latest = Some(tag);
            st.error = None;
        }
        Err(e) => {
            warn!("update check failed: {e}");
            st.error = Some(e);
        }
    }
    if let Ok(mut s) = sh.update.lock() {
        *s = st.clone();
    }
    if st.available
        && let Some(tag) = &st.latest
    {
        announce(sh, tag);
    }
    st
}

/// Records a "new version" notice once per release; with automatic updates on, the timer acts instead.
fn announce(sh: &Shared, tag: &str) {
    let path = sh.cfg.data_dir.join("update.json");
    let mut saved: Saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if saved.notified.as_deref() == Some(tag) || sh.cfg.auto_update {
        return;
    }
    info!("vps-watcher {tag} is available (running {VERSION})");
    let ev = Event {
        key: "update".into(),
        state: State::Notice,
        severity: Severity::Info,
        title: format!("vps-watcher {tag} is available"),
        detail: format!(
            "You run {VERSION}. Upgrade with: sudo vps-watcher upgrade · Release notes: {RELEASES_PAGE}/tag/{tag}"
        ),
        ts: now(),
    };
    sh.alerts().record(ev.clone());
    if sh.notifier.has_channel() {
        let _ =
            sh.alert_tx.try_send(alert::Batch { events: vec![ev], ctx: alert::Context::from_snapshot(&sh.snapshot()) });
    }
    saved.notified = Some(tag.to_string());
    if let Ok(json) = serde_json::to_vec(&saved)
        && let Err(e) = atomic_write(&path, &json, true)
    {
        warn!("could not save update state: {e}");
    }
}

// ---------- `vps-watcher upgrade` ----------

/// Options of the upgrade command.
pub struct Opts {
    /// Only report whether an update exists.
    pub check: bool,
    /// Run by the daily timer: do nothing unless VPSW_AUTO_UPDATE=true, and report by alert.
    pub auto: bool,
}

/// Runs the upgrade command and returns the lines to print.
pub fn upgrade(cfg: &Config, opts: Opts) -> Result<Vec<String>, String> {
    if opts.auto && !cfg.auto_update {
        return Ok(vec!["Automatic updates are off (set VPSW_AUTO_UPDATE=true to turn them on).".into()]);
    }
    // SAFETY: geteuid has no preconditions.
    if !opts.check && unsafe { libc::geteuid() } != 0 {
        return Err("run it as root: sudo vps-watcher upgrade".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let src = Source::new(cfg.update_url.as_deref());
        let tag = match latest(&src).await {
            Ok(t) => t,
            // The timer tries again tomorrow; a network blip is not a failed update.
            Err(e) if opts.auto => return Ok(vec![format!("Could not check for updates: {e}")]),
            Err(e) => return Err(e),
        };
        if !is_release_tag(VERSION) {
            return Ok(vec![format!(
                "This is a development build ({VERSION}). The latest release is {tag}; install it with the installer."
            )]);
        }
        if !is_newer(&tag, VERSION) {
            return Ok(vec![format!("vps-watcher {VERSION} is up to date.")]);
        }
        if opts.check {
            return Ok(vec![format!(
                "vps-watcher {tag} is available (you run {VERSION}). Upgrade with: sudo vps-watcher upgrade"
            )]);
        }
        let result = install(&src, &tag).await;
        if opts.auto
            && let Err(e) = &result
            && e.contains(NOT_SIGNED)
        {
            // Releases are signed minutes after publishing; the timer tries again tomorrow.
            return Ok(vec![e.clone()]);
        }
        if opts.auto {
            report(cfg, &tag, &result).await;
        }
        result.map(|()| vec![format!("Upgraded vps-watcher from {VERSION} to {tag}.")])
    })
}

/// Sends the outcome of an automatic update to the alert channels.
async fn report(cfg: &Config, tag: &str, result: &Result<(), String>) {
    let n = alert::Notifier::new(
        &cfg.alerts,
        Some(&cfg.data_dir),
        false,
        &crate::collect::hostname(),
        cfg.domain.as_deref(),
    );
    if !n.has_channel() {
        return;
    }
    let (severity, title, detail) = match result {
        Ok(()) => {
            (Severity::Info, format!("vps-watcher updated to {tag}"), format!("Automatic update from {VERSION}."))
        }
        Err(e) => {
            (Severity::Warning, format!("Automatic update to {tag} failed"), format!("{e} · Still running {VERSION}."))
        }
    };
    n.notify(Event { key: "update".into(), state: State::Notice, severity, title, detail, ts: now() }).await;
}

/// One file an upgrade replaces.
struct Item {
    path: PathBuf,
    data: Vec<u8>,
    mode: u32,
    gid: Option<u32>,
}

/// What a file looked like before the upgrade: contents, mode and group; None if it did not exist.
type Before = Option<(Vec<u8>, u32, u32)>;

/// Downloads and verifies everything first, then swaps files and restarts, rolling back on failure.
async fn install(src: &Source, tag: &str) -> Result<(), String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        a => return Err(format!("no release build for this CPU ({a})")),
    };
    let m = fetch_manifest(src, tag).await?;
    let env_gid = std::fs::metadata(Path::new(CONF_DIR).join("vps-watcher.env")).ok().map(|m| {
        use std::os::unix::fs::MetadataExt;
        m.gid()
    });
    let mut items = vec![Item {
        path: BIN.into(),
        data: fetch_verified(src, &m, &format!("vps-watcher-{arch}-linux")).await?,
        mode: 0o755,
        gid: None,
    }];
    let extras = [
        ("vps-watcher.service", Path::new(UNIT_DIR).join("vps-watcher.service"), 0o644, None),
        ("vps-watcher-update.service", Path::new(UNIT_DIR).join("vps-watcher-update.service"), 0o644, None),
        ("vps-watcher-update.timer", Path::new(UNIT_DIR).join(TIMER), 0o644, None),
        ("vps-watcher.env", Path::new(CONF_DIR).join("vps-watcher.env.example"), 0o640, env_gid),
    ];
    for (name, path, mode, gid) in extras {
        // Older releases may lack some files; anything listed must verify.
        if m.files.contains_key(name) && path.parent().is_some_and(Path::exists) {
            items.push(Item { data: fetch_verified(src, &m, name).await?, path, mode, gid });
        }
    }
    let tag = tag.to_string();
    tokio::task::spawn_blocking(move || swap(&tag, &items)).await.map_err(|e| e.to_string())?
}

const TIMER: &str = "vps-watcher-update.timer";

/// Stages every file, trial-runs the binary, swaps them in, restarts, and restores everything
/// (files, modes and the timer) if anything fails or the service does not stay up.
fn swap(tag: &str, items: &[Item]) -> Result<(), String> {
    for it in items {
        safe_dir(it.path.parent().unwrap_or(Path::new("/")))?;
    }
    let mut staged: Vec<PathBuf> = Vec::new();
    let cleanup = |staged: &[PathBuf]| {
        for t in staged {
            let _ = std::fs::remove_file(t);
        }
    };
    for it in items {
        match stage(&it.path, &it.data, it.mode, it.gid) {
            Ok(t) => staged.push(t),
            Err(e) => {
                cleanup(&staged);
                return Err(e);
            }
        }
    }
    // The binary is items[0]: it must run and report the expected version before anything changes.
    if let Err(e) = trial_run(&staged[0], tag) {
        cleanup(&staged);
        return Err(e);
    }
    let before: Vec<Before> = items.iter().map(|it| snapshot(&it.path)).collect();
    let timer_existed = Path::new(UNIT_DIR).join(TIMER).exists();

    for (n, (tmp, it)) in staged.iter().zip(items).enumerate() {
        if let Err(e) = std::fs::rename(tmp, &it.path) {
            cleanup(&staged[n..]);
            restore(&items[..n], &before[..n]);
            systemctl(&["daemon-reload"]);
            return Err(format!("cannot replace {}: {e}; nothing was changed", it.path.display()));
        }
    }
    systemctl(&["daemon-reload"]);
    // Enable the timer only when this upgrade installed it; an admin's choice to disable it stays.
    let new_timer = !timer_existed && Path::new(UNIT_DIR).join(TIMER).exists();
    if new_timer {
        systemctl(&["enable", "--now", TIMER]);
    }
    systemctl(&["restart", "vps-watcher"]);
    if stays_up() {
        return Ok(());
    }

    warn!("{tag} did not stay up; rolling back");
    if new_timer {
        systemctl(&["disable", "--now", TIMER]);
    }
    restore(items, &before);
    systemctl(&["daemon-reload"]);
    systemctl(&["restart", "vps-watcher"]);
    Err(format!("{tag} did not stay up, so the previous version was restored. See: journalctl -u vps-watcher -n 50"))
}

/// Refuses folders that a non-root user could write to: a planted link there could redirect root's writes.
fn safe_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by users other than root; fix it with: sudo chown root {0} && sudo chmod go-w {0}",
            dir.display()
        ));
    }
    Ok(())
}

/// Writes `data` to a new, randomly named file next to `path` (never following links), with its mode and group.
fn stage(path: &Path, data: &[u8], mode: u32, gid: Option<u32>) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.new", crate::util::random_hex(6)));
    let err = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .mode(0o600)
        .open(&tmp)
        .map_err(err)?;
    let done = (|| {
        f.write_all(data)?;
        if let Some(g) = gid {
            std::os::unix::fs::fchown(&f, None, Some(g))?;
        }
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.sync_all()
    })();
    drop(f);
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(e));
    }
    Ok(tmp)
}

/// Runs `<binary> version` with a 10-second limit and checks it names `tag`.
fn trial_run(bin: &Path, tag: &str) -> Result<(), String> {
    let mut child = Command::new(bin)
        .arg("version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("the new binary does not run: {e}"))?;
    for _ in 0..100 {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if out.status.success() && String::from_utf8_lossy(&out.stdout).contains(tag) {
                return Ok(());
            }
            return Err("the new binary did not report the expected version".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("the new binary hung during its trial run".into())
}

/// The current state of a file, for rollback.
fn snapshot(path: &Path) -> Before {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).ok()?;
    Some((std::fs::read(path).ok()?, m.mode() & 0o7777, m.gid()))
}

/// Puts files back as they were: old contents with old modes, or removes files that were new.
fn restore(items: &[Item], before: &[Before]) {
    for (it, old) in items.iter().zip(before) {
        let ok = match old {
            Some((data, mode, gid)) => stage(&it.path, data, *mode, Some(*gid))
                .and_then(|t| std::fs::rename(&t, &it.path).map_err(|e| e.to_string())),
            None => std::fs::remove_file(&it.path).map_err(|e| e.to_string()),
        };
        if let Err(e) = ok {
            warn!("could not restore {}: {e}", it.path.display());
        }
    }
}

/// Runs systemctl, logging failures (the health check decides the outcome).
fn systemctl(args: &[&str]) -> bool {
    match Command::new("systemctl").args(args).output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            warn!("systemctl {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim());
            false
        }
        Err(e) => {
            warn!("systemctl {}: {e}", args.join(" "));
            false
        }
    }
}

/// True when the service is active and keeps the same process, without restarts, for about 20 seconds.
fn stays_up() -> bool {
    let state = || -> Option<(String, String, String)> {
        let out = Command::new("systemctl")
            .args(["show", "vps-watcher", "-p", "ActiveState", "-p", "MainPID", "-p", "NRestarts"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("").to_string();
        Some((get("ActiveState="), get("MainPID="), get("NRestarts=")))
    };
    // Give a crashing binary time to exit before trusting "active".
    std::thread::sleep(Duration::from_secs(3));
    let Some(first) = state().filter(|s| s.0 == "active" && s.1 != "0" && !s.1.is_empty()) else { return false };
    for _ in 0..10 {
        std::thread::sleep(Duration::from_secs(2));
        if state().as_ref() != Some(&first) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn tags_and_ordering() {
        assert!(is_release_tag("v26.11.21.01.38"));
        for bad in ["26.11.21.01.38", "v26.11.21.01", "v26.11.21-test", "0.1.0-dev", "v26.11.21.01.3x"] {
            assert!(!is_release_tag(bad), "{bad}");
        }
        assert!(is_newer("v26.11.22.00.00", "v26.11.21.23.59"));
        assert!(!is_newer("v26.11.21.01.38", "v26.11.21.01.38"));
        assert!(!is_newer("v26.11.20.00.00", "v26.11.21.00.00"), "never downgrade");
        assert!(!is_newer("v26.11.22.00.00", "0.1.0-dev"), "dev builds are left alone");
    }

    fn signed(text: &str) -> (Vec<u8>, String, Vec<u8>) {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let sig = crate::util::base64(kp.sign(text.as_bytes()).as_ref());
        (text.as_bytes().to_vec(), sig, kp.public_key().as_ref().to_vec())
    }

    #[test]
    fn manifest_must_be_signed_and_well_formed() {
        let h = "a".repeat(64);
        let text = format!("vps-watcher v26.11.21.01.38\n{h}  vps-watcher-x86_64-linux\n{h}  install.sh\n");
        let (bytes, sig, key) = signed(&text);
        let m = verify_manifest(&bytes, &sig, &key).unwrap();
        assert_eq!(m.tag, "v26.11.21.01.38");
        assert_eq!(m.files.len(), 2);

        let mut tampered = bytes.clone();
        tampered[30] ^= 1;
        assert!(verify_manifest(&tampered, &sig, &key).unwrap_err().contains("not valid"));
        assert!(verify_manifest(&bytes, &sig, &RELEASE_KEY).is_err(), "another key's signature is refused");
        assert!(verify_manifest(&bytes, "bm90IGEgc2ln", &key).is_err());

        let (b, s, k) = signed(&format!("vps-watcher v26.11.21.01.38\n{h}  ../../etc/passwd\n"));
        assert!(verify_manifest(&b, &s, &k).is_err(), "paths are refused even when signed");
    }

    #[test]
    fn release_key_is_a_valid_point() {
        // Verifying any message checks that the embedded key decodes as an Ed25519 public key.
        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &RELEASE_KEY);
        assert!(key.verify(b"x", &[0u8; 64]).is_err());
        assert_eq!(RELEASE_KEY.len(), 32);
    }

    #[test]
    fn redirect_targets() {
        let base = net::parse_url("https://github.com/o/r/releases/download/v1/x").unwrap();
        assert_eq!(resolve(&base, "https://cdn.example/a?b=1"), "https://cdn.example/a?b=1");
        assert_eq!(resolve(&base, "//cdn.example/a"), "https://cdn.example/a");
        assert_eq!(resolve(&base, "/z"), "https://github.com/z");
        assert_eq!(resolve(&base, "y"), "https://github.com/o/r/releases/download/v1/y");
    }

    #[test]
    fn staging_never_follows_links_and_restore_puts_files_back() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("unit.service");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let before = vec![snapshot(&target), snapshot(&d.path().join("new.timer"))];
        let t = stage(&target, b"new", 0o644, None).unwrap();
        assert_eq!(std::fs::metadata(&t).unwrap().permissions().mode() & 0o777, 0o644);
        std::fs::rename(&t, &target).unwrap();
        std::fs::write(d.path().join("new.timer"), "t").unwrap();
        let items = [
            Item { path: target.clone(), data: vec![], mode: 0o644, gid: None },
            Item { path: d.path().join("new.timer"), data: vec![], mode: 0o644, gid: None },
        ];
        restore(&items, &before);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o640, "old mode is back");
        assert!(!d.path().join("new.timer").exists(), "files the upgrade added are removed");
        assert!(std::fs::read_dir(d.path()).unwrap().count() == 1, "no temp files left");
    }

    #[tokio::test]
    async fn follows_redirects_and_reads_latest() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            for reply in [
                "HTTP/1.1 302 Found\r\nlocation: /real\r\ncontent-length: 0\r\n\r\n".to_string(),
                "HTTP/1.1 200 OK\r\ncontent-length: 31\r\n\r\n{\"tag_name\":\"v26.11.22.00.00\"}\n".to_string(),
            ] {
                let (mut s, _) = l.accept().await.unwrap();
                let mut b = [0u8; 2048];
                let _ = s.read(&mut b).await;
                s.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let src = Source::new(Some(&format!("http://127.0.0.1:{port}")));
        assert_eq!(latest(&src).await.unwrap(), "v26.11.22.00.00");
        assert_eq!(src.file("v1", "x"), format!("http://127.0.0.1:{port}/releases/download/v1/x"));
    }
}
