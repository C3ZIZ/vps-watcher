//! Reports built from history: averages, peaks, availability and disk-full forecasts.

use serde::Serialize;

use crate::collect::Snapshot;
use crate::store::{History, Points, Range};

/// Average, peak and latest value of one series.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct Stat {
    pub avg: Option<f64>,
    pub peak: Option<f64>,
    pub last: Option<f64>,
}

/// Summarizes known values of `p`.
pub fn stat(p: Option<&Points>) -> Stat {
    let Some(p) = p else { return Stat::default() };
    let (mut sum, mut n, mut peak) = (0.0f64, 0usize, f64::MIN);
    for v in p.known() {
        sum += v as f64;
        n += 1;
        peak = peak.max(v as f64);
    }
    if n == 0 {
        return Stat::default();
    }
    Stat {
        avg: Some(sum / n as f64),
        peak: Some(peak),
        last: p.values.iter().rev().flatten().next().map(|v| *v as f64),
    }
}

#[derive(Debug, Serialize)]
pub struct HostRow {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    #[serde(flatten)]
    pub stat: Stat,
}

#[derive(Debug, Serialize)]
pub struct AppRow {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub cpu: Stat,
    pub mem: Stat,
    /// Approximate bytes moved in the window (average rate × time with data).
    pub net_rx: Option<f64>,
    pub net_tx: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct MonitorRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Percent of checks that were up.
    pub availability: Option<f64>,
    pub avg_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct DiskRow {
    pub mount: String,
    pub used_pct: f64,
    /// Growth in percentage points per day (negative means shrinking).
    pub growth_per_day: Option<f64>,
    pub days_to_full: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub range: &'static str,
    pub generated: u64,
    pub hostname: String,
    pub host: Vec<HostRow>,
    pub apps: Vec<AppRow>,
    pub monitors: Vec<MonitorRow>,
    pub disks: Vec<DiskRow>,
}

const HOST_ROWS: [(&str, &str, &str); 9] = [
    ("host.cpu", "CPU", "%"),
    ("host.cpu.steal", "CPU steal", "%"),
    ("host.cpu.iowait", "I/O wait", "%"),
    ("host.mem", "Memory", "%"),
    ("host.swap", "Swap", "%"),
    ("host.load1", "Load (1m)", ""),
    ("host.net.rx", "Network in", "B/s"),
    ("host.net.tx", "Network out", "B/s"),
    ("host.disk.write", "Disk writes", "B/s"),
];

/// Builds a report for `range` from history and the current list of apps.
pub fn build(h: &History, snap: &Snapshot, range: Range, label: &'static str, now: u64) -> Report {
    let q = |k: &str| h.query(k, range, now);
    let host =
        HOST_ROWS.iter().map(|(key, label, unit)| HostRow { key, label, unit, stat: stat(q(key).as_ref()) }).collect();

    let mut apps: Vec<AppRow> = snap
        .apps
        .iter()
        .filter(|a| h.has(&format!("app:{}.cpu", a.id)) || h.has(&format!("app:{}.mem", a.id)))
        .map(|a| {
            let total = |k: &str| {
                let p = q(&format!("app:{}.{k}", a.id))?;
                let n = p.known().count();
                stat(Some(&p)).avg.map(|avg| avg * (n as u64 * p.step) as f64)
            };
            AppRow {
                id: a.id.clone(),
                name: a.name.clone(),
                kind: a.kind,
                cpu: stat(q(&format!("app:{}.cpu", a.id)).as_ref()),
                mem: stat(q(&format!("app:{}.mem", a.id)).as_ref()),
                net_rx: total("net.rx"),
                net_tx: total("net.tx"),
            }
        })
        .collect();
    apps.sort_by(|a, b| b.cpu.avg.unwrap_or(0.0).total_cmp(&a.cpu.avg.unwrap_or(0.0)));

    let monitors = snap
        .monitors
        .iter()
        .map(|m| MonitorRow {
            id: m.id.clone(),
            name: m.name.clone(),
            kind: m.kind.clone(),
            availability: stat(q(&format!("mon:{}.up", m.id)).as_ref()).avg,
            avg_ms: stat(q(&format!("mon:{}.ms", m.id)).as_ref()).avg,
        })
        .collect();

    // Forecasts need a long baseline, so they always use the 7-day tier at least.
    let fr = if range == Range::D30 { Range::D30 } else { Range::D7 };
    let disks = snap
        .disks
        .iter()
        .map(|d| {
            let used_pct = d.used as f64 * 100.0 / d.total.max(1) as f64;
            let slope = h.query(&format!("disk:{}.used", d.mount), fr, now).and_then(|p| slope_per_day(&p));
            DiskRow {
                mount: d.mount.clone(),
                used_pct,
                growth_per_day: slope,
                days_to_full: slope.filter(|s| *s > 0.01).map(|s| ((100.0 - used_pct) / s).max(0.0)),
            }
        })
        .collect();

    Report { range: label, generated: now, hostname: snap.hostname.clone(), host, apps, monitors, disks }
}

/// Least-squares slope in units per day; needs points spanning at least 6 hours.
pub fn slope_per_day(p: &Points) -> Option<f64> {
    let pts: Vec<(f64, f64)> = p
        .values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| Some(((i as u64 * p.step) as f64 / 86_400.0, (*v)? as f64)))
        .collect();
    let (first, last) = (pts.first()?.0, pts.last()?.0);
    if pts.len() < 3 || last - first < 0.25 {
        return None;
    }
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let num: f64 = pts.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
    let den: f64 = pts.iter().map(|(x, _)| (x - mx).powi(2)).sum();
    (den > 0.0).then(|| num / den)
}

