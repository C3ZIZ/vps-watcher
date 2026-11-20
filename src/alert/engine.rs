//! Turns snapshots into alert events: each condition must hold for a while before it fires or clears.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::settings::AlertConfig;
use crate::collect::Snapshot;
use crate::util::fmt_age;

/// Events kept for the Alerts page.
const HISTORY: usize = 200;
/// One-shot notices (restarts, OOM kills) for the same app are sent at most this often.
const NOTICE_GAP: u64 = 15 * 60;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// What happened to a condition.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Firing,
    Resolved,
    /// A one-off fact, such as a restart, that has no "resolved" step.
    Notice,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Firing => "ALERT",
            State::Resolved => "RESOLVED",
            State::Notice => "NOTICE",
        }
    }
}

/// One notification-worthy change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub key: String,
    pub state: State,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub ts: u64,
}

/// A condition that is firing right now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Active {
    pub key: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub since: u64,
}

/// One condition to test this sample. `bad: None` means unknown, so nothing changes.
struct Check {
    key: String,
    bad: Option<bool>,
    fire_after: u64,
    clear_after: u64,
    severity: Severity,
    fire: (String, String),
    clear: (String, String),
}

/// What survives a restart, so a firing alert is not sent twice.
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    active: Vec<Active>,
    history: Vec<Event>,
    running: Vec<String>,
}

/// Alert state between samples.
pub struct Engine {
    cfg: AlertConfig,
    path: PathBuf,
    active: BTreeMap<String, Active>,
    /// When each condition started to disagree with its firing state.
    pending: HashMap<String, u64>,
    history: VecDeque<Event>,
    /// Containers seen running: only these alert when they stop.
    running: HashSet<String>,
    restarts: HashMap<String, u64>,
    ooms: HashMap<String, u64>,
    last_notice: HashMap<String, u64>,
    dirty: bool,
}

impl Engine {
    /// Loads saved state from `dir/alerts.json`; a missing or broken file starts fresh.
    pub fn load(cfg: AlertConfig, dir: &Path) -> Self {
        let path = dir.join("alerts.json");
        let saved: Saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Engine {
            cfg,
            path,
            active: saved.active.into_iter().map(|a| (a.key.clone(), a)).collect(),
            pending: HashMap::new(),
            history: saved.history.into_iter().rev().take(HISTORY).rev().collect(),
            running: saved.running.into_iter().collect(),
            restarts: HashMap::new(),
            ooms: HashMap::new(),
            last_notice: HashMap::new(),
            dirty: false,
        }
    }

    /// Alert rules in effect.
    pub fn config(&self) -> &AlertConfig {
        &self.cfg
    }

    /// Firing alerts, worst first, then oldest first.
    pub fn active(&self) -> Vec<Active> {
        let mut v: Vec<Active> = self.active.values().cloned().collect();
        v.sort_by_key(|a| (a.severity as u8, a.since));
        v
    }

    /// Past events, newest first.
    pub fn history(&self) -> Vec<Event> {
        self.history.iter().rev().cloned().collect()
    }

    /// The state file's path and contents when something changed since the last call.
    pub fn take_save(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let mut running: Vec<String> = self.running.iter().cloned().collect();
        running.sort();
        let saved = Saved { active: self.active(), history: self.history.iter().cloned().collect(), running };
        Some((self.path.clone(), serde_json::to_vec(&saved).unwrap_or_default()))
    }

    /// Adds an event raised outside the rules (such as a new release) to the history.
    pub fn record(&mut self, e: Event) {
        self.history.push_back(e);
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        self.dirty = true;
    }

