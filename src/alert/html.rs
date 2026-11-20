//! Email bodies: alert and report messages as inline-styled HTML tables, plus plain-text twins.
//! Email clients ignore most CSS, so layout uses tables and every style is inline.

use super::engine::{Event, Severity, State};
use crate::collect::Snapshot;
use crate::report::Report;
use crate::util::utc_stamp;

/// The server's state when an alert went out, so the reader sees what is using resources.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub cores: usize,
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: [f32; 3],
    /// Fullest disks: mount, used percent, free bytes.
    pub disks: Vec<(String, f32, u64)>,
    /// Running apps with usage: name, CPU percent, memory bytes.
    pub apps: Vec<(String, Option<f32>, Option<u64>)>,
}

impl Context {
    pub fn from_snapshot(s: &Snapshot) -> Self {
        let mut disks: Vec<(String, f32, u64)> = s.disks.iter().map(|d| (d.mount.clone(), d.pct, d.avail)).collect();
        disks.sort_by(|a, b| b.1.total_cmp(&a.1));
        disks.truncate(3);
        let apps = s
            .apps
            .iter()
            .filter(|a| a.state == "running" && (a.cpu_pct.is_some() || a.mem.is_some()))
            .map(|a| (a.name.clone(), a.cpu_pct, a.mem))
            .collect();
        Context {
            cores: s.cores,
            cpu: s.cpu.total,
            mem_used: s.mem.used,
            mem_total: s.mem.total,
            swap_used: s.mem.swap_used,
            swap_total: s.mem.swap_total,
            load: s.load,
            disks,
            apps,
        }
    }

    fn mem_pct(&self) -> f32 {
        if self.mem_total > 0 { self.mem_used as f32 * 100.0 / self.mem_total as f32 } else { 0.0 }
    }
}

const BRAND: &str = "#356699";
const TEXT: &str = "#1f2933";
const MUTED: &str = "#5f6b7a";
const LINE: &str = "#e4e8ee";

/// Escapes text for HTML element and attribute content.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// Human bytes with binary units, like the dashboard.
pub fn bytes(n: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let (mut n, mut i) = (n, 0);
    while n.abs() >= 1024.0 && i < units.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n:.0} {}", units[i])
    } else if n.abs() < 10.0 {
        format!("{n:.1} {}", units[i])
    } else {
        format!("{n:.0} {}", units[i])
    }
}

fn pct(v: f64) -> String {
    if v < 10.0 { format!("{v:.1}%") } else { format!("{v:.0}%") }
}

/// Colored label: text and background pass WCAG AA contrast.
fn pill(text: &str, fg: &str, bg: &str) -> String {
    format!(
        "<span style=\"display:inline-block;padding:2px 8px;border-radius:10px;font-size:12px;font-weight:600;\
         color:{fg};background:{bg};white-space:nowrap\">{}</span>",
        esc(text)
    )
}

fn state_pill(e: &Event) -> String {
    match (e.state, e.severity) {
        (State::Resolved, _) => pill("Resolved", "#05603a", "#dcfae6"),
        (State::Notice, Severity::Info) => pill("Info", "#1d4f7a", "#e3effa"),
        (State::Notice, _) => pill("Notice", "#93370d", "#fef0c7"),
        (State::Firing, Severity::Critical) => pill("Critical", "#912018", "#fee4e2"),
        (State::Firing, _) => pill("Warning", "#93370d", "#fef0c7"),
    }
}

/// Colors a usage percent: amber from 80, red from 90.
fn level(v: f64) -> &'static str {
    if v >= 90.0 {
        "#b42318"
    } else if v >= 80.0 {
        "#b54708"
    } else {
        TEXT
    }
}

/// A table with a header row; `right` marks numeric columns.
fn table(head: &[(&str, bool)], rows: &[Vec<String>]) -> String {
    let th: String = head
        .iter()
        .map(|(h, right)| {
            format!(
                "<th align=\"{}\" style=\"padding:8px 10px;border-bottom:1px solid {LINE};font-size:12px;font-weight:600;\
                 color:{MUTED};text-transform:uppercase;letter-spacing:.03em\">{}</th>",
                if *right { "right" } else { "left" },
                esc(h)
            )
        })
        .collect();
    let tr: String = rows
        .iter()
        .map(|r| {
            let tds: String = r
                .iter()
                .zip(head)
                .map(|(cell, (_, right))| {
                    format!(
                        "<td align=\"{}\" style=\"padding:8px 10px;border-bottom:1px solid {LINE};font-size:14px;color:{TEXT};\
                         vertical-align:top\">{cell}</td>",
                        if *right { "right" } else { "left" }
                    )
                })
                .collect();
            format!("<tr>{tds}</tr>")
        })
        .collect();
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" \
         style=\"border-collapse:collapse;font-variant-numeric:tabular-nums\"><tr>{th}</tr>{tr}</table>"
    )
}

