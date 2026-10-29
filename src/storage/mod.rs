pub mod api_keys;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use secrecy::ExposeSecret;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::config::DatabaseConfig;

/// A boxed future, so `DatabaseCheck` can be used as a trait object
/// (`Arc<dyn DatabaseCheck>`). Plain `async fn` in traits is not yet
/// usable through `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why a database ping failed. The message is for logs only; it must
/// never be copied into an HTTP response.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PingError(pub String);

/// The test seam between readiness and the real database.
pub trait DatabaseCheck: Send + Sync {
    fn ping(&self) -> BoxFuture<'_, Result<(), PingError>>;
}

/// `DatabaseCheck` backed by a PostgreSQL connection pool.
pub struct PgDatabaseCheck {
    pool: PgPool,
}

impl PgDatabaseCheck {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl DatabaseCheck for PgDatabaseCheck {
    fn ping(&self) -> BoxFuture<'_, Result<(), PingError>> {
        Box::pin(async move {
            sqlx::query("SELECT 1")
                .execute(&self.pool)
                .await
                .map(|_| ())
                .map_err(|error| PingError(error.to_string()))
        })
    }
}

/// The SQL files in `migrations/`, embedded in the binary at compile time
/// so a deployed binary always carries the schema it expects.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Creates the pool without connecting. The service starts even when the
/// database is down and reports that through readiness instead of
/// refusing to boot.
pub fn connect_lazy(config: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy(config.url.expose_secret())
}
