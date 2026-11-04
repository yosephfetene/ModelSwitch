//! Rate limiting for `/v1/*` routes (ADR-031). Runs right after
//! authentication, before any body parsing or provider work, so a flood
//! is turned away at the cheapest point and bad requests count too.
//!
//! One token per client request: the gateway's own retries and fallback
//! are free, since a provider's bad minute is not the client's fault.

use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::auth::AuthenticatedKey;
use crate::api::error::ApiError;
use crate::app::AppState;
use crate::rate_limit::Decision;

pub async fn rate_limit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    // Set by the auth layer outside this one; its absence is a wiring bug.
    let Some(key) = request.extensions().get::<AuthenticatedKey>().copied() else {
        tracing::error!("rate limit: no authenticated key on the request");
        return ApiError::Internal.into_response();
    };

    match state.rate_limiter.check(key.id).await {
        Decision::Allowed => next.run(request).await,
        Decision::Limited { retry_after } => {
            tracing::info!("rate limit: request refused");
            state.metrics.record_rate_limited();
            ApiError::RateLimited {
                retry_after_secs: whole_seconds(retry_after),
            }
            .into_response()
        }
    }
}

/// `Retry-After` takes whole seconds. Rounded up, so waiting that long
/// always works, and at least 1.
fn whole_seconds(wait: Duration) -> u64 {
    wait.as_secs_f64().ceil().max(1.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        for (millis, seconds) in [(1, 1), (200, 1), (1_000, 1), (1_500, 2), (2_000, 2)] {
            assert_eq!(
                whole_seconds(Duration::from_millis(millis)),
                seconds,
                "{millis} ms"
            );
        }
    }
}