    /// Compares one sample with the rules and returns what changed.
    pub fn evaluate(&mut self, s: &Snapshot) -> Vec<Event> {
        let ts = s.ts;
        let mut events = Vec::new();
        let mut present = HashSet::new();
        for c in self.checks(s) {
            present.insert(c.key.clone());
            let Some(bad) = c.bad else { continue };
            let firing = self.active.contains_key(&c.key);
            if bad == firing {
                self.pending.remove(&c.key);
                continue;
            }
            let since = *self.pending.entry(c.key.clone()).or_insert(ts);
            if ts.saturating_sub(since) < if bad { c.fire_after } else { c.clear_after } {
                continue;
            }
            self.pending.remove(&c.key);
            if bad {
                let (title, detail) = c.fire;
                self.active.insert(
                    c.key.clone(),
                    Active {
                        key: c.key.clone(),
                        severity: c.severity,
                        title: title.clone(),
                        detail: detail.clone(),
                        since,
                    },
                );
                events.push(Event { key: c.key, state: State::Firing, severity: c.severity, title, detail, ts });
            } else if let Some(a) = self.active.remove(&c.key) {
                let (title, detail) = c.clear;
                let lasted = format!("lasted {}", fmt_age(ts.saturating_sub(a.since)));
                let detail = if detail.is_empty() { lasted } else { format!("{detail} · {lasted}") };
                events.push(Event { key: c.key, state: State::Resolved, severity: c.severity, title, detail, ts });
            }
        }
        // While Docker is unreachable the container list is empty; that says nothing about the containers.
        if s.docker.error.is_some() {
            present.extend(self.active.keys().chain(self.pending.keys()).filter(|k| k.starts_with("app:c:")).cloned());
        }
        // Deleted monitors, removed containers and unmounted disks end quietly.
        let before = self.active.len();
        self.active.retain(|k, _| present.contains(k));
        self.pending.retain(|k, _| present.contains(k));
        self.dirty |= self.active.len() != before;

        events.extend(self.notices(s));
        self.track_running(s);
        for e in &events {
            self.history.push_back(e.clone());
        }
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        self.dirty |= !events.is_empty();
        events
    }

