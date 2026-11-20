//! The collector: samples host, apps and monitors every interval and keeps history.

pub mod cgroup;
pub mod checks;
pub mod docker;
pub mod host;
pub mod logs;
pub mod ports;
pub mod procs;
pub mod systemd;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{mpsc, watch};

use crate::alert::{self, Engine as AlertEngine, Notifier};
use crate::config::{AuthMode, Config};
use crate::store::{self, History};
use crate::util::now;
use crate::{info, warn};
use cgroup::{CgStats, Cgroups};
use checks::{CheckResult, MonitorKind, MonitorStore};
use docker::{ContainerInfo, ContainerSummary, Docker, PortMap};
use host::{CpuTimes, NetBytes};

const DOCKER_EVERY: Duration = Duration::from_secs(15);
const INSPECT_EVERY: Duration = Duration::from_secs(60);
const SERVICES_EVERY: Duration = Duration::from_secs(30);
const CHECKS_EVERY: Duration = Duration::from_secs(30);
const SAVE_EVERY: Duration = Duration::from_secs(300);

/// State shared between the collector and the web server.
pub struct Shared {
    pub cfg: Config,
    pub snapshot: RwLock<Arc<Snapshot>>,
    pub history: Mutex<History>,
    pub monitors: Mutex<MonitorStore>,
    pub check_results: Mutex<HashMap<String, CheckResult>>,
    pub docker: Docker,
    pub log_files: Mutex<Vec<PathBuf>>,
    /// Bumped after every sample so live views can push updates.
    pub tick: watch::Sender<u64>,
    /// The latest snapshot as JSON for live views: (admin, public). Built once per sample, only when watched.
    pub live: RwLock<(Arc<str>, Arc<str>)>,
    pub alerts: Mutex<AlertEngine>,
    pub notifier: Arc<Notifier>,
    /// Events for the delivery task; full means delivery is stuck, so new events are dropped.
    pub alert_tx: mpsc::Sender<alert::Batch>,
}

impl Shared {
    /// Latest snapshot (cheap: clones an Arc).
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// Locks history, recovering from a poisoned lock (a panic elsewhere must not stop monitoring).
    pub fn history(&self) -> std::sync::MutexGuard<'_, History> {
        self.history.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Locks the alert engine.
    pub fn alerts(&self) -> std::sync::MutexGuard<'_, AlertEngine> {
        self.alerts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Locks the monitor store.
    pub fn monitors(&self) -> std::sync::MutexGuard<'_, MonitorStore> {
        self.monitors.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Path of the history file.
    pub fn history_path(&self) -> PathBuf {
        self.cfg.data_dir.join("history.bin")
    }

    /// JSON for a live view; anonymous viewers get the redacted copy.
    pub fn live_json(&self, admin: bool) -> Arc<str> {
        let l = self.live.read().unwrap_or_else(|e| e.into_inner());
        if admin { l.0.clone() } else { l.1.clone() }
    }

    /// Writes history to disk now, logging (not failing) on error. Used at shutdown.
    pub fn save_history(&self) {
        let buf = store::encode(&self.history());
        if let Err(e) = store::write(&buf, &self.history_path()) {
            warn!("could not save history: {e}");
        }
    }

    /// Encodes under the lock, then writes and fsyncs on a blocking thread so slow disks stall nothing.
    pub fn save_history_background(&self) {
        let buf = store::encode(&self.history());
        let path = self.history_path();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store::write(&buf, &path) {
                warn!("could not save history: {e}");
            }
        });
    }
}

/// Everything the dashboard shows at one moment.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub ts: u64,
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub uptime: u64,
    pub cores: usize,
    pub interval: u64,
    pub cpu: host::CpuPct,
    pub per_core: Vec<f32>,
    pub load: [f32; 3],
    pub mem: Mem,
    pub disks: Vec<Disk>,
    pub disk_io: Rates,
    pub net: Rates,
    pub ifaces: Vec<Iface>,
    pub apps: Vec<App>,
    pub monitors: Vec<MonitorStatus>,
    pub docker: DockerStatus,
    pub cgroup_v2: bool,
    /// vps-watcher's own footprint, so users can see it stays small.
    pub own: OwnUsage,
    /// Alerts firing right now.
    pub alerts: Vec<alert::Active>,
}

