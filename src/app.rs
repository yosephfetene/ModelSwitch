use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use axum::response::{IntoResponse, Response};
use axum::{Router, middleware};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::{DefaultOnFailure, TraceLayer};
use tracing::Level;

use crate::accounting::RecordStore;
use crate::api;
use crate::api::error::ApiError;
use crate::budget::BudgetStore;
use crate::concurrency::ConcurrencyLimiter;
use crate::metrics::Metrics;
use crate::providers::Registry;
use crate::rate_limit::RateLimiter;
use crate::reliability::Limits;
use crate::routing::Catalog;
use crate::storage::DatabaseCheck;
use crate::storage::api_keys::KeyStore;

/// Shared state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    /// Dependency check used by readiness. A trait object so tests can
    /// substitute a fake database that is down, hanging, or recovering.
    pub db: Arc<dyn DatabaseCheck>,
    /// Upper bound on how long readiness waits for the database.
    pub db_check_timeout: Duration,
    /// API key lookup, a trait object for the same reason.
    pub keys: Arc<dyn KeyStore>,
    /// Upper bound on how long authentication waits for the key lookup.
    pub auth_timeout: Duration,
    /// Monthly budget checks, a trait object for the same reason. Bounded
    /// by `accounting_timeout`.
    pub budgets: Arc<dyn BudgetStore>,
    /// Where request records are written, a trait object for the same
    /// reason.
    pub records: Arc<dyn RecordStore>,
    /// Upper bound on how long a response waits for its record to be
    /// written (ADR-027).
    pub accounting_timeout: Duration,
    /// How far each request may go across retries and fallback.
    pub limits: Limits,
    /// Running totals, read by the metrics listener.
    pub metrics: Arc<Metrics>,
    /// Per-key request limits, a trait object so a shared store can
    /// replace the in-memory one.
    pub rate_limiter: Arc<dyn RateLimiter>,
    /// Per-key limits on requests running at once (ADR-032).
    pub concurrency: Arc<ConcurrencyLimiter>,
    /// Configured aliases and models, for resolving a request's `model`.
    pub catalog: Arc<Catalog>,
    /// Configured providers, ready to call.
    pub providers: Arc<Registry>,
}

/// Builds the complete HTTP application.
pub fn build_router(state: AppState) -> Router {
    let v1 = require_api_key(api::generate::router(), &state);
    with_middleware(api::health::router().merge(v1)).with_state(state)
}

/// Builds the internal metrics application, served on its own listener
/// so the public port never exposes it (ADR-030).
pub fn build_metrics_router(metrics: Arc<Metrics>) -> Router {
    with_middleware(api::metrics::router()).with_state(metrics)
}

/// Requires a valid API key on every route in `routes` (the `/v1` API),
/// then applies that key's rate limit and concurrency limit. Route
/// layers, so unknown paths still get 404 rather than 401, and the last
/// one added runs first: the limiters only ever see authenticated keys.
pub fn require_api_key(routes: Router<AppState>, state: &AppState) -> Router<AppState> {
    routes
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::concurrency::limit_concurrency,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::rate_limit::rate_limit,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::auth::require_api_key,
        ))
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
