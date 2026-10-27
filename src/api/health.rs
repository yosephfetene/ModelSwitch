//! Liveness and readiness endpoints.
//!
//! - `/health/live` checks nothing external: if this handler runs, the
//!   process is alive. Checking the database here would make platforms
//!   restart every instance during a database outage.
//! - `/health/ready` checks only PostgreSQL, under a deadline. Providers
//!   are deliberately not checked: fallback keeps the gateway useful when
//!   one is down, and polling them would burn free-tier quota.
//!
//! Neither endpoint requires an API key. Failure details go to the logs,
//! never to the response body.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::trace::{DefaultOnFailure, TraceLayer};
use tracing::Level;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        // Requests and responses log at DEBUG without headers, so frequent
        // platform polling does not flood INFO logs. Failures (503s) are
        // also lowered to DEBUG: `ready` already logs one WARN with the
        // reason, and the default ERROR would repeat on every poll.
        .layer(TraceLayer::new_for_http().on_failure(DefaultOnFailure::new().level(Level::DEBUG)))
}

#[derive(Debug, Serialize)]
struct LiveBody {
    status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReadyStatus {
    Ready,
    NotReady,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct Checks {
    database: CheckStatus,
}

#[derive(Debug, Serialize)]
struct ReadyBody {
    status: ReadyStatus,
    checks: Checks,
}

async fn live() -> Json<LiveBody> {
    Json(LiveBody { status: "ok" })
}

async fn ready(State(state): State<AppState>) -> (StatusCode, Json<ReadyBody>) {
    let database = match tokio::time::timeout(state.db_check_timeout, state.db.ping()).await {
        Ok(Ok(())) => CheckStatus::Ok,
        Ok(Err(error)) => {
            tracing::warn!(%error, "readiness: database ping failed");
            CheckStatus::Unavailable
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = state.db_check_timeout.as_millis() as u64,
                "readiness: database ping timed out"
            );
            CheckStatus::Unavailable
        }
    };

    let (code, status) = match database {
        CheckStatus::Ok => (StatusCode::OK, ReadyStatus::Ready),
        CheckStatus::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, ReadyStatus::NotReady),
    };

    (
        code,
        Json(ReadyBody {
            status,
            checks: Checks { database },
        }),
    )
}