impl Snapshot {
    /// Copy for anonymous viewers: monitor URLs, hosts and paths are reduced to harmless labels.
    pub fn public(&self) -> Snapshot {
        let mut s = self.clone();
        for m in &mut s.monitors {
            m.target = redact_target(&m.kind, &m.target);
            if m.kind == "logfile" {
                m.detail.clear();
            }
        }
        for a in &mut s.alerts {
            if !self.alert_detail_is_public(&a.key) {
                a.detail.clear();
            }
        }
        s
    }

    /// Monitor alert details are public only for monitors whose details are; log-file and deleted
    /// monitors could reveal paths.
    pub fn alert_detail_is_public(&self, key: &str) -> bool {
        match key.strip_prefix("mon:") {
            None => true,
            Some(id) => self.monitors.iter().any(|m| m.id == id && m.kind != "logfile"),
        }
    }
}

/// Keeps only what is safe to show publicly: scheme and host for URLs, file name for paths, port for TCP.
fn redact_target(kind: &str, target: &str) -> String {
    match kind {
        "http" => target
            .split_once("://")
            .map(|(scheme, rest)| {
                let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                format!("{scheme}://{}", host.rsplit('@').next().unwrap_or(""))
            })
            .unwrap_or_default(),
        "logfile" => {
            std::path::Path::new(target).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
        }
        "tcp" => format!("port {}", target.rsplit(':').next().unwrap_or("")),
        _ => target.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Mem {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Disk {
    pub mount: String,
    pub device: String,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
    pub avail: u64,
    /// Used percent as `df` shows it: blocks reserved for root count as unavailable.
    pub pct: f32,
    pub inodes_pct: f32,
}

/// Bytes per second in two directions (rx/tx or read/write).
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Rates {
    pub a: f64,
    pub b: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Iface {
    pub name: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total: u64,
    pub tx_total: u64,
}

/// A container or service.
#[derive(Debug, Clone, Default, Serialize)]
pub struct App {
    pub id: String,
    pub kind: &'static str,
    pub name: String,
    pub group: Option<String>,
    pub image: Option<String>,
    pub state: String,
    pub status: String,
    pub health: Option<String>,
    pub cpu_pct: Option<f32>,
    pub mem: Option<u64>,
    pub mem_limit: Option<u64>,
    pub net: Option<Rates>,
    pub io: Option<Rates>,
    pub pids: Option<u64>,
    pub restarts: Option<u64>,
    pub oom_killed: bool,
    /// Out-of-memory kills inside the app since its cgroup was created.
    #[serde(skip)]
    pub oom_kills: Option<u64>,
    pub ports: Vec<PortMap>,
    pub system: bool,
    #[serde(skip)]
    pub container_id: Option<String>,
    #[serde(skip)]
    pub tty: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MonitorStatus {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub up: Option<bool>,
    pub ms: Option<f32>,
    pub detail: String,
    pub cpu_pct: Option<f32>,
    pub mem: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DockerStatus {
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct OwnUsage {
    pub rss: u64,
    pub cpu_pct: f32,
}

/// Cached container details plus where its cgroup lives.
struct Known {
    info: ContainerInfo,
    cgroup: Option<String>,
    state: String,
    at: Instant,
}

/// Previous counters needed to turn totals into rates.
#[derive(Default)]
struct Prev {
    cpu: Option<CpuTimes>,
    cores: Vec<CpuTimes>,
    net: HashMap<String, NetBytes>,
    disk: HashMap<String, (u64, u64)>,
    /// Per app: counters, network counters and the container id they came from.
    apps: HashMap<String, AppPrev>,
    procs: HashMap<u32, procs::ProcStat>,
    own_ticks: u64,
}

/// Previous counters of one app; `cid` detects a container recreated under the same name.
type AppPrev = (CgStats, Option<NetBytes>, Option<String>);

/// Mutable collector state between ticks.
struct Collector {
    cg: Cgroups,
    prev: Prev,
    last: Instant,
    containers: Vec<ContainerSummary>,
    known: HashMap<String, Known>,
    docker_err: Option<String>,
    docker_at: Option<Instant>,
    services: Vec<String>,
    failed: Vec<String>,
    mounts: Vec<host::Mount>,
    services_at: Option<Instant>,
    saved_at: Instant,
    static_info: (String, String, String),
}

/// Loads saved state and builds the shared handle, plus the queue the alert sender reads.
pub fn init(cfg: Config) -> Result<(Arc<Shared>, mpsc::Receiver<alert::Batch>), String> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| format!("cannot create data dir {}: {e}", cfg.data_dir.display()))?;
    let history = match store::load(&cfg.data_dir.join("history.bin")) {
        Ok(h) => h,
        Err(e) => {
            warn!("{e}");
            History::default()
        }
    };
    info!("loaded {} history series", history.len());
    let monitors = MonitorStore::load(&cfg.data_dir)?;
    let (tick, _) = watch::channel(0);
    let alerts = AlertEngine::load(cfg.alerts.clone(), &cfg.data_dir);
    let notifier = Arc::new(Notifier::new(&cfg.alerts, Some(&cfg.data_dir), true, &hostname(), cfg.domain.as_deref()));
    let (alert_tx, alert_rx) = mpsc::channel(64);
    let sh = Arc::new(Shared {
        docker: Docker::new(&cfg.docker_socket),
        cfg,
        snapshot: RwLock::default(),
        history: Mutex::new(history),
        monitors: Mutex::new(monitors),
        check_results: Mutex::default(),
        log_files: Mutex::default(),
        tick,
        live: RwLock::new((Arc::from(""), Arc::from(""))),
        alerts: Mutex::new(alerts),
        notifier,
        alert_tx,
    });
    Ok((sh, alert_rx))
}

/// This machine's host name.
pub fn hostname() -> String {
    host::read("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string()
}

/// Samples forever at the configured interval.
pub async fn run(shared: Arc<Shared>) {
    let mut c = Collector {
        cg: Cgroups::detect("/sys/fs/cgroup"),
        prev: Prev::default(),
        last: Instant::now(),
        containers: Vec::new(),
        known: HashMap::new(),
        docker_err: None,
        docker_at: None,
        services: Vec::new(),
        failed: Vec::new(),
        mounts: Vec::new(),
        services_at: None,
        saved_at: Instant::now(),
        static_info: static_info(),
    };
    let mut ticker = tokio::time::interval(shared.cfg.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut n = 0u64;
    loop {
        ticker.tick().await;
        c.tick(&shared).await;
        n += 1;
        shared.tick.send_replace(n);
    }
}

/// Runs TCP, HTTP and log-file monitors every 30 seconds, apart from sampling.
pub async fn run_checks(shared: Arc<Shared>) {
    let mut ticker = tokio::time::interval(CHECKS_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let list = shared.monitors().list().to_vec();
        let mut set = tokio::task::JoinSet::new();
        for m in list {
            set.spawn(async move {
                let r = match &m.kind {
                    MonitorKind::Tcp { host, port } => checks::check_tcp(host, *port).await,
                    MonitorKind::Http { url, expect_status } => checks::check_http(url, *expect_status).await,
                    MonitorKind::Logfile { path } => checks::check_logfile(path),
                    MonitorKind::Process { .. } => return None,
                };
                Some((m.id, r))
            });
        }
        let ts = now();
        while let Some(res) = set.join_next().await {
            let Ok(Some((id, r))) = res else { continue };
            {
                let mut h = shared.history();
                h.record(&format!("mon:{id}.up"), ts, if r.up { 100.0 } else { 0.0 });
                if r.up {
                    h.record(&format!("mon:{id}.ms"), ts, r.ms as f64);
                }
            }
            shared.check_results.lock().unwrap_or_else(|e| e.into_inner()).insert(id, r);
        }
    }
}

/// Reads hostname, OS name and kernel version once.
fn static_info() -> (String, String, String) {
    let hostname = hostname();
    let kernel = host::read("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
    let os = host::read("/etc/os-release")
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
        .unwrap_or_else(|| "Linux".into());
    (hostname, os, kernel)
}

/// Per-second rate from a counter delta; None when the counter reset.
fn rate(prev: Option<u64>, cur: u64, secs: f64) -> Option<f64> {
    host::delta(prev?, cur).map(|d| d as f64 / secs)
}

impl Collector {
    /// One sampling pass.
    async fn tick(&mut self, sh: &Shared) {
        let ts = now();
        let secs = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();

        if self.docker_at.is_none_or(|t| t.elapsed() >= DOCKER_EVERY) {
            self.refresh_docker(sh).await;
        }
        if self.services_at.is_none_or(|t| t.elapsed() >= SERVICES_EVERY) {
            self.refresh_services(sh).await;
        }

        let mut snap = Snapshot {
            ts,
            hostname: self.static_info.0.clone(),
            os: self.static_info.1.clone(),
            kernel: self.static_info.2.clone(),
            cores: procs::cores(),
            interval: sh.cfg.interval.as_secs(),
            cgroup_v2: self.cg.is_v2(),
            docker: DockerStatus {
                available: self.docker_err.is_none() && sh.docker.present(),
                error: self.docker_err.clone(),
            },
            ..Default::default()
        };
        self.sample_host(&mut snap, secs);
        self.sample_apps(&mut snap, secs);
        self.sample_monitors(sh, &mut snap, secs);
        snap.own = self.own_usage(secs);
        self.record(sh, &snap);
        check_alerts(sh, &mut snap);

        let snap = Arc::new(snap);
        if sh.tick.receiver_count() > 0 {
            let admin: Arc<str> = serde_json::to_string(&*snap).unwrap_or_default().into();
            let public: Arc<str> = if sh.cfg.auth == AuthMode::Open {
                serde_json::to_string(&snap.public()).unwrap_or_default().into()
            } else {
                Arc::from("")
            };
            *sh.live.write().unwrap_or_else(|e| e.into_inner()) = (admin, public);
        }
        if let Ok(mut s) = sh.snapshot.write() {
            *s = snap;
        }
        if self.saved_at.elapsed() >= SAVE_EVERY {
            self.saved_at = Instant::now();
            sh.history().prune(ts);
            sh.save_history_background();
        }
    }

    /// Re-lists containers and re-inspects new, changed or stale ones.
    async fn refresh_docker(&mut self, sh: &Shared) {
        self.docker_at = Some(Instant::now());
        if !sh.docker.present() {
            self.containers.clear();
            self.docker_err = None;
            return;
        }
        match sh.docker.list().await {
            Ok(list) => {
                self.docker_err = None;
                for c in list.iter().filter(|c| c.state == "running") {
                    let stale =
                        self.known.get(&c.id).is_none_or(|k| k.state != c.state || k.at.elapsed() >= INSPECT_EVERY);
                    if !stale {
                        continue;
                    }
                    if let Ok(info) = sh.docker.inspect(&c.id).await {
                        let cgroup = (info.pid > 0).then(|| cgroup::path_of_pid(info.pid)).flatten();
                        self.known
                            .insert(c.id.clone(), Known { info, cgroup, state: c.state.clone(), at: Instant::now() });
                    }
                }
                self.known.retain(|id, _| list.iter().any(|c| &c.id == id && c.state == "running"));
                self.containers = list;
            }
            Err(e) => {
                if self.docker_err.as_ref() != Some(&e) {
                    warn!("docker: {e}");
                }
                self.docker_err = Some(e);
                self.containers.clear();
            }
        }
    }

    /// Re-discovers services, failed units, filesystems and log files.
    async fn refresh_services(&mut self, sh: &Shared) {
        self.services_at = Some(Instant::now());
        self.services = self.cg.running_services();
        // A slow or missing systemctl keeps the previous list, so failed-unit alerts neither end nor repeat.
        if let Some(failed) = systemd::failed_units().await {
            self.failed = failed;
        }
        self.mounts = host::read("/proc/self/mountinfo").map(|t| host::parse_mountinfo(&t)).unwrap_or_default();
        let files = logs::discover_files(std::path::Path::new("/var/log"));
        *sh.log_files.lock().unwrap_or_else(|e| e.into_inner()) = files;
    }

    /// CPU, memory, load, disks and network for the whole host.
    fn sample_host(&mut self, s: &mut Snapshot, secs: f64) {
        if let Some((cpu, cores)) = host::read("/proc/stat").and_then(|t| host::parse_stat(&t)) {
            if let Some(p) = self.prev.cpu.and_then(|prev| host::cpu_pct(&prev, &cpu)) {
                s.cpu = p;
            }
            s.per_core = cores
                .iter()
                .zip(self.prev.cores.iter())
                .map(|(c, p)| host::cpu_pct(p, c).map_or(0.0, |x| x.total))
                .collect();
            self.prev.cpu = Some(cpu);
            self.prev.cores = cores;
        }
        if let Some(m) = host::read("/proc/meminfo").and_then(|t| host::parse_meminfo(&t)) {
            s.mem = Mem {
                total: m.total,
                used: m.total.saturating_sub(m.available),
                available: m.available,
                cached: m.cached,
                swap_total: m.swap_total,
                swap_used: m.swap_total.saturating_sub(m.swap_free),
            };
        }
        s.load = host::read("/proc/loadavg").and_then(|t| host::parse_loadavg(&t)).unwrap_or_default();
        s.uptime = host::read("/proc/uptime").and_then(|t| host::parse_uptime(&t)).unwrap_or(0);

        s.disks = self
            .mounts
            .iter()
            .filter_map(|m| {
                let u = host::statvfs(&m.path)?;
                (u.total > 0).then(|| Disk {
                    mount: m.path.clone(),
                    device: m.device.clone(),
                    fstype: m.fstype.clone(),
                    total: u.total,
                    used: u.used,
                    avail: u.avail,
                    pct: if u.used + u.avail > 0 { u.used as f32 * 100.0 / (u.used + u.avail) as f32 } else { 0.0 },
                    inodes_pct: if u.inodes_total > 0 {
                        u.inodes_used as f32 * 100.0 / u.inodes_total as f32
                    } else {
                        0.0
                    },
                })
            })
            .collect();

        let disks = host::read("/proc/diskstats").map(|t| host::parse_diskstats(&t)).unwrap_or_default();
        let (mut r, mut w) = (0.0, 0.0);
        let mut next = HashMap::new();
        for (name, rs, ws) in disks.into_iter().filter(|(n, _, _)| host::is_physical_disk(n)) {
            let prev = self.prev.disk.get(&name);
            r += rate(prev.map(|p| p.0), rs, secs).unwrap_or(0.0) * 512.0;
            w += rate(prev.map(|p| p.1), ws, secs).unwrap_or(0.0) * 512.0;
            next.insert(name, (rs, ws));
        }
        s.disk_io = Rates { a: r, b: w };
        self.prev.disk = next;

        let nets = host::read("/proc/net/dev").map(|t| host::parse_net_dev(&t)).unwrap_or_default();
        let mut next = HashMap::new();
        for (name, b) in nets.into_iter().filter(|(n, _)| host::is_physical_iface(n)) {
            let prev = self.prev.net.get(&name);
            let i = Iface {
                name: name.clone(),
                rx_bps: rate(prev.map(|p| p.rx), b.rx, secs).unwrap_or(0.0),
                tx_bps: rate(prev.map(|p| p.tx), b.tx, secs).unwrap_or(0.0),
                rx_total: b.rx,
                tx_total: b.tx,
            };
            s.net.a += i.rx_bps;
            s.net.b += i.tx_bps;
            s.ifaces.push(i);
            next.insert(name, b);
        }
        self.prev.net = next;
    }

    /// Containers and services with their cgroup usage.
    fn sample_apps(&mut self, s: &mut Snapshot, secs: f64) {
        let cores = s.cores.max(1) as f64;
        let mut seen = HashMap::new();
        let mut apps = Vec::new();

        for c in &self.containers {
            let name = c.name();
            let mut app = App {
                id: format!("c:{name}"),
                kind: "container",
                name,
                group: c.project(),
                image: Some(c.image.clone()),
                state: c.state.clone(),
                status: c.status.clone(),
                ports: c.ports.clone(),
                container_id: Some(c.id.clone()),
                ..Default::default()
            };
            if let Some(k) = self.known.get(&c.id) {
                app.health = k.info.health.clone();
                app.restarts = Some(k.info.restarts);
                app.oom_killed = k.info.oom_killed;
                app.tty = k.info.tty;
                let cg = k.cgroup.clone().or_else(|| self.container_cgroup_guess(&c.id));
                let stats = cg.as_deref().and_then(|p| self.cg.stats(p));
                let net = (!k.info.host_network && k.info.pid > 0).then(|| container_net(k.info.pid)).flatten();
                if let Some(st) = stats {
                    self.fill_usage(&mut app, st, net, secs, cores, &mut seen);
                }
            }
            apps.push(app);
        }

        let container_scopes: Vec<String> = self.containers.iter().map(|c| format!("docker-{}.scope", c.id)).collect();
        for unit in &self.services {
            if container_scopes.iter().any(|s| s == unit) {
                continue;
            }
            // Same relative path in both layouts: v1 resolves it under each controller.
            let rel = format!("system.slice/{unit}");
            let mut app = App {
                id: format!("s:{unit}"),
                kind: "service",
                name: unit.trim_end_matches(".service").to_string(),
                state: "running".into(),
                status: "active".into(),
                system: systemd::is_system_unit(unit),
                ..Default::default()
            };
            if let Some(st) = self.cg.stats(&rel) {
                self.fill_usage(&mut app, st, None, secs, cores, &mut seen);
            }
            apps.push(app);
        }
        for unit in &self.failed {
            apps.push(App {
                id: format!("s:{unit}"),
                kind: "service",
                name: unit.trim_end_matches(".service").to_string(),
                state: "failed".into(),
                status: "failed".into(),
                system: systemd::is_system_unit(unit),
                ..Default::default()
            });
        }
        self.prev.apps = seen;
        s.apps = apps;
    }

    /// Fills usage fields from cgroup counters, turning totals into rates.
    fn fill_usage(
        &self,
        app: &mut App,
        st: CgStats,
        net: Option<NetBytes>,
        secs: f64,
        cores: f64,
        seen: &mut HashMap<String, AppPrev>,
    ) {
        // A recreated container has fresh counters; never diff across two different containers.
        let prev = self.prev.apps.get(&app.id).filter(|p| p.2 == app.container_id);
        app.mem = Some(st.mem);
        app.oom_kills = st.oom_kills;
        app.mem_limit = st.mem_limit;
        app.pids = Some(st.pids);
        if let Some((p, pn, _)) = prev {
            app.cpu_pct = rate(Some(p.cpu_usec), st.cpu_usec, secs).map(|us| (us / 1e6 / cores * 100.0) as f32);
            if let (Some(r), Some(w)) =
                (rate(Some(p.io_read), st.io_read, secs), rate(Some(p.io_write), st.io_write, secs))
            {
                app.io = Some(Rates { a: r, b: w });
            }
            if let (Some(n), Some(pn)) = (net, pn)
                && let (Some(rx), Some(tx)) = (rate(Some(pn.rx), n.rx, secs), rate(Some(pn.tx), n.tx, secs))
            {
                app.net = Some(Rates { a: rx, b: tx });
            }
        }
        seen.insert(app.id.clone(), (st, net, app.container_id.clone()));
    }

    /// Common cgroup locations of a container when /proc/<pid>/cgroup was unreadable.
    fn container_cgroup_guess(&self, id: &str) -> Option<String> {
        [format!("system.slice/docker-{id}.scope"), format!("docker/{id}")]
            .into_iter()
            .find(|p| self.cg.stats(p).is_some())
    }

    /// Statuses for every monitor; process monitors are measured here each tick.
    fn sample_monitors(&mut self, sh: &Shared, s: &mut Snapshot, secs: f64) {
        let list = sh.monitors().list().to_vec();
        let results = sh.check_results.lock().map(|r| r.clone()).unwrap_or_default();
        let want_procs = list.iter().any(|m| matches!(m.kind, MonitorKind::Process { .. }));
        let procs_now = if want_procs { procs::snapshot() } else { HashMap::new() };
        let cpu = if want_procs { procs::cpu_between(&self.prev.procs, &procs_now, secs) } else { HashMap::new() };
        let page = procs::page_size();
        // Read each command line once per sample, however many process monitors there are.
        let haystacks: Vec<(u32, String)> = procs_now
            .iter()
            .filter(|(pid, _)| **pid != std::process::id())
            .map(|(pid, p)| (*pid, format!("{}\0{}", p.name, procs::cmdline(*pid).unwrap_or_default()).to_lowercase()))
            .collect();

        for m in list {
            let (kind, target) = match &m.kind {
                MonitorKind::Tcp { host, port } => ("tcp", format!("{host}:{port}")),
                MonitorKind::Http { url, .. } => ("http", url.clone()),
                MonitorKind::Process { pattern } => ("process", pattern.clone()),
                MonitorKind::Logfile { path } => ("logfile", path.clone()),
            };
            let mut st = MonitorStatus {
                id: m.id.clone(),
                name: m.name.clone(),
                kind: kind.into(),
                target,
                ..Default::default()
            };
            if let MonitorKind::Process { pattern } = &m.kind {
                let pat = pattern.to_lowercase();
                let matched: Vec<u32> =
                    haystacks.iter().filter(|(_, hay)| hay.contains(&pat)).map(|(pid, _)| *pid).collect();
                st.up = Some(!matched.is_empty());
                st.cpu_pct = Some(matched.iter().filter_map(|p| cpu.get(p)).sum());
                st.mem = Some(matched.iter().filter_map(|p| procs_now.get(p)).map(|p| p.rss_pages * page).sum());
                st.detail = format!("{} process{}", matched.len(), if matched.len() == 1 { "" } else { "es" });
            } else if let Some(r) = results.get(&m.id) {
                st.up = Some(r.up);
                st.ms = Some(r.ms);
                st.detail = r.detail.clone();
            } else {
                st.detail = "waiting for first check".into();
            }
            s.monitors.push(st);
        }
        self.prev.procs = procs_now;
    }

    /// vps-watcher's own resident memory and CPU.
    fn own_usage(&mut self, secs: f64) -> OwnUsage {
        let Some(stat) = host::read("/proc/self/stat").and_then(|t| procs::parse_stat(&t)) else {
            return OwnUsage::default();
        };
        let d = stat.cpu_ticks.saturating_sub(self.prev.own_ticks);
        let first = self.prev.own_ticks == 0;
        self.prev.own_ticks = stat.cpu_ticks;
        OwnUsage {
            rss: stat.rss_pages * procs::page_size(),
            cpu_pct: if first { 0.0 } else { (d as f64 / procs::clock_ticks() as f64 / secs * 100.0) as f32 },
        }
    }

    /// Writes this tick's values into history.
    fn record(&self, sh: &Shared, s: &Snapshot) {
        let mut h = sh.history();
        let ts = s.ts;
        if self.prev.cpu.is_some() {
            h.record("host.cpu", ts, s.cpu.total as f64);
            h.record("host.cpu.steal", ts, s.cpu.steal as f64);
            h.record("host.cpu.iowait", ts, s.cpu.iowait as f64);
        }
        if s.mem.total > 0 {
            h.record("host.mem", ts, s.mem.used as f64 * 100.0 / s.mem.total as f64);
            h.record("host.mem.used", ts, s.mem.used as f64);
        }
        if s.mem.swap_total > 0 {
            h.record("host.swap", ts, s.mem.swap_used as f64 * 100.0 / s.mem.swap_total as f64);
        }
        h.record("host.load1", ts, s.load[0] as f64);
        h.record("host.net.rx", ts, s.net.a);
        h.record("host.net.tx", ts, s.net.b);
        h.record("host.disk.read", ts, s.disk_io.a);
        h.record("host.disk.write", ts, s.disk_io.b);
        for d in &s.disks {
            h.record(&format!("disk:{}.used", d.mount), ts, d.pct as f64);
        }
        for a in s.apps.iter().filter(|a| a.state == "running") {
            if let Some(v) = a.cpu_pct {
                h.record(&format!("app:{}.cpu", a.id), ts, v as f64);
            }
            if let Some(v) = a.mem {
                h.record(&format!("app:{}.mem", a.id), ts, v as f64);
            }
            if let Some(n) = a.net {
                h.record(&format!("app:{}.net.rx", a.id), ts, n.a);
                h.record(&format!("app:{}.net.tx", a.id), ts, n.b);
            }
            if let Some(io) = a.io {
                h.record(&format!("app:{}.io.r", a.id), ts, io.a);
                h.record(&format!("app:{}.io.w", a.id), ts, io.b);
            }
        }
        for m in s.monitors.iter().filter(|m| m.kind == "process") {
            h.record(&format!("mon:{}.up", m.id), ts, if m.up == Some(true) { 100.0 } else { 0.0 });
            if let Some(c) = m.cpu_pct {
                h.record(&format!("mon:{}.cpu", m.id), ts, c as f64);
            }
            if let Some(mem) = m.mem {
                h.record(&format!("mon:{}.mem", m.id), ts, mem as f64);
            }
        }
    }
}

/// Emails a usage report every VPSW_REPORT period; the last send time survives restarts.
pub async fn run_reports(sh: Arc<Shared>) {
    let Some(days) = sh.cfg.alerts.report_days else { return };
    if sh.cfg.alerts.email.is_none() {
        warn!("VPSW_REPORT is set, but email is not configured; no reports will be sent");
        return;
    }
    let path = sh.cfg.data_dir.join("report-sent");
    let period = days * 86_400;
    // The first report goes out one full period after setup, not at every restart.
    let mut last = match std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(t) => t,
        None => {
            let _ = crate::util::atomic_write(&path, now().to_string().as_bytes(), true);
            now()
        }
    };
    let (range, label) = if days == 1 { (store::Range::H24, "24h") } else { (store::Range::D7, "7d") };
    let mut ticker = tokio::time::interval(Duration::from_secs(600));
    loop {
        ticker.tick().await;
        let t = now();
        if t.saturating_sub(last) < period {
            continue;
        }
        let r = crate::report::build(&sh.history(), &sh.snapshot(), range, label, t);
        match sh.notifier.send_report(&r).await {
            Ok(()) => {
                info!("emailed the {label} report");
                last = t;
            }
            Err(e) => {
                warn!("could not email the {label} report: {e}; retrying in an hour");
                last = t.saturating_sub(period) + 3600;
            }
        }
        if let Err(e) = crate::util::atomic_write(&path, last.to_string().as_bytes(), true) {
            warn!("could not save report time: {e}");
        }
    }
}

/// Writes alert state off the runtime. Writes run one at a time and an older state never
/// overwrites a newer one, even when a slow disk makes saves pile up.
fn save_alert_state(path: PathBuf, bytes: Vec<u8>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static WRITTEN: Mutex<u64> = Mutex::new(0);
    let seq = NEXT.fetch_add(1, Ordering::SeqCst);
    tokio::task::spawn_blocking(move || {
        let mut last = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        if seq < *last {
            return;
        }
        match crate::util::atomic_write(&path, &bytes, true) {
            Ok(()) => *last = seq,
            Err(e) => warn!("could not save alert state: {e}"),
        }
    });
}

/// Runs the alert rules on this sample, saves alert state when it changed and queues new events.
fn check_alerts(sh: &Shared, snap: &mut Snapshot) {
    let events = {
        let mut engine = sh.alerts();
        let events = engine.evaluate(snap);
        snap.alerts = engine.active();
        if let Some((path, bytes)) = engine.take_save() {
            save_alert_state(path, bytes);
        }
        events
    };
    for e in &events {
        info!("alert {}: {} ({})", e.state.label(), e.title, e.detail);
    }
    if !events.is_empty()
        && sh.notifier.has_channel()
        && sh.alert_tx.try_send(alert::Batch { events, ctx: alert::Context::from_snapshot(snap) }).is_err()
    {
        warn!("alert delivery is backed up; dropped new events");
    }
}

/// Network counters inside a container's namespace, summed over its interfaces.
fn container_net(pid: u32) -> Option<NetBytes> {
    let text = host::read(format!("/proc/{pid}/net/dev"))?;
    let mut sum = NetBytes::default();
    for (name, b) in host::parse_net_dev(&text) {
        if name != "lo" {
            sum.rx += b.rx;
            sum.tx += b.tx;
        }
    }
    Some(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_view_hides_targets() {
        assert_eq!(
            redact_target("http", "https://user:pw@api.example.com:8443/health?token=abc"),
            "https://api.example.com:8443"
        );
        assert_eq!(redact_target("logfile", "/srv/app/secret/app.log"), "app.log");
        assert_eq!(redact_target("tcp", "10.0.0.5:5432"), "port 5432");
        assert_eq!(redact_target("process", "node"), "node");
    }
}
