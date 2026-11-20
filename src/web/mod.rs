//! HTTP server: routing, the security middleware and startup.

mod api;
mod assets;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};

use crate::auth::Auth;
use crate::collect::{self, Shared};
use crate::config::{AuthMode, Config};
use crate::{info, warn};

/// Open connections allowed at once; more are closed immediately.
const MAX_CONNECTIONS: usize = 256;
/// Time a client gets to send request headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// Time a request (including its body) may take before it is answered with 408.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest request body; the API only ever receives small JSON.
const MAX_BODY: usize = 16 * 1024;

/// Everything handlers need.
pub struct Ctx {
    pub sh: Arc<Shared>,
    pub auth: Auth,
}

pub type AppState = Arc<Ctx>;

/// Who is making a request, attached by the guard.
#[derive(Clone)]
pub struct Viewer {
    pub admin: bool,
    pub token: Option<String>,
}

pub const COOKIE: &str = "vpsw_session";

/// Starts the collector and serves the UI until SIGINT or SIGTERM.
pub fn serve(cfg: Config) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let auth = Auth::load(&cfg.data_dir)?;
        let listen = cfg.listen;
        let (sh, alert_rx) = collect::init(cfg)?;
        let listener =
            tokio::net::TcpListener::bind(listen).await.map_err(|e| format!("cannot listen on {listen}: {e}"))?;
        info!("vps-watcher {} listening on {listen}", crate::VERSION);
        let local =
            if listen.ip().is_unspecified() { format!("127.0.0.1:{}", listen.port()) } else { listen.to_string() };
        if auth.needs_setup() {
            info!("no admin yet: open http://{local} from this server (or an SSH tunnel) to create one");
        }
        tokio::spawn(collect::run(sh.clone()));
        tokio::spawn(collect::run_checks(sh.clone()));
        tokio::spawn(crate::alert::run(sh.notifier.clone(), alert_rx));
        tokio::spawn(collect::run_reports(sh.clone()));
        let state: AppState = Arc::new(Ctx { sh: sh.clone(), auth });
        tokio::select! {
            _ = accept_loop(listener, router(state)) => {}
            _ = shutdown_signal() => info!("shutting down"),
        }
        sh.save_history();
        Ok(())
    })
}

/// Accepts connections with a cap, and serves each with header and keep-alive timeouts.
async fn accept_loop(listener: tokio::net::TcpListener, app: Router) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                // Usually EMFILE; back off instead of spinning.
                warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
        let svc = hyper_util::service::TowerToHyperService::new(app.clone().layer(axum::Extension(ConnectInfo(peer))));
        tokio::spawn(async move {
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .serve_connection(hyper_util::rt::TokioIo::new(stream), svc);
            let _ = conn.await;
            drop(slot);
        });
    }
}

/// Resolves on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => return std::future::pending().await,
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// All routes behind the guard.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(assets::index))
        .route("/app.js", get(assets::js))
        .route("/app.css", get(assets::css))
        .route("/logo.svg", get(assets::logo))
        .route("/api/session", get(api::session))
        .route("/api/setup", post(api::setup))
        .route("/api/login", post(api::login))
        .route("/api/logout", post(api::logout))
        .route("/api/account", post(api::account))
        .route("/api/logout-others", post(api::logout_others))
        .route("/api/overview", get(api::overview))
        .route("/api/live", get(api::live))
        .route("/api/history", get(api::history))
        .route("/api/log-sources", get(api::log_sources))
        .route("/api/logs", get(api::logs))
        .route("/api/processes", get(api::processes))
        .route("/api/ports", get(api::ports))
        .route("/api/monitors", get(api::monitors).post(api::add_monitor))
        .route("/api/monitors/{id}", delete(api::delete_monitor))
        .route("/api/alerts", get(api::alerts))
        .route("/api/alerts/test", post(api::test_alert))
        .route("/api/report", get(api::report))
        .route("/api/report.csv", get(api::report_csv))
        .route("/api/report/email", post(api::email_report))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

