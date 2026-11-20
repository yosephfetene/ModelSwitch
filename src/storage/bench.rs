//! The `bench_runs` and `bench_samples` tables (ADR-038).

use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::bench::{CandidateSummary, Sample, Workload};
use crate::domain::Target;
use crate::pricing::UsdAmount;

/// A run as stored, without its samples.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RunInfo {
    pub id: Uuid,
    pub alias: String,
    pub workload_name: String,
    pub workload_sha256: String,
    pub repeats: i32,
    pub created_at: OffsetDateTime,
}

/// Stores a finished run and its samples in one transaction, so a run is
/// either complete or absent, then prices the samples at the run's time.
pub async fn save(
    pool: &PgPool,
    alias: &str,
    workload: &Workload,
    repeats: u32,
    samples: &[Sample],
) -> Result<RunInfo, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let run: RunInfo = sqlx::query_as(
        "INSERT INTO bench_runs (alias, workload_name, workload_sha256, repeats) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id, alias, workload_name, workload_sha256, repeats, created_at",
    )
    .bind(alias)
    .bind(&workload.name)
    .bind(&workload.sha256)
    .bind(i32::try_from(repeats).unwrap_or(i32::MAX))
    .fetch_one(&mut *transaction)
    .await?;
    for (index, sample) in samples.iter().enumerate() {
        let number = i32::try_from(index + 1).unwrap_or(i32::MAX);
        insert_sample(&mut transaction, run.id, number, sample).await?;
    }
    // Multiplying by 0.000001, unlike dividing, never rounds (ADR-034).
    sqlx::query(
        "UPDATE bench_samples s \
         SET pricing_version = p.id, \
             estimated_cost = (s.reported_input_tokens * p.input_usd_per_million \
                 + s.reported_output_tokens * p.output_usd_per_million) * 0.000001 \
         FROM bench_runs r, pricing_versions p \
         WHERE s.run_id = $1 AND r.id = s.run_id \
           AND s.reported_input_tokens IS NOT NULL AND s.reported_output_tokens IS NOT NULL \
           AND p.provider = s.provider AND p.model = s.model \
           AND p.effective_from <= r.created_at \
           AND (p.effective_to IS NULL OR r.created_at < p.effective_to)",
    )
    .bind(run.id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(run)
}

async fn insert_sample(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    number: i32,
    sample: &Sample,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO bench_samples (\
             run_id, sample_number, item_id, repeat, provider, model, status, passed, \
             error_kind, error_code, provider_status, latency_ms, \
             reported_input_tokens, reported_output_tokens\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(run_id)
    .bind(number)
    .bind(&sample.item_id)
    .bind(i32::try_from(sample.repeat).unwrap_or(i32::MAX))
    .bind(&sample.target.provider)
    .bind(&sample.target.model)
    .bind(sample.outcome.as_str())
    .bind(sample.passed)
    .bind(sample.failure.kind)
    .bind(sample.failure.code.as_deref())
    .bind(sample.failure.provider_status.map(i32::from))
    .bind(i64::try_from(sample.latency_ms).unwrap_or(i64::MAX))
    .bind(sample.usage.input_tokens.map(i64::from))
    .bind(sample.usage.output_tokens.map(i64::from))
    .execute(&mut **transaction)
    .await
    .map(|_| ())
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    provider: String,
    model: String,
    samples: i64,
    passed: i64,
    p50_ms: Option<i64>,
    p95_ms: Option<i64>,
    mean_cost_usd: Option<String>,
    unpriced: i64,
    within_ceiling: Option<bool>,
}

/// The alias's latest run and one summary per candidate in it, by
/// provider and model. `None` if the alias was never benchmarked.
/// `ceiling` is the most mean cost per answered call that counts as
/// within it.
pub async fn latest(
    pool: &PgPool,
    alias: &str,
    ceiling: Option<&UsdAmount>,
) -> Result<Option<(RunInfo, Vec<CandidateSummary>)>, sqlx::Error> {
    let run: Option<RunInfo> = sqlx::query_as(
        "SELECT id, alias, workload_name, workload_sha256, repeats, created_at \
         FROM bench_runs WHERE alias = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(alias)
    .fetch_optional(pool)
    .await?;
    let Some(run) = run else {
        return Ok(None);
    };
    // Latency and cost cover answered calls; the mean is shown to 9
    // decimal places, but compared with the ceiling unrounded.
    let rows: Vec<SummaryRow> = sqlx::query_as(
        "SELECT provider, model, count(*) AS samples, \
                count(*) FILTER (WHERE passed) AS passed, \
                percentile_disc(0.5) WITHIN GROUP (ORDER BY latency_ms) \
                    FILTER (WHERE status = 'succeeded') AS p50_ms, \
                percentile_disc(0.95) WITHIN GROUP (ORDER BY latency_ms) \
                    FILTER (WHERE status = 'succeeded') AS p95_ms, \
                trim_scale(round(avg(estimated_cost) FILTER (WHERE status = 'succeeded'), 9))::text \
                    AS mean_cost_usd, \
                count(*) FILTER (WHERE status = 'succeeded' AND estimated_cost IS NULL) \
                    AS unpriced, \
                CASE WHEN $2::text IS NULL THEN NULL \
                     ELSE coalesce(avg(estimated_cost) FILTER (WHERE status = 'succeeded') \
                                   <= $2::text::numeric, false) \
                END AS within_ceiling \
         FROM bench_samples WHERE run_id = $1 \
         GROUP BY provider, model ORDER BY provider, model",
    )
    .bind(run.id)
    .bind(ceiling.map(UsdAmount::as_str))
    .fetch_all(pool)
    .await?;
    let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
    let ms = |n: Option<i64>| n.map(|n| u64::try_from(n).unwrap_or(0));
    let summaries = rows
        .into_iter()
        .map(|row| CandidateSummary {
            target: Target {
                provider: row.provider,
                model: row.model,
            },
            samples: count(row.samples),
            passed: count(row.passed),
            p50_ms: ms(row.p50_ms),
            p95_ms: ms(row.p95_ms),
            mean_cost_usd: row.mean_cost_usd,
            unpriced: count(row.unpriced),
            within_ceiling: row.within_ceiling,
        })
        .collect();
    Ok(Some((run, summaries)))
}