    /// Every condition the rules ask for, given this sample.
    fn checks(&self, s: &Snapshot) -> Vec<Check> {
        let mut v = Vec::new();
        let firing = |k: &str| self.active.contains_key(k);
        // Clear a few points below the threshold so a value hovering at it does not flap.
        let over =
            |k: &str, value: f32, limit: f32, margin: f32| value >= if firing(k) { limit - margin } else { limit };
        let sustain = self.cfg.after_mins * 60;

        let top_cpu = top_apps(s, true);
        let top_mem = top_apps(s, false);
        let mem_pct = (s.mem.total > 0).then(|| s.mem.used as f32 * 100.0 / s.mem.total as f32);

        if let Some(t) = self.cfg.cpu {
            let c = s.cpu.total;
            v.push(Check {
                key: "host.cpu".into(),
                bad: Some(over("host.cpu", c, t, 5.0)),
                fire_after: sustain,
                clear_after: 120,
                severity: Severity::Warning,
                fire: (
                    format!("CPU is at {c:.0}%"),
                    with_top(
                        format!("Above {t:.0}% for {} min · load {:.2}", self.cfg.after_mins, s.load[0]),
                        &top_cpu,
                    ),
                ),
                clear: ("CPU is back to normal".into(), format!("{c:.0}% now")),
            });
        }
        if let (Some(t), Some(m)) = (self.cfg.memory, mem_pct) {
            v.push(Check {
                key: "host.mem".into(),
                bad: Some(over("host.mem", m, t, 5.0)),
                fire_after: sustain,
                clear_after: 120,
                severity: Severity::Warning,
                fire: (
                    format!("Memory is at {m:.0}%"),
                    with_top(format!("{} free of {}", size(s.mem.available), size(s.mem.total)), &top_mem),
                ),
                clear: ("Memory is back to normal".into(), format!("{m:.0}% now")),
            });
        }
        // A spike near 100% is urgent: it alerts after one minute instead of the sustain time.
        if let Some(t) = self.cfg.spike {
            let c = s.cpu.total;
            v.push(Check {
                key: "host.cpu.spike".into(),
                bad: Some(over("host.cpu.spike", c, t, 3.0)),
                fire_after: 60,
                clear_after: 60,
                severity: Severity::Critical,
                fire: (format!("CPU spike: {c:.0}%"), with_top(format!("At or above {t:.0}% for a minute"), &top_cpu)),
                clear: ("CPU spike is over".into(), format!("{c:.0}% now")),
            });
            if let Some(m) = mem_pct {
                let swap = if s.mem.swap_total > 0 {
                    format!(" · swap {:.0}% used", s.mem.swap_used as f32 * 100.0 / s.mem.swap_total as f32)
                } else {
                    String::new()
                };
                v.push(Check {
                    key: "host.mem.spike".into(),
                    bad: Some(over("host.mem.spike", m, t, 3.0)),
                    fire_after: 60,
                    clear_after: 60,
                    severity: Severity::Critical,
                    fire: (
                        format!("Memory nearly exhausted: {m:.0}%"),
                        with_top(format!("{} free{swap}; apps may be killed soon", size(s.mem.available)), &top_mem),
                    ),
                    clear: ("Memory pressure is over".into(), format!("{m:.0}% now")),
                });
            }
        }
        if let Some(per_core) = self.cfg.load_per_core
            && s.cores > 0
        {
            let limit = per_core * s.cores as f32;
            v.push(Check {
                key: "host.load".into(),
                bad: Some(over("host.load", s.load[0], limit, limit * 0.1)),
                fire_after: sustain,
                clear_after: 120,
                severity: Severity::Warning,
                fire: (
                    format!("Load is {:.1} on {} cores", s.load[0], s.cores),
                    with_top(format!("Above {limit:.1} for {} min; work is queuing up", self.cfg.after_mins), &top_cpu),
                ),
                clear: ("Load is back to normal".into(), format!("{:.1} now", s.load[0])),
            });
        }
        if let Some(t) = self.cfg.disk {
            for d in &s.disks {
                let key = format!("disk:{}", d.mount);
                let space = over(&key, d.pct, t, 2.0);
                let inodes = over(&key, d.inodes_pct, t, 2.0);
                let fire = if inodes && !space {
                    (
                        format!("Disk {} is out of inodes", d.mount),
                        format!("{:.0}% of inodes used (many small files)", d.inodes_pct),
                    )
                } else {
                    (
                        format!("Disk {} is {:.0}% full", d.mount, d.pct),
                        format!("{} free of {}", size(d.avail), size(d.total)),
                    )
                };
                v.push(Check {
                    key,
                    bad: Some(space || inodes),
                    fire_after: 60,
                    clear_after: 120,
                    severity: Severity::Critical,
                    fire,
                    clear: (format!("Disk {} has room again", d.mount), format!("{:.0}% used", d.pct)),
                });
            }
        }

        for m in &s.monitors {
            v.push(Check {
                key: format!("mon:{}", m.id),
                bad: m.up.map(|up| !up),
                // About two failed checks in a row (checks run every 30 s; processes every sample).
                fire_after: if m.kind == "process" { 30 } else { 50 },
                clear_after: 0,
                severity: Severity::Critical,
                fire: (format!("{} is down", m.name), m.detail.clone()),
                clear: (format!("{} is back up", m.name), m.detail.clone()),
            });
        }

        if !self.cfg.apps {
            return v;
        }
        if s.docker.error.is_some() || firing("docker") {
            v.push(Check {
                key: "docker".into(),
                bad: Some(s.docker.error.is_some()),
                fire_after: 60,
                clear_after: 0,
                severity: Severity::Critical,
                fire: ("Docker is not reachable".into(), s.docker.error.clone().unwrap_or_default()),
                clear: ("Docker is reachable again".into(), String::new()),
            });
        }
        for a in &s.apps {
            if a.kind == "container" {
                if self.running.contains(&a.id) {
                    v.push(Check {
                        key: format!("app:{}:down", a.id),
                        bad: Some(matches!(a.state.as_str(), "exited" | "dead" | "restarting")),
                        fire_after: 60,
                        clear_after: 0,
                        severity: Severity::Critical,
                        fire: (format!("{} stopped", a.name), exit_hint(&a.status)),
                        clear: (format!("{} is running again", a.name), a.status.clone()),
                    });
                }
                if a.state == "running" {
                    v.push(Check {
                        key: format!("app:{}:health", a.id),
                        bad: Some(a.health.as_deref() == Some("unhealthy")),
                        fire_after: 60,
                        clear_after: 0,
                        severity: Severity::Warning,
                        fire: (format!("{} is unhealthy", a.name), "Its Docker health check is failing".into()),
                        clear: (format!("{} is healthy again", a.name), String::new()),
                    });
                }
            } else if a.kind == "service" {
                v.push(Check {
                    key: format!("app:{}:failed", a.id),
                    bad: Some(a.state == "failed"),
                    fire_after: 0,
                    clear_after: 0,
                    severity: Severity::Critical,
                    fire: (format!("Service {} failed", a.name), "systemd marked the unit as failed".into()),
                    clear: (format!("Service {} recovered", a.name), String::new()),
                });
            }
        }
        v
    }