/// API paths anyone may call (they do their own checks).
const PUBLIC_API: [&str; 4] = ["/api/session", "/api/setup", "/api/login", "/api/logout"];
/// Paths that can reveal secrets (logs, command lines); public in open mode only with VPSW_OPEN_LOGS.
const SENSITIVE: [&str; 3] = ["/api/logs", "/api/log-sources", "/api/processes"];

/// Enforces CSRF rules and access mode, then adds security headers.
async fn guard(State(ctx): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let headers = req.headers();

    if method != Method::GET && method != Method::HEAD {
        if !same_origin(headers) {
            return secure(api::err(StatusCode::FORBIDDEN, "Cross-site request blocked."));
        }
        if method == Method::POST && !is_json(headers) {
            return secure(api::err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Send JSON."));
        }
    }

    let token = cookie(headers, COOKIE);
    let admin = token.as_deref().and_then(|t| ctx.auth.session(t)).is_some();
    if path.starts_with("/api/") && !PUBLIC_API.contains(&path.as_str()) && !admin {
        let cfg = &ctx.sh.cfg;
        let sensitive = SENSITIVE.contains(&path.as_str());
        let open_ok = cfg.auth == AuthMode::Open && method == Method::GET && (!sensitive || cfg.open_logs);
        if !open_ok {
            return secure(api::err(StatusCode::UNAUTHORIZED, "Sign in required."));
        }
    }
    req.extensions_mut().insert(Viewer { admin, token });
    // Bounds slow request bodies; streaming responses (live updates) return before this fires.
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(req)).await {
        Ok(res) => secure(res),
        Err(_) => secure(api::err(StatusCode::REQUEST_TIMEOUT, "Request took too long.")),
    }
}

/// Adds strict security headers to every response.
fn secure(mut res: Response) -> Response {
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: header::HeaderName, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(
        h,
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; \
         frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    );
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(h, header::REFERRER_POLICY, "no-referrer");
    set(h, header::X_FRAME_OPTIONS, "DENY");
    set(h, header::HeaderName::from_static("permissions-policy"), "camera=(), microphone=(), geolocation=()");
    if !h.contains_key(header::CACHE_CONTROL) {
        set(h, header::CACHE_CONTROL, "no-store");
    }
    res
}

/// Rejects requests whose Origin (or Sec-Fetch-Site) shows another site.
fn same_origin(h: &HeaderMap) -> bool {
    if h.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_some_and(|v| v == "cross-site") {
        return false;
    }
    let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else { return true };
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    !host.is_empty() && origin_host.eq_ignore_ascii_case(host)
}

fn is_json(h: &HeaderMap) -> bool {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json")))
}

/// Value of one cookie from the Cookie header.
pub fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn origin_rules() {
        assert!(same_origin(&hm(&[("host", "a.com")])), "no Origin: non-browser client");
        assert!(same_origin(&hm(&[("host", "a.com"), ("origin", "https://a.com")])));
        assert!(!same_origin(&hm(&[("host", "a.com"), ("origin", "https://evil.com")])));
        assert!(!same_origin(&hm(&[("host", "a.com"), ("sec-fetch-site", "cross-site")])));
        assert!(same_origin(&hm(&[("host", "127.0.0.1:9100"), ("origin", "http://127.0.0.1:9100")])));
    }

    #[test]
    fn json_and_cookies() {
        assert!(is_json(&hm(&[("content-type", "application/json; charset=utf-8")])));
        assert!(!is_json(&hm(&[("content-type", "text/plain")])));
        let h = hm(&[("cookie", "a=1; vpsw_session=abc"), ("cookie", "b=2")]);
        assert_eq!(cookie(&h, "vpsw_session").as_deref(), Some("abc"));
        assert_eq!(cookie(&h, "zzz"), None);
    }
}