fn section(title: &str, body: &str) -> String {
    format!("<h2 style=\"margin:28px 0 8px;font-size:15px;color:{TEXT}\">{}</h2>{body}", esc(title))
}

/// Page frame: brand header, white card, footer. `preheader` is the inbox preview line.
fn frame(heading: &str, preheader: &str, inner: &str, link: Option<&str>, link_label: &str) -> String {
    let button = link.map_or(String::new(), |l| {
        format!(
            "<p style=\"margin:28px 0 0\"><a href=\"{}\" style=\"display:inline-block;padding:10px 18px;border-radius:6px;\
             background:{BRAND};color:#ffffff;text-decoration:none;font-weight:600;font-size:14px\">{}</a></p>",
            esc(l),
            esc(link_label)
        )
    });
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <meta name=\"color-scheme\" content=\"light\"><title>{h}</title></head>\
         <body style=\"margin:0;padding:0;background:#f3f5f8\">\
         <div style=\"display:none;max-height:0;overflow:hidden\">{pre}</div>\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#f3f5f8\"><tr><td align=\"center\" style=\"padding:24px 12px\">\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:640px;background:#ffffff;border:1px solid {LINE};border-radius:10px;\
         font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif\">\
         <tr><td style=\"padding:16px 24px;border-bottom:3px solid {BRAND};font-size:13px;font-weight:700;color:{BRAND};letter-spacing:.02em\">vps-watcher</td></tr>\
         <tr><td style=\"padding:20px 24px 28px\"><h1 style=\"margin:0 0 4px;font-size:20px;line-height:1.3;color:{TEXT}\">{h}</h1>{inner}{button}</td></tr>\
         </table>\
         <p style=\"margin:16px 0 0;font-family:Arial,sans-serif;font-size:12px;color:{MUTED}\">Sent by vps-watcher on your server. \
         Rules and recipients are set in /etc/vps-watcher/vps-watcher.env.</p>\
         </td></tr></table></body></html>",
        h = esc(heading),
        pre = esc(preheader),
    )
}

