//! The `request_records` table (ADR-027).

use sqlx::PgPool;

use crate::accounting::{RecordStore, RecordStoreError, RequestRecord};
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
}

impl RecordStore for PgRecordStore {
    fn insert<'a>(
        &'a self,
        record: &'a RequestRecord,
    ) -> BoxFuture<'a, Result<(), RecordStoreError>> {
        Box::pin(async move {
            let latency_ms = i64::try_from(record.latency_ms).unwrap_or(i64::MAX);
            // `created_at` is when the request started: the database clock
            // at insert minus the measured latency, so there is one clock.
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
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|error| RecordStoreError(error.to_string()))
        })
    }
}
