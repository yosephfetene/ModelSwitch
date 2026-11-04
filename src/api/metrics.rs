//! `GET /metrics` on the internal metrics listener (ADR-030). Never part
//! of the public router, and it needs no API key: it must keep working
//! when the database, and so authentication, is down.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::metrics::Metrics;

/// The content type Prometheus expects for its text format.
const TEXT_FORMAT: &str = "text/plain; version=0.0.4; charset=utf-8";

pub fn router() -> Router<Arc<Metrics>> {
    Router::new().route("/metrics", get(metrics))
}

async fn metrics(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    ([(CONTENT_TYPE, TEXT_FORMAT)], metrics.render())
}
