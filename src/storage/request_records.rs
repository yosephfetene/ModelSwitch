//! The `request_records` and `request_attempts` tables (ADR-027,
//! ADR-029).

use sqlx::{PgPool, Postgres, Transaction};

use crate::accounting::{AttemptRecord, Cost, RecordStore, RecordStoreError, RequestRecord};
use crate::storage::BoxFuture;

/// `RecordStore` backed by PostgreSQL. The caller bounds each write with
/// its own deadline.
pub struct PgRecordStore {
    pool: PgPool,
}

impl PgRecordStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The request and its attempts in one transaction, so neither can
    /// exist without the other. If the caller's deadline drops this
    /// future, the transaction rolls back.
    async fn write(&self, record: &RequestRecord) -> Result<Cost, sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        insert_request(&mut transaction, record).await?;
        for (index, attempt) in record.attempts.iter().enumerate() {
            let number = i32::try_from(index + 1).unwrap_or(i32::MAX);
            insert_attempt(&mut transaction, &record.request_id, number, attempt).await?;
        }
        let cost = estimate_cost(&mut transaction, record).await?;
        transaction.commit().await?;
        Ok(cost)
    }
}

impl RecordStore for PgRecordStore {
    fn insert<'a>(
        &'a self,
        record: &'a RequestRecord,
    ) -> BoxFuture<'a, Result<Cost, RecordStoreError>> {
        Box::pin(async move {
            self.write(record)
                .await
                .map_err(|error| RecordStoreError(error.to_string()))
        })
    }
}

async fn insert_request(
    transaction: &mut Transaction<'_, Postgres>,
    record: &RequestRecord,
) -> Result<(), sqlx::Error> {
    let latency_ms = i64::try_from(record.latency_ms).unwrap_or(i64::MAX);
    // `created_at` is when the request started: the database clock at
    // insert minus the measured latency, so there is one clock.
    sqlx::query(
        "INSERT INTO request_records (\
             id, client_request_id, client_api_key_id, requested_model, \
             selected_provider, selected_model, status, error_kind, error_code, \
             provider_status, latency_ms, reported_input_tokens, \
             reported_output_tokens, reported_total_tokens, usage_source, created_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
             now() - $11::double precision * interval '1 millisecond'\
         )",
    )
    .bind(&record.request_id)
    .bind(record.client_request_id.as_deref())
    .bind(record.client_api_key_id)
    .bind(&record.requested_model)
    .bind(&record.provider)
    .bind(&record.model)
    .bind(record.outcome.as_str())
    .bind(record.failure.kind)
    .bind(record.failure.code.as_deref())
    .bind(record.failure.provider_status.map(i32::from))
    .bind(latency_ms)
    .bind(record.usage.input_tokens.map(i64::from))
    .bind(record.usage.output_tokens.map(i64::from))
    .bind(record.usage.total_tokens.map(i64::from))
    .bind(record.usage_source())
    .execute(&mut **transaction)
    .await
    .map(|_| ())
}

/// Prices a stored request from its reported tokens and the price in
/// effect when it started (ADR-034). In the database because `numeric`
/// is exact, and `created_at` and the prices share its clock. Multiplying
/// by 0.000001, unlike dividing, never rounds.
async fn estimate_cost(
    transaction: &mut Transaction<'_, Postgres>,
    record: &RequestRecord,
) -> Result<Cost, sqlx::Error> {
    if record.usage.input_tokens.is_none() || record.usage.output_tokens.is_none() {
        return Ok(Cost::NoUsage);
    }
    let priced = sqlx::query(
        "UPDATE request_records r \
         SET pricing_version = p.id, \
             estimated_cost = (r.reported_input_tokens * p.input_usd_per_million \
                 + r.reported_output_tokens * p.output_usd_per_million) * 0.000001 \
         FROM pricing_versions p \
         WHERE r.id = $1 \
           AND p.provider = r.selected_provider AND p.model = r.selected_model \
           AND p.effective_from <= r.created_at \
           AND (p.effective_to IS NULL OR r.created_at < p.effective_to)",
    )
    .bind(&record.request_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    Ok(if priced == 0 {
        Cost::Unpriced
    } else {
        Cost::Estimated
    })
}

async fn insert_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: &str,
    number: i32,
    attempt: &AttemptRecord,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO request_attempts (\
             request_id, attempt_number, provider, model, status, error_kind, error_code, \
             provider_status, error_class, retry_decision, latency_ms\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(request_id)
    .bind(number)
    .bind(&attempt.provider)
    .bind(&attempt.model)
    .bind(attempt.outcome.as_str())
    .bind(attempt.failure.kind)
    .bind(attempt.failure.code.as_deref())
    .bind(attempt.failure.provider_status.map(i32::from))
    .bind(attempt.class.map(|class| class.as_str()))
    .bind(attempt.decision.as_str())
    .bind(i64::try_from(attempt.latency_ms).unwrap_or(i64::MAX))
    .execute(&mut **transaction)
    .await
    .map(|_| ())
}