/// The alert email: events, what to do, then the server's state and its busiest apps.
pub fn alert_html(host: &str, events: &[Event], ctx: &Context, link: Option<&str>) -> String {
    let heading = match events {
        [e] => e.title.clone(),
        _ => format!("{} alerts on {host}", events.len()),
    };
    let rows: Vec<Vec<String>> = events
        .iter()
        .take(20)
        .map(|e| {
            let detail = if e.detail.is_empty() {
                String::new()
            } else {
                format!("<div style=\"color:{MUTED};font-size:13px;margin-top:2px\">{}</div>", esc(&e.detail))
            };
            // "22:01 UTC" over "2026-11-20", so narrow screens never split the date.
            let stamp = utc_stamp(e.ts);
            let (date, time) = stamp.split_once(' ').unwrap_or(("", &stamp));
            let when = format!(
                "<span style=\"white-space:nowrap\">{}</span><div style=\"color:{MUTED};font-size:12px;white-space:nowrap\">{}</div>",
                esc(time),
                esc(date)
            );
            vec![state_pill(e), format!("<b>{}</b>{detail}", esc(&e.title)), when]
        })
        .collect();
    let mut inner = format!(
        "<p style=\"margin:0;color:{MUTED};font-size:14px\">On <b style=\"color:{TEXT}\">{}</b></p>",
        esc(host)
    );
    inner += &section("What happened", &table(&[("Status", false), ("Alert", false), ("When", true)], &rows));
    if events.len() > 20 {
        inner += &format!(
            "<p style=\"color:{MUTED};font-size:13px\">…and {} more on the Alerts page.</p>",
            events.len() - 20
        );
    }

    let advice = advice(events);
    if !advice.is_empty() {
        let items: String = advice.iter().map(|a| format!("<li style=\"margin:4px 0\">{}</li>", esc(a))).collect();
        inner += &section(
            "What you can do",
            &format!(
                "<ul style=\"margin:0;padding-left:20px;font-size:14px;color:{TEXT};line-height:1.5\">{items}</ul>"
            ),
        );
    }

    if ctx.mem_total > 0 {
        let mut rows = vec![
            vec!["CPU".into(), colored(pct(ctx.cpu as f64), ctx.cpu as f64), format!("{} cores", ctx.cores)],
            vec![
                "Memory".into(),
                colored(pct(ctx.mem_pct() as f64), ctx.mem_pct() as f64),
                format!("{} of {}", bytes(ctx.mem_used as f64), bytes(ctx.mem_total as f64)),
            ],
        ];
        if ctx.swap_total > 0 {
            let p = ctx.swap_used as f64 * 100.0 / ctx.swap_total as f64;
            rows.push(vec![
                "Swap".into(),
                colored(pct(p), p),
                format!("{} of {}", bytes(ctx.swap_used as f64), bytes(ctx.swap_total as f64)),
            ]);
        }
        let per_core = if ctx.cores > 0 { ctx.load[0] as f64 / ctx.cores as f64 * 50.0 } else { 0.0 };
        rows.push(vec![
            "Load".into(),
            colored(format!("{:.2}", ctx.load[0]), per_core),
            format!("5m {:.2} · 15m {:.2}", ctx.load[1], ctx.load[2]),
        ]);
        for (mount, p, free) in &ctx.disks {
            rows.push(vec![
                format!("Disk {}", esc(mount)),
                colored(pct(*p as f64), *p as f64),
                format!("{} free", bytes(*free as f64)),
            ]);
        }
        inner += &section("Server right now", &table(&[("Resource", false), ("Used", true), ("Detail", true)], &rows));

        let by_mem = events.iter().any(|e| e.key.starts_with("host.mem"));
        let mut apps = ctx.apps.clone();
        if by_mem {
            apps.sort_by_key(|a| std::cmp::Reverse(a.2.unwrap_or(0)));
        } else {
            apps.sort_by(|a, b| b.1.unwrap_or(0.0).total_cmp(&a.1.unwrap_or(0.0)));
        }
        let rows: Vec<Vec<String>> = apps
            .iter()
            .take(6)
            .map(|(name, cpu, mem)| {
                let mem_share =
                    mem.map(|m| if ctx.mem_total > 0 { m as f64 * 100.0 / ctx.mem_total as f64 } else { 0.0 });
                vec![
                    esc(name),
                    cpu.map_or("–".into(), |c| pct(c as f64)),
                    mem.map_or("–".into(), |m| format!("{} ({})", bytes(m as f64), pct(mem_share.unwrap_or(0.0)))),
                ]
            })
            .collect();
        if !rows.is_empty() {
            let title = if by_mem { "Apps using the most memory" } else { "Apps using the most CPU" };
            inner += &section(title, &table(&[("App", false), ("CPU", true), ("Memory", true)], &rows));
        }
    }
    let preheader = events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>().join(" · ");
    frame(&heading, &preheader, &inner, link.map(|l| format!("{l}/#/alerts")).as_deref(), "Open the dashboard")
}

fn colored(text: String, v: f64) -> String {
    format!("<span style=\"color:{};font-weight:600\">{text}</span>", level(v))
}

/// Plain advice for resource alerts, so the reader knows whether to add capacity.
fn advice(events: &[Event]) -> Vec<String> {
    let firing = |p: &str| events.iter().any(|e| e.state == State::Firing && e.key.starts_with(p));
    let mut v = Vec::new();
    if firing("host.mem") {
        v.push("Memory is running out. Restart or limit the apps listed below, or give the server more RAM. Adding swap only buys time.".into());
    }
    if firing("host.cpu") || firing("host.load") {
        v.push("The CPU is saturated. Check the busiest apps below; if this load is normal now, move to a plan with more vCPUs.".into());
    }
    if firing("disk:") {
        v.push("A disk is nearly full. Clear old logs, images (docker system prune) and backups, or grow the volume before writes start failing.".into());
    }
    if firing("mon:") || firing("app:") {
        v.push("Something stopped. Open its logs on the dashboard to see why before restarting it.".into());
    }
    v
}

