//! JSON API handlers.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AppState, COOKIE, Viewer};
use crate::auth::{self, LoginError, SESSION_IDLE};
use crate::collect::checks::{Monitor, MonitorKind};
use crate::collect::{cgroup, logs, ports, procs};
use crate::config::AuthMode;
use crate::report;
use crate::store::Range;
use crate::util::now;

/// An error with a status code, rendered as `{"error": "..."}`.
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        err(self.0, &self.1)
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

/// JSON error body with the given status.
pub fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

/// Builds the session cookie; `Secure` when the browser used HTTPS.
fn session_cookie(token: &str, secure: bool, max_age: u64) -> HeaderValue {
    let s = format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    );
    HeaderValue::from_str(&s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

#[derive(Deserialize)]
pub struct Credentials {
    email: String,
    password: String,
}

/// Access mode, setup state and whether the caller is signed in.
pub async fn session(
    State(ctx): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(v): Extension<Viewer>,
    headers: HeaderMap,
) -> Json<Value> {
    let cfg = &ctx.sh.cfg;
    Json(json!({
        "mode": if cfg.auth == AuthMode::Open { "open" } else { "login" },
        "setup_required": ctx.auth.needs_setup(),
        "setup_allowed": auth::is_direct_loopback(peer, &headers),
        "authenticated": v.admin,
        "email": v.token.as_deref().and_then(|t| ctx.auth.session(t)),
        "open_logs": cfg.open_logs,
        "version": crate::VERSION,
    }))
}

/// Creates the first admin; only from a direct loopback connection.
pub async fn setup(
    State(ctx): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(c): Json<Credentials>,
) -> ApiResult<Response> {
    if !ctx.auth.needs_setup() {
        return Err(ApiError(StatusCode::CONFLICT, "An admin already exists. Sign in instead.".into()));
    }
    if !auth::is_direct_loopback(peer, &headers) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "For safety, create the admin from the server itself: open http://127.0.0.1:9100 through an SSH tunnel."
                .into(),
        ));
    }
    ctx.auth.create_admin(&c.email, &c.password).await.map_err(bad)?;
    let ip = peer.ip();
    let token = ctx.auth.login(ip, &c.email, &c.password).await.map_err(|_| bad("Admin created; please sign in."))?;
    Ok(([(header::SET_COOKIE, session_cookie(&token, false, SESSION_IDLE.as_secs()))], Json(json!({"ok": true})))
        .into_response())
}

/// Signs in and sets the session cookie.
pub async fn login(
    State(ctx): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(c): Json<Credentials>,
) -> ApiResult<Response> {
    let trusted = &ctx.sh.cfg.trusted_proxies;
    let ip = auth::client_ip(peer, &headers, trusted);
    match ctx.auth.login(ip, &c.email, &c.password).await {
        Ok(token) => {
            let secure = auth::is_https(peer, &headers, trusted);
            let cookie = session_cookie(&token, secure, SESSION_IDLE.as_secs());
            Ok(([(header::SET_COOKIE, cookie)], Json(json!({"ok": true}))).into_response())
        }
        Err(LoginError::Limited(s)) => Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many failed attempts. Try again in {} minutes.", s.div_ceil(60)),
        )),
        Err(LoginError::Busy) => Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts right now. Try again shortly.".into(),
        )),
        Err(LoginError::Invalid) => Err(ApiError(StatusCode::UNAUTHORIZED, "Wrong email or password.".into())),
    }
}

/// Ends the current session.
pub async fn logout(State(ctx): State<AppState>, Extension(v): Extension<Viewer>) -> Response {
    if let Some(t) = &v.token {
        ctx.auth.logout(t);
    }
    ([(header::SET_COOKIE, session_cookie("", false, 0))], Json(json!({"ok": true}))).into_response()
}

#[derive(Deserialize)]
pub struct AccountChange {
    current_password: String,
    email: Option<String>,
    password: Option<String>,
}

