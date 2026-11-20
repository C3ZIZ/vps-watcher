//! The web UI, compiled into the binary.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

const INDEX: &str = include_str!("../../web/index.html");
const JS: &str = include_str!("../../web/app.js");
const CSS: &str = include_str!("../../web/app.css");
const LOGO: &str = include_str!("../../web/logo.svg");

/// Serves a static file with a version ETag so browsers revalidate cheaply.
fn file(headers: &HeaderMap, body: &'static str, mime: &'static str) -> Response {
    let tag = HeaderValue::from_str(&format!("\"{}\"", crate::VERSION)).unwrap_or(HeaderValue::from_static("\"v\""));
    if headers.get(header::IF_NONE_MATCH) == Some(&tag) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (header::ETAG, tag),
        ],
        body,
    )
        .into_response()
}

pub async fn index(h: HeaderMap) -> Response {
    file(&h, INDEX, "text/html; charset=utf-8")
}

pub async fn js(h: HeaderMap) -> Response {
    file(&h, JS, "text/javascript; charset=utf-8")
}

pub async fn css(h: HeaderMap) -> Response {
    file(&h, CSS, "text/css; charset=utf-8")
}

pub async fn logo(h: HeaderMap) -> Response {
    file(&h, LOGO, "image/svg+xml")
}