/// Report email: server stats, busiest apps, availability and disk forecast, with capacity advice.
pub fn report_html(r: &Report, link: Option<&str>) -> String {
    let fmtv = |unit: &str, v: Option<f64>| match v {
        None => "–".to_string(),
        Some(v) => match unit {
            "%" => pct(v),
            "B/s" => format!("{}/s", bytes(v)),
            _ => format!("{v:.2}"),
        },
    };
    let host_rows: Vec<Vec<String>> = r
        .host
        .iter()
        .map(|h| {
            let peak = fmtv(h.unit, h.stat.peak);
            let peak = if h.unit == "%" { colored(peak, h.stat.peak.unwrap_or(0.0)) } else { peak };
            vec![esc(h.label), fmtv(h.unit, h.stat.avg), peak, fmtv(h.unit, h.stat.last)]
        })
        .collect();
    let mut inner = format!(
        "<p style=\"margin:0;color:{MUTED};font-size:14px\">{} · last {} · {}</p>",
        esc(&r.hostname),
        esc(r.range),
        esc(&utc_stamp(r.generated))
    );
    let advice = report_advice(r);
    if !advice.is_empty() {
        let items: String = advice.iter().map(|a| format!("<li style=\"margin:4px 0\">{}</li>", esc(a))).collect();
        inner += &section(
            "Worth a look",
            &format!("<ul style=\"margin:0;padding-left:20px;font-size:14px;line-height:1.5\">{items}</ul>"),
        );
    }
    inner += &section(
        "Server",
        &table(&[("Metric", false), ("Average", true), ("Peak", true), ("Latest", true)], &host_rows),
    );

    let mut apps: Vec<_> = r.apps.iter().collect();
    apps.sort_by(|a, b| b.cpu.avg.unwrap_or(0.0).total_cmp(&a.cpu.avg.unwrap_or(0.0)));
    let app_rows: Vec<Vec<String>> = apps
        .iter()
        .take(10)
        .map(|a| {
            vec![
                esc(&a.name),
                fmtv("%", a.cpu.avg),
                fmtv("%", a.cpu.peak),
                a.mem.avg.map_or("–".into(), bytes),
                a.mem.peak.map_or("–".into(), bytes),
            ]
        })
        .collect();
    if !app_rows.is_empty() {
        let head = [("App", false), ("CPU avg", true), ("CPU peak", true), ("Mem avg", true), ("Mem peak", true)];
        inner += &section("Busiest apps", &table(&head, &app_rows));
    }

    if !r.monitors.is_empty() {
        let rows: Vec<Vec<String>> = r
            .monitors
            .iter()
            .map(|m| {
                let a = match m.availability {
                    None => pill("no data", MUTED, "#eef1f5"),
                    Some(v) if v >= 99.9 => pill(&format!("{v:.2}%"), "#05603a", "#dcfae6"),
                    Some(v) if v >= 99.0 => pill(&format!("{v:.1}%"), "#93370d", "#fef0c7"),
                    Some(v) => pill(&format!("{v:.1}%"), "#912018", "#fee4e2"),
                };
                vec![esc(&m.name), a, m.avg_ms.map_or("–".into(), |ms| format!("{ms:.0} ms"))]
            })
            .collect();
        inner += &section("Availability", &table(&[("Monitor", false), ("Up", true), ("Avg response", true)], &rows));
    }

    let disk_rows: Vec<Vec<String>> = r
        .disks
        .iter()
        .map(|d| {
            let full = match (d.days_to_full, d.growth_per_day) {
                (Some(days), _) if days < 14.0 => pill(&format!("~{days:.0} days"), "#912018", "#fee4e2"),
                (Some(days), _) if days < 60.0 => pill(&format!("~{days:.0} days"), "#93370d", "#fef0c7"),
                (Some(days), _) => format!("~{days:.0} days"),
                (None, Some(_)) => "not growing".into(),
                (None, None) => "–".into(),
            };
            let growth = d.growth_per_day.map_or("–".into(), |g| format!("{g:+.2} pts/day"));
            vec![esc(&d.mount), colored(pct(d.used_pct), d.used_pct), growth, full]
        })
        .collect();
    if !disk_rows.is_empty() {
        inner += &section(
            "Disk forecast",
            &table(&[("Disk", false), ("Used", true), ("Growth", true), ("Full in", true)], &disk_rows),
        );
    }
    let heading = format!("Report for {}: last {}", r.hostname, r.range);
    frame(&heading, &advice.join(" "), &inner, link.map(|l| format!("{l}/#/reports")).as_deref(), "Open reports")
}

