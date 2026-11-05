//! Concurrency limiting for `/v1/*` routes (ADR-032). Runs after the rate
//! limit, so a refused request has still spent a token: it did arrive.
//!
//! The seat is held for the whole of the inner service, so one request
//! holds one seat across all its retries and fallback.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::auth::AuthenticatedKey;
use crate::api::error::ApiError;
use crate::app::AppState;

pub async fn limit_concurrency(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    // Set by the auth layer outside this one; its absence is a wiring bug.
    let Some(key) = request.extensions().get::<AuthenticatedKey>().copied() else {
        tracing::error!("concurrency limit: no authenticated key on the request");
        return ApiError::Internal.into_response();
    };

    let Some(_seat) = state.concurrency.try_acquire(key.id) else {
        tracing::info!("concurrency limit: request refused");
        state.metrics.record_concurrency_limited();
        return ApiError::ConcurrencyLimited.into_response();
    };
    next.run(request).await
}