    /// Restarts and out-of-memory kills, rate limited per app.
    fn notices(&mut self, s: &Snapshot) -> Vec<Event> {
        if !self.cfg.apps {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for a in &s.apps {
            seen.insert(a.id.clone());
            let mut found = Vec::new();
            // Counters are known only while the app runs; first sight is the baseline, and a
            // smaller value means a new container or cgroup.
            if let Some(n) = a.restarts
                && self.restarts.insert(a.id.clone(), n).is_some_and(|p| n > p)
            {
                found.push((
                    format!("app:{}:restart", a.id),
                    format!("{} restarted", a.name),
                    format!("{n} restarts so far · {}", a.status),
                ));
            }
            if let Some(k) = a.oom_kills
                && let Some(p) = self.ooms.insert(a.id.clone(), k)
                && k > p
            {
                let limit = a.mem_limit.map_or(String::new(), |l| format!(" (limit {})", size(l)));
                found.push((
                    format!("app:{}:oom", a.id),
                    format!("{} ran out of memory", a.name),
                    format!("The kernel killed {} of its processes for using too much memory{limit}", k - p),
                ));
            }
            for (key, title, detail) in found {
                if self.last_notice.get(&key).is_some_and(|t| s.ts.saturating_sub(*t) < NOTICE_GAP) {
                    continue;
                }
                self.last_notice.insert(key.clone(), s.ts);
                out.push(Event { key, state: State::Notice, severity: Severity::Warning, title, detail, ts: s.ts });
            }
        }
        if s.docker.error.is_none() {
            self.restarts.retain(|k, _| seen.contains(k));
            self.ooms.retain(|k, _| seen.contains(k));
        }
        self.last_notice.retain(|_, t| s.ts.saturating_sub(*t) < NOTICE_GAP);
        out
    }

    /// Remembers which containers ran, forgetting removed ones (unless Docker itself is down).
    fn track_running(&mut self, s: &Snapshot) {
        let before = self.running.len();
        for a in s.apps.iter().filter(|a| a.kind == "container" && a.state == "running") {
            if !self.running.contains(&a.id) {
                self.running.insert(a.id.clone());
                self.dirty = true;
            }
        }
        if s.docker.error.is_none() {
            self.running.retain(|id| s.apps.iter().any(|a| &a.id == id));
        }
        self.dirty |= self.running.len() != before;
    }
}

/// Docker's status, plus what the common kill exit codes mean.
fn exit_hint(status: &str) -> String {
    if status.contains("(137)") {
        format!("{status} · code 137 means it was killed, often for running out of memory")
    } else if status.contains("(139)") {
        format!("{status} · code 139 means it crashed (segmentation fault)")
    } else {
        status.to_string()
    }
}

/// The three running apps using the most CPU (or memory), such as `web 45%, db 20%`.
fn top_apps(s: &Snapshot, cpu: bool) -> String {
    let mut apps: Vec<&crate::collect::App> = s.apps.iter().filter(|a| a.state == "running").collect();
    if cpu {
        apps.retain(|a| a.cpu_pct.is_some_and(|c| c >= 1.0));
        apps.sort_by(|a, b| b.cpu_pct.unwrap_or(0.0).total_cmp(&a.cpu_pct.unwrap_or(0.0)));
    } else {
        apps.retain(|a| a.mem.is_some_and(|m| m > 0));
        apps.sort_by_key(|a| std::cmp::Reverse(a.mem.unwrap_or(0)));
    }
    let parts: Vec<String> = apps
        .iter()
        .take(3)
        .map(|a| {
            if cpu {
                format!("{} {:.0}%", a.name, a.cpu_pct.unwrap_or(0.0))
            } else {
                format!("{} {}", a.name, size(a.mem.unwrap_or(0)))
            }
        })
        .collect();
    parts.join(", ")
}

fn with_top(detail: String, top: &str) -> String {
    if top.is_empty() { detail } else { format!("{detail} · top: {top}") }
}

/// Bytes with binary units, matching the dashboard.
fn size(b: u64) -> String {
    super::html::bytes(b as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{App, Disk, Mem, MonitorStatus};

    fn engine() -> (Engine, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        (Engine::load(AlertConfig::default(), d.path()), d)
    }

    fn snap(ts: u64) -> Snapshot {
        Snapshot { ts, mem: Mem { total: 100, used: 10, available: 90, ..Default::default() }, ..Default::default() }
    }

    fn container(state: &str) -> App {
        App {
            id: "c:web".into(),
            kind: "container",
            name: "web".into(),
            state: state.into(),
            status: state.into(),
            ..Default::default()
        }
    }

    #[test]
    fn cpu_must_stay_high_before_firing_and_low_before_clearing() {
        let (mut e, _d) = engine();
        let mut s = snap(1000);
        s.cpu.total = 95.0;
        assert!(e.evaluate(&s).is_empty(), "starts pending");
        s.ts = 1000 + 299;
        assert!(e.evaluate(&s).is_empty());
        s.ts = 1300;
        let ev = e.evaluate(&s);
        assert_eq!((ev.len(), ev[0].state, ev[0].title.as_str()), (1, State::Firing, "CPU is at 95%"));
        assert_eq!(e.active().len(), 1);

        // 87% is below 90 but within the 5-point margin: still firing.
        s.cpu.total = 87.0;
        s.ts = 2000;
        assert!(e.evaluate(&s).is_empty());
        s.cpu.total = 20.0;
        s.ts = 2010;
        assert!(e.evaluate(&s).is_empty(), "must stay low for 2 minutes");
        s.ts = 2130;
        let ev = e.evaluate(&s);
        assert_eq!(ev[0].state, State::Resolved);
        assert!(ev[0].detail.contains("lasted"), "{}", ev[0].detail);
        assert!(e.active().is_empty());
    }

    #[test]
    fn spike_fires_after_one_minute_and_names_top_apps() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.mem = Mem { total: 100, used: 99, available: 1, ..Default::default() };
        let mut a = container("running");
        a.mem = Some(2_000_000_000);
        s.apps = vec![a];
        assert!(e.evaluate(&s).is_empty());
        s.ts = 60;
        let ev = e.evaluate(&s);
        assert_eq!(ev.len(), 1, "only the spike, the 5-minute rule is still pending");
        assert_eq!((ev[0].title.as_str(), ev[0].severity), ("Memory nearly exhausted: 99%", Severity::Critical));
        assert!(ev[0].detail.ends_with("top: web 1.9 GB"), "{}", ev[0].detail);
    }

    #[test]
    fn load_is_per_core() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.cores = 2;
        s.load = [3.9, 0.0, 0.0];
        e.evaluate(&s);
        s.ts = 400;
        assert!(e.evaluate(&s).is_empty(), "3.9 is under 2 per core on 2 cores");
        s.load = [4.5, 0.0, 0.0];
        e.evaluate(&s);
        s.ts = 800;
        assert_eq!(e.evaluate(&s)[0].title, "Load is 4.5 on 2 cores");
    }

