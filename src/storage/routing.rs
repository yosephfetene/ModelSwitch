//! Adaptive routing data from PostgreSQL (ADR-039): the alias's latest
//! benchmark and its candidates' live attempts within the window.

use sqlx::PgPool;

use crate::domain::Target;
use crate::routing::adaptive::{
    AdaptivePolicy, LiveStat, RoutingData, RoutingSource, RoutingSourceError,
};
use crate::storage::{BoxFuture, bench};

/// `RoutingSource` backed by PostgreSQL. The caller bounds each load with
/// its own deadline.
pub struct PgRoutingSource {
    pool: PgPool,
}

impl PgRoutingSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn read(
        &self,
        candidates: &[Target],
        alias: &str,
        policy: &AdaptivePolicy,
    ) -> Result<RoutingData, sqlx::Error> {
        let bench = bench::latest(&self.pool, alias, policy.max_mean_cost.as_ref())
            .await?
            .map(|(_, summaries)| summaries);
        Ok(RoutingData {
            bench,
            live: live(&self.pool, candidates, policy).await?,
        })
    }
}

impl RoutingSource for PgRoutingSource {
    fn load<'a>(
        &'a self,
        candidates: &'a [Target],
        alias: &'a str,
        policy: &'a AdaptivePolicy,
    ) -> BoxFuture<'a, Result<RoutingData, RoutingSourceError>> {
        Box::pin(async move {
            self.read(candidates, alias, policy)
                .await
                .map_err(|error| RoutingSourceError(error.to_string()))
        })
    }
}

#[derive(sqlx::FromRow)]
struct LiveRow {
    provider: String,
    model: String,
    attempts: i64,
    succeeded: i64,
    p50_ms: Option<i64>,
}

/// Every attempt on the candidates by requests that started within the
/// window, through any alias or pin: a provider's speed is the same
/// whoever asked.
pub async fn live(
    pool: &PgPool,
    candidates: &[Target],
    policy: &AdaptivePolicy,
) -> Result<Vec<LiveStat>, sqlx::Error> {
    let providers: Vec<&str> = candidates.iter().map(|t| t.provider.as_str()).collect();
    let models: Vec<&str> = candidates.iter().map(|t| t.model.as_str()).collect();
    let rows: Vec<LiveRow> = sqlx::query_as(
        "SELECT a.provider, a.model, count(*) AS attempts, \
                count(*) FILTER (WHERE a.status = 'succeeded') AS succeeded, \
                percentile_disc(0.5) WITHIN GROUP (ORDER BY a.latency_ms) \
                    FILTER (WHERE a.status = 'succeeded') AS p50_ms \
         FROM request_attempts a JOIN request_records r ON r.id = a.request_id \
         WHERE r.created_at >= now() - make_interval(secs => $1) \
           AND (a.provider, a.model) IN (SELECT * FROM unnest($2::text[], $3::text[])) \
         GROUP BY a.provider, a.model",
    )
    .bind(policy.window.as_secs_f64())
    .bind(&providers)
    .bind(&models)
    .fetch_all(pool)
    .await?;
    let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
    Ok(rows
        .into_iter()
        .map(|row| LiveStat {
            target: Target {
                provider: row.provider,
                model: row.model,
            },
            attempts: count(row.attempts),
            succeeded: count(row.succeeded),
            p50_ms: row.p50_ms.map(|ms| u64::try_from(ms).unwrap_or(0)),
        })
        .collect())
}