/// Changes the admin's email or password.
pub async fn account(
    State(ctx): State<AppState>,
    Extension(v): Extension<Viewer>,
    Json(c): Json<AccountChange>,
) -> ApiResult<Json<Value>> {
    let token = v.token.ok_or_else(|| bad("Not signed in."))?;
    ctx.auth.change(&token, &c.current_password, c.email.as_deref(), c.password.as_deref()).await.map_err(bad)?;
    Ok(Json(json!({"ok": true})))
}

/// Signs out every other browser.
pub async fn logout_others(State(ctx): State<AppState>, Extension(v): Extension<Viewer>) -> Json<Value> {
    if let Some(t) = &v.token {
        ctx.auth.logout_others(t);
    }
    Json(json!({"ok": true}))
}

/// The latest snapshot.
pub async fn overview(State(ctx): State<AppState>, Extension(v): Extension<Viewer>) -> Response {
    let snap = ctx.sh.snapshot();
    if v.admin { Json(&*snap).into_response() } else { Json(snap.public()).into_response() }
}

/// Most live streams at once; each costs one connection and a copy of the JSON per sample.
const MAX_LIVE: usize = 32;

/// Server-Sent Events: one snapshot per sample while the page is open.
pub async fn live(
    State(ctx): State<AppState>,
    Extension(v): Extension<Viewer>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    if ctx.sh.tick.receiver_count() >= MAX_LIVE {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "Too many live viewers right now.".into()));
    }
    let rx = ctx.sh.tick.subscribe();
    let stream = futures_util::stream::unfold((ctx, rx, v), |(ctx, mut rx, v)| async move {
        rx.changed().await.ok()?;
        // End the stream as soon as the admin session is signed out elsewhere.
        if v.admin && v.token.as_deref().and_then(|t| ctx.auth.session(t)).is_none() {
            return None;
        }
        let data = ctx.sh.live_json(v.admin);
        Some((Ok(Event::default().data(&*data)), (ctx, rx, v)))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20))))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    keys: String,
    range: Option<String>,
}

/// Chart data for up to 24 series.
pub async fn history(State(ctx): State<AppState>, Query(q): Query<HistoryQuery>) -> ApiResult<Json<Value>> {
    let label = q.range.as_deref().unwrap_or("1h");
    let range = Range::parse(label).ok_or_else(|| bad("range must be 1h, 24h, 7d or 30d"))?;
    let now = now();
    let h = ctx.sh.history();
    let series: HashMap<&str, _> = q
        .keys
        .split(',')
        .filter(|k| !k.is_empty())
        .take(24)
        .filter_map(|k| Some((k, h.query(k, range, now)?)))
        .collect();
    Ok(Json(json!({ "range": label, "now": now, "series": series })))
}

/// Containers, services and files that have logs.
pub async fn log_sources(State(ctx): State<AppState>) -> Json<Value> {
    let snap = ctx.sh.snapshot();
    let apps: Vec<Value> = snap
        .apps
        .iter()
        .map(|a| json!({ "id": a.id, "name": a.name, "kind": a.kind, "state": a.state, "system": a.system }))
        .collect();
    let mut files: Vec<String> = allowed_files(&ctx).into_iter().map(|p| p.display().to_string()).collect();
    files.sort();
    files.dedup();
    Json(json!({ "apps": apps, "files": files }))
}

/// Files a viewer may read: discovered /var/log files plus log-file monitors.
fn allowed_files(ctx: &AppState) -> Vec<PathBuf> {
    let mut v = ctx.sh.log_files.lock().map(|f| f.clone()).unwrap_or_default();
    v.extend(ctx.sh.monitors().list().iter().filter_map(|m| match &m.kind {
        MonitorKind::Logfile { path } => Some(PathBuf::from(path)),
        _ => None,
    }));
    v
}

#[derive(Deserialize)]
pub struct LogQuery {
    source: String,
    lines: Option<usize>,
    since: Option<u64>,
    q: Option<String>,
}