    #[test]
    fn a_blip_does_not_fire() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.cpu.total = 99.0;
        e.evaluate(&s);
        s.ts = 200;
        s.cpu.total = 10.0;
        e.evaluate(&s);
        s.ts = 400;
        s.cpu.total = 99.0;
        assert!(e.evaluate(&s).is_empty(), "the timer restarted after the dip");
    }

    #[test]
    fn monitor_needs_two_failed_checks_and_unknown_changes_nothing() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.monitors = vec![MonitorStatus {
            id: "m1".into(),
            name: "api".into(),
            kind: "http".into(),
            up: None,
            ..Default::default()
        }];
        assert!(e.evaluate(&s).is_empty());
        s.monitors[0].up = Some(false);
        s.monitors[0].detail = "HTTP 503".into();
        s.ts = 10;
        assert!(e.evaluate(&s).is_empty());
        s.ts = 60;
        let ev = e.evaluate(&s);
        assert_eq!((ev[0].title.as_str(), ev[0].detail.as_str()), ("api is down", "HTTP 503"));
        s.monitors[0].up = Some(true);
        s.ts = 90;
        assert_eq!(e.evaluate(&s)[0].title, "api is back up", "recovery is reported at once");
    }

    #[test]
    fn deleted_monitor_ends_quietly() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.monitors = vec![MonitorStatus { id: "m1".into(), name: "api".into(), up: Some(false), ..Default::default() }];
        e.evaluate(&s);
        s.ts = 100;
        assert_eq!(e.evaluate(&s).len(), 1);
        s.monitors.clear();
        s.ts = 200;
        assert!(e.evaluate(&s).is_empty());
        assert!(e.active().is_empty());
    }

    #[test]
    fn only_containers_seen_running_alert_when_stopped() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.apps = vec![container("exited")];
        e.evaluate(&s);
        s.ts = 500;
        assert!(e.evaluate(&s).is_empty(), "an old stopped container is not news");

        s.apps = vec![container("running")];
        s.ts = 600;
        e.evaluate(&s);
        s.apps = vec![container("exited")];
        s.ts = 610;
        assert!(e.evaluate(&s).is_empty(), "a quick redeploy within a minute stays quiet");
        s.ts = 680;
        let ev = e.evaluate(&s);
        assert_eq!((ev[0].state, ev[0].title.as_str()), (State::Firing, "web stopped"));
    }

    #[test]
    fn restarts_and_oom_are_notices_rate_limited() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        let mut a = container("running");
        a.restarts = Some(0);
        a.oom_kills = Some(2);
        s.apps = vec![a.clone()];
        assert!(e.evaluate(&s).is_empty(), "first sight is the baseline");
        a.restarts = Some(1);
        s.apps = vec![a.clone()];
        s.ts = 60;
        assert_eq!(e.evaluate(&s)[0].title, "web restarted");
        a.restarts = Some(2);
        s.apps = vec![a.clone()];
        s.ts = 120;
        assert!(e.evaluate(&s).is_empty(), "at most one restart notice per 15 minutes");
        a.oom_kills = Some(3);
        a.mem_limit = Some(512_000_000);
        s.apps = vec![a.clone()];
        s.ts = 180;
        let ev = e.evaluate(&s);
        assert_eq!(ev[0].title, "web ran out of memory");
        assert!(ev[0].detail.contains("killed 1 of its processes") && ev[0].detail.contains("limit 488 MB"));
        a.oom_kills = Some(0);
        s.apps = vec![a];
        s.ts = 2000;
        assert!(e.evaluate(&s).is_empty(), "a reset counter is a new baseline");
    }

    #[test]
    fn docker_outage_keeps_container_alerts() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.apps = vec![container("running")];
        e.evaluate(&s);
        s.apps = vec![container("exited")];
        s.ts = 10;
        e.evaluate(&s);
        s.ts = 80;
        assert_eq!(e.evaluate(&s)[0].title, "web stopped");
        // Docker stops answering: the list is empty, but the alert must neither end nor repeat.
        s.apps.clear();
        s.docker.error = Some("connection refused".into());
        s.ts = 90;
        assert!(e.evaluate(&s).is_empty());
        assert_eq!(e.active().len(), 1);
        s.docker.error = None;
        s.apps = vec![container("exited")];
        s.ts = 300;
        assert!(e.evaluate(&s).is_empty(), "no second 'stopped' alert");
        assert!(e.active().iter().any(|a| a.title == "web stopped"));
    }

    #[test]
    fn failed_service_fires_at_once_and_disk_uses_inodes_too() {
        let (mut e, _d) = engine();
        let mut s = snap(0);
        s.apps = vec![App {
            id: "s:nginx.service".into(),
            kind: "service",
            name: "nginx".into(),
            state: "failed".into(),
            ..Default::default()
        }];
        s.disks = vec![Disk { mount: "/".into(), pct: 40.0, inodes_pct: 97.0, ..Default::default() }];
        let ev = e.evaluate(&s);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].title, "Service nginx failed");
        s.ts = 60;
        assert_eq!(e.evaluate(&s)[0].title, "Disk / is out of inodes");
    }

    #[test]
    fn state_survives_restart_without_resending() {
        let d = tempfile::tempdir().unwrap();
        let mut e = Engine::load(AlertConfig::default(), d.path());
        let mut s = snap(0);
        s.apps = vec![App {
            id: "s:x.service".into(),
            kind: "service",
            name: "x".into(),
            state: "failed".into(),
            ..Default::default()
        }];
        assert_eq!(e.evaluate(&s).len(), 1);
        let (path, bytes) = e.take_save().unwrap();
        std::fs::write(path, bytes).unwrap();
        assert!(e.take_save().is_none(), "nothing new to save");

        let mut e = Engine::load(AlertConfig::default(), d.path());
        s.ts = 100;
        assert!(e.evaluate(&s).is_empty(), "already firing before the restart");
        assert_eq!(e.history().len(), 1);
        s.apps[0].state = "running".into();
        assert_eq!(e.evaluate(&s)[0].state, State::Resolved);
    }

    #[test]
    fn rules_can_be_turned_off() {
        let d = tempfile::tempdir().unwrap();
        let cfg = AlertConfig { cpu: None, spike: None, apps: false, ..AlertConfig::default() };
        let mut e = Engine::load(cfg, d.path());
        let mut s = snap(0);
        s.cpu.total = 100.0;
        s.apps = vec![App {
            id: "s:x.service".into(),
            kind: "service",
            name: "x".into(),
            state: "failed".into(),
            ..Default::default()
        }];
        e.evaluate(&s);
        s.ts = 10_000;
        assert!(e.evaluate(&s).is_empty());
    }
}