/// Renders the report as CSV (section, name, metric, avg, peak, last).
pub fn to_csv(r: &Report) -> String {
    let f = |v: Option<f64>| v.map(|v| format!("{v:.2}")).unwrap_or_default();
    let mut out = String::from("section,name,metric,avg,peak,last\n");
    for h in &r.host {
        out += &format!(
            "host,{},{},{},{},{}\n",
            csv(r.hostname.as_str()),
            csv(h.label),
            f(h.stat.avg),
            f(h.stat.peak),
            f(h.stat.last)
        );
    }
    for a in &r.apps {
        out += &format!("app,{},cpu %,{},{},{}\n", csv(&a.name), f(a.cpu.avg), f(a.cpu.peak), f(a.cpu.last));
        out += &format!("app,{},memory bytes,{},{},{}\n", csv(&a.name), f(a.mem.avg), f(a.mem.peak), f(a.mem.last));
    }
    for m in &r.monitors {
        out += &format!("monitor,{},availability %,{},,\n", csv(&m.name), f(m.availability));
        out += &format!("monitor,{},response ms,{},,\n", csv(&m.name), f(m.avg_ms));
    }
    for d in &r.disks {
        out += &format!("disk,{},used %,,,{:.2}\n", csv(&d.mount), d.used_pct);
        out += &format!("disk,{},days to full,,,{}\n", csv(&d.mount), f(d.days_to_full));
    }
    out
}

/// Quotes a CSV field and defuses spreadsheet formulas.
fn csv(s: &str) -> String {
    let s = if s.starts_with(['=', '+', '-', '@']) { format!("'{s}") } else { s.to_string() };
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_ignores_gaps() {
        let p = Points { start: 0, step: 60, values: vec![Some(1.0), None, Some(3.0), None] };
        assert_eq!(stat(Some(&p)), Stat { avg: Some(2.0), peak: Some(3.0), last: Some(3.0) });
        assert_eq!(stat(None), Stat::default());
    }

    #[test]
    fn slope_detects_growth() {
        // +1 point per hour → 24 per day.
        let p = Points { start: 0, step: 3600, values: (0..24).map(|i| Some(50.0 + i as f32)).collect() };
        assert!((slope_per_day(&p).unwrap() - 24.0).abs() < 0.01);
        let short = Points { start: 0, step: 60, values: vec![Some(1.0), Some(2.0), Some(3.0)] };
        assert_eq!(slope_per_day(&short), None, "too little time to trust");
    }

    #[test]
    fn csv_escapes() {
        assert_eq!(csv("a,b"), "\"a,b\"");
        assert_eq!(csv("=cmd()"), "'=cmd()");
        assert_eq!(csv("plain"), "plain");
    }
}