/// Log lines of an app or file, optionally filtered by text.
pub async fn logs(State(ctx): State<AppState>, Query(q): Query<LogQuery>) -> ApiResult<Json<Value>> {
    let lines = q.lines.unwrap_or(200).clamp(1, logs::MAX_LINES);
    let mut out = if let Some(id) = q.source.strip_prefix("app:") {
        let snap = ctx.sh.snapshot();
        let app = snap.apps.iter().find(|a| a.id == id).ok_or_else(|| bad("Unknown app."))?;
        match (&app.container_id, id.strip_prefix("s:")) {
            (Some(cid), _) => ctx.sh.docker.logs(cid, lines, q.since, app.tty).await,
            (None, Some(unit)) => logs::journal(unit, lines, q.since).await,
            _ => Err("This app has no logs.".into()),
        }
    } else if let Some(path) = q.source.strip_prefix("file:") {
        let path = PathBuf::from(path);
        if !allowed_files(&ctx).contains(&path) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "That file is not a known log. Add it as a log-file monitor first.".into(),
            ));
        }
        tokio::task::spawn_blocking(move || logs::tail_file(&path, lines)).await.map_err(|e| bad(e.to_string()))?
    } else {
        return Err(bad("source must start with app: or file:"));
    }
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    if let Some(needle) = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty()) {
        out.retain(|l| l.text.to_lowercase().contains(&needle));
    }
    Ok(Json(json!({ "lines": out, "now": now() })))
}

/// Top processes by CPU, measured over half a second.
pub async fn processes() -> Json<Value> {
    let a = tokio::task::spawn_blocking(procs::snapshot).await.unwrap_or_default();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rows = tokio::task::spawn_blocking(move || {
        let b = procs::snapshot();
        let total = b.len();
        (procs::rows(&a, &b, 0.5, 300), total)
    })
    .await
    .unwrap_or_default();
    Json(json!({ "processes": rows.0, "total": rows.1 }))
}

/// Listening ports with the process and app that own them.
pub async fn ports(State(ctx): State<AppState>) -> Json<Value> {
    let snap = ctx.sh.snapshot();
    let mut list = tokio::task::spawn_blocking(ports::listeners).await.unwrap_or_default();
    for l in &mut list {
        l.app = app_for_port(&snap, l);
    }
    Json(json!({ "ports": list }))
}

/// Names the container or service that owns a listening socket.
fn app_for_port(snap: &crate::collect::Snapshot, l: &ports::Listener) -> Option<String> {
    let published = snap.apps.iter().find(|a| a.ports.iter().any(|p| p.public_port == Some(l.port)));
    if let Some(a) = published {
        return Some(a.name.clone());
    }
    let cg = cgroup::path_of_pid(l.pid?)?;
    snap.apps
        .iter()
        .find(|a| match &a.container_id {
            Some(id) => cg.contains(id.as_str()),
            None => a.id.strip_prefix("s:").is_some_and(|unit| cg.ends_with(&format!("/{unit}"))),
        })
        .map(|a| a.name.clone())
}

/// Saved monitors with their latest status.
pub async fn monitors(State(ctx): State<AppState>, Extension(v): Extension<Viewer>) -> Json<Value> {
    let snap = ctx.sh.snapshot();
    if !v.admin {
        // Anonymous viewers (open mode) never see URLs, hosts or paths.
        return Json(json!({ "monitors": [], "status": snap.public().monitors }));
    }
    let list = ctx.sh.monitors().list().to_vec();
    Json(json!({ "monitors": list, "status": snap.monitors }))
}

/// Adds a monitor (admin only).
pub async fn add_monitor(
    State(ctx): State<AppState>,
    Extension(v): Extension<Viewer>,
    Json(m): Json<Monitor>,
) -> ApiResult<Json<Value>> {
    require_admin(&v)?;
    let m = ctx.sh.monitors().add(m).map_err(bad)?;
    Ok(Json(json!({ "monitor": m })))
}

/// Deletes a monitor (admin only).
pub async fn delete_monitor(
    State(ctx): State<AppState>,
    Extension(v): Extension<Viewer>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&v)?;
    if !ctx.sh.monitors().remove(&id).map_err(bad)? {
        return Err(ApiError(StatusCode::NOT_FOUND, "No such monitor.".into()));
    }
    ctx.sh.check_results.lock().map(|mut r| r.remove(&id)).ok();
    Ok(Json(json!({ "ok": true })))
}

fn require_admin(v: &Viewer) -> ApiResult<()> {
    if v.admin { Ok(()) } else { Err(ApiError(StatusCode::FORBIDDEN, "Only the admin can do this.".into())) }
}

