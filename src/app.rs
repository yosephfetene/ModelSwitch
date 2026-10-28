use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use axum::response::{IntoResponse, Response};
use axum::{Router, middleware};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::{DefaultOnFailure, TraceLayer};
use tracing::Level;

use crate::api;
use crate::api::error::ApiError;
use crate::storage::DatabaseCheck;

/// Shared state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    /// Dependency check used by readiness. A trait object so tests can
    /// substitute a fake database that is down, hanging, or recovering.
    pub db: Arc<dyn DatabaseCheck>,
    /// Upper bound on how long readiness waits for the database.
    pub db_check_timeout: Duration,
}

/// Builds the complete HTTP application.
pub fn build_router(state: AppState) -> Router {
    with_middleware(Router::new().merge(api::health::router())).with_state(state)
}

/// Adds the fallbacks and middleware every route shares. Public so tests
/// can wrap routes of their own, such as one that panics.
///
/// Layers run outside-in: the request context (IDs, span, error bodies)
/// wraps tracing, which wraps panic recovery, which wraps the routes.
pub fn with_middleware<S>(routes: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    routes
        .fallback(|| async { ApiError::NotFound })
        .method_not_allowed_fallback(|| async { ApiError::MethodNotAllowed })
        .layer(CatchPanicLayer::custom(panic_response))
        // Requests and responses log at DEBUG without headers, so frequent
        // health polling does not flood INFO logs. Failures (5xx) are also
        // lowered to DEBUG: the code that knows the cause logs it once
        // (readiness WARNs, panics ERROR), and the default ERROR here would
        // repeat on every readiness poll during a database outage.
        .layer(TraceLayer::new_for_http().on_failure(DefaultOnFailure::new().level(Level::DEBUG)))
        .layer(middleware::from_fn(api::request_context::request_context))
}

/// A panicking handler fails only its own request: the detail goes to
/// the logs (inside the request span), the client gets a generic 500.
fn panic_response(panic: Box<dyn Any + Send + 'static>) -> Response {
    let detail = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    tracing::error!(panic = detail, "handler panicked");
    ApiError::Internal.into_response()
}
