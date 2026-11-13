//! The dashboard's page files, on the admin listener (ADR-036).
//!
//! Only two things are served from `admin.dashboard_dir`: `index.html` at
//! `/`, and the files under `assets/` at `/assets/...`. Nothing else in
//! the folder is reachable, and paths cannot climb out of it. Every page
//! response carries a Content Security Policy that lets the page load
//! scripts, styles, and data only from this listener, and forbids other
//! sites from framing it.

use std::path::Path;

use axum::Router;
use axum::handler::HandlerWithoutStateExt;
use axum::http::HeaderValue;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::middleware;
use axum::response::Response;
use tower_http::services::{ServeDir, ServeFile};

use crate::api::error::ApiError;

/// Same-origin everything, no plugins, no framing, no inline scripts.
const CONTENT_SECURITY: &str = "default-src 'self'; base-uri 'none'; form-action 'none'; \
                                frame-ancestors 'none'; object-src 'none'";

/// Routes for the built dashboard in `dir`: `index.html` and `assets/`.
pub fn router<S>(dir: &Path) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let not_found = (|| async { ApiError::NotFound }).into_service();
    let assets = ServeDir::new(dir.join("assets"))
        .append_index_html_on_directories(false)
        .call_fallback_on_method_not_allowed(true)
        .not_found_service(not_found);
    Router::new()
        .route_service("/", ServeFile::new(dir.join("index.html")))
        .nest_service("/assets", assets)
        .layer(middleware::map_response(page_headers))
}

async fn page_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (CONTENT_SECURITY_POLICY, CONTENT_SECURITY),
        (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (X_FRAME_OPTIONS, "DENY"),
        (REFERRER_POLICY, "no-referrer"),
        // Check for a newer build on every load; files are small.
        (CACHE_CONTROL, "no-cache"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}