/// Firing alerts, recent history and rules; channel details only for the admin.
pub async fn alerts(State(ctx): State<AppState>, Extension(v): Extension<Viewer>) -> Json<Value> {
    let (active, history, rules) = {
        let e = ctx.sh.alerts();
        let c = e.config();
        let rules = json!({
            "cpu": c.cpu, "memory": c.memory, "disk": c.disk, "after_mins": c.after_mins, "apps": c.apps,
            "spike": c.spike, "load_per_core": c.load_per_core, "report_days": c.report_days,
            "email": c.email.is_some(),
        });
        (e.active(), e.history(), rules)
    };
    let (mut active, mut history) = (active, history);
    if !v.admin {
        let snap = ctx.sh.snapshot();
        active.iter_mut().filter(|a| !snap.alert_detail_is_public(&a.key)).for_each(|a| a.detail.clear());
        history.iter_mut().filter(|e| !snap.alert_detail_is_public(&e.key)).for_each(|e| e.detail.clear());
    }
    let channels = if v.admin { ctx.sh.notifier.describe() } else { Value::Null };
    Json(json!({ "active": active, "history": history, "rules": rules, "channels": channels }))
}

/// Sends a test alert to every channel (admin only).
pub async fn test_alert(State(ctx): State<AppState>, Extension(v): Extension<Viewer>) -> ApiResult<Json<Value>> {
    require_admin(&v)?;
    let snap_ctx = crate::alert::Context::from_snapshot(&ctx.sh.snapshot());
    let results = ctx.sh.notifier.test(&snap_ctx).await.map_err(bad)?;
    let results: Vec<Value> = results
        .into_iter()
        .map(|(channel, r)| json!({ "channel": channel, "ok": r.is_ok(), "error": r.err() }))
        .collect();
    Ok(Json(json!({ "results": results })))
}

#[derive(Deserialize)]
pub struct ReportQuery {
    range: Option<String>,
}

/// Parses a report range, defaulting to 24 hours.
fn report_range(q: &ReportQuery) -> ApiResult<(Range, &'static str)> {
    match q.range.as_deref().unwrap_or("24h") {
        "1h" => Ok((Range::H1, "1h")),
        "24h" => Ok((Range::H24, "24h")),
        "7d" => Ok((Range::D7, "7d")),
        "30d" => Ok((Range::D30, "30d")),
        _ => Err(bad("range must be 1h, 24h, 7d or 30d")),
    }
}

/// Usage summary for a time range.
pub async fn report(State(ctx): State<AppState>, Query(q): Query<ReportQuery>) -> ApiResult<Json<report::Report>> {
    let (range, label) = report_range(&q)?;
    let snap = ctx.sh.snapshot();
    Ok(Json(report::build(&ctx.sh.history(), &snap, range, label, now())))
}

#[derive(Deserialize)]
pub struct ReportEmail {
    range: Option<String>,
}

/// Emails the report for a range to the alert recipients (admin only).
pub async fn email_report(
    State(ctx): State<AppState>,
    Extension(v): Extension<Viewer>,
    Json(q): Json<ReportEmail>,
) -> ApiResult<Json<Value>> {
    require_admin(&v)?;
    let (range, label) = report_range(&ReportQuery { range: q.range })?;
    let r = report::build(&ctx.sh.history(), &ctx.sh.snapshot(), range, label, now());
    ctx.sh.notifier.send_report(&r).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({ "ok": true })))
}

/// The same report as a CSV download.
pub async fn report_csv(State(ctx): State<AppState>, Query(q): Query<ReportQuery>) -> ApiResult<Response> {
    let (range, label) = report_range(&q)?;
    let snap = ctx.sh.snapshot();
    let r = report::build(&ctx.sh.history(), &snap, range, label, now());
    let name = format!("attachment; filename=\"vps-watcher-{}-{label}.csv\"", sanitize(&snap.hostname));
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8")),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&name).unwrap_or(HeaderValue::from_static("attachment")),
            ),
        ],
        report::to_csv(&r),
    )
        .into_response())
}

/// Keeps only filename-safe characters.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(60).collect()
}