/// Capacity hints from a report, worst first.
pub fn report_advice(r: &Report) -> Vec<String> {
    let get = |k: &str| r.host.iter().find(|h| h.key == k).map(|h| h.stat);
    let mut v = Vec::new();
    if let Some(m) = get("host.mem") {
        if m.avg.unwrap_or(0.0) >= 85.0 {
            v.push(format!(
                "Memory averaged {} over this period. The server needs more RAM, or its biggest apps need limits.",
                pct(m.avg.unwrap_or(0.0))
            ));
        } else if m.peak.unwrap_or(0.0) >= 95.0 {
            v.push(format!(
                "Memory peaked at {}. Short peaks like this can get apps killed; watch the busiest apps below.",
                pct(m.peak.unwrap_or(0.0))
            ));
        }
    }
    if let Some(c) = get("host.cpu")
        && c.avg.unwrap_or(0.0) >= 70.0
    {
        v.push(format!("CPU averaged {}. Little headroom is left; consider more vCPUs.", pct(c.avg.unwrap_or(0.0))));
    }
    if let Some(s) = get("host.cpu.steal")
        && s.avg.unwrap_or(0.0) >= 5.0
    {
        v.push(format!(
            "CPU steal averaged {}: other tenants on the host are slowing this VPS. Your provider may move you.",
            pct(s.avg.unwrap_or(0.0))
        ));
    }
    for d in &r.disks {
        if let Some(days) = d.days_to_full.filter(|d| *d < 30.0) {
            v.push(format!("Disk {} will be full in about {days:.0} days at the current rate.", d.mount));
        } else if d.used_pct >= 90.0 {
            v.push(format!("Disk {} is {} full.", d.mount, pct(d.used_pct)));
        }
    }
    for m in &r.monitors {
        if let Some(a) = m.availability.filter(|a| *a < 99.0) {
            v.push(format!("{} was up only {a:.1}% of the time.", m.name));
        }
    }
    v
}

/// Plain-text twin of the report email.
pub fn report_text(r: &Report, link: Option<&str>) -> String {
    let mut o = format!("vps-watcher report for {}: last {} ({})\n\n", r.hostname, r.range, utc_stamp(r.generated));
    for a in report_advice(r) {
        o += &format!("* {a}\n");
    }
    o += "\nServer (average / peak)\n";
    for h in &r.host {
        let f = |v: Option<f64>| match (h.unit, v) {
            (_, None) => "-".to_string(),
            ("%", Some(v)) => pct(v),
            ("B/s", Some(v)) => format!("{}/s", bytes(v)),
            (_, Some(v)) => format!("{v:.2}"),
        };
        o += &format!("  {:<14} {:>10} / {}\n", h.label, f(h.stat.avg), f(h.stat.peak));
    }
    if !r.disks.is_empty() {
        o += "\nDisks\n";
        for d in &r.disks {
            let full = d.days_to_full.map_or("not filling up".into(), |x| format!("full in ~{x:.0} days"));
            o += &format!("  {:<14} {:>6} used, {full}\n", d.mount, pct(d.used_pct));
        }
    }
    if let Some(l) = link {
        o += &format!("\nFull report: {l}/#/reports\n");
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(key: &str, state: State, title: &str) -> Event {
        Event {
            key: key.into(),
            state,
            severity: Severity::Critical,
            title: title.into(),
            detail: "d <b>".into(),
            ts: 0,
        }
    }

    #[test]
    fn escapes_everything_from_the_server() {
        assert_eq!(esc("<script>\"x\"&'"), "&lt;script&gt;&quot;x&quot;&amp;&#39;");
        let ctx = Context {
            mem_total: 100,
            mem_used: 50,
            apps: vec![("<img onerror=x>".into(), Some(5.0), Some(10))],
            ..Default::default()
        };
        let html = alert_html("h<1>", &[ev("mon:1", State::Firing, "api <down>")], &ctx, Some("https://w.io"));
        assert!(!html.contains("<img") && !html.contains("<down>") && !html.contains("d <b>"));
        assert!(html.contains("api &lt;down&gt;") && html.contains("https://w.io/#/alerts"));
    }

    #[test]
    fn memory_alert_lists_apps_by_memory_and_gives_advice() {
        let ctx = Context {
            mem_total: 1000,
            mem_used: 990,
            cores: 2,
            apps: vec![("small".into(), Some(80.0), Some(10)), ("big".into(), Some(1.0), Some(700))],
            ..Default::default()
        };
        let html = alert_html("h", &[ev("host.mem.spike", State::Firing, "Memory nearly exhausted: 99%")], &ctx, None);
        assert!(html.contains("Apps using the most memory"));
        assert!(html.find("big").unwrap() < html.find("small").unwrap(), "sorted by memory");
        assert!(html.contains("Memory is running out"));
        assert!(!html.contains("Open the dashboard"), "no link without a domain");
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(
            (bytes(512.0), bytes(1536.0), bytes(5.0 * 1024.0 * 1024.0 * 1024.0)),
            ("512 B".into(), "1.5 KB".into(), "5.0 GB".into())
        );
    }
}
