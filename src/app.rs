use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use crate::api;
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
    Router::new().merge(api::health::router()).with_state(state)
}
