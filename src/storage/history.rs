//! Request history from `request_records`, `request_attempts`,
//! `client_api_keys`, and `pricing_versions` (ADR-036).
//!
//! Pages are keyset-paged on `(created_at, id)`, newest first: a page
//! never repeats or skips a request, however many arrive while the
//! operator reads. Times are formatted in UTC by the database, whatever
//! the session's time zone, and costs leave it as exact decimal text.

use sqlx::PgPool;
use uuid::Uuid;

use crate::history::{
    AttemptView, HistoryFilter, HistoryPage, HistoryStore, HistoryStoreError, KeyLabel, PriceView,
    RequestDetail, RequestSummary,
};
use crate::storage::BoxFuture;

/// The columns of a `RequestSummary`, shared by the list and the detail.
const SUMMARY_COLUMNS: &str = "\
    r.id, r.client_request_id, \
    to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS created_at, \
    k.id AS key_id, k.name AS key_name, k.key_prefix, \
    r.requested_model, r.selected_provider, r.selected_model, r.status, \
    r.error_kind, r.error_code, r.provider_status, r.latency_ms, \
    r.reported_input_tokens, r.reported_output_tokens, r.reported_total_tokens, \
    r.usage_source, trim_scale(r.estimated_cost)::text AS estimated_cost, r.pricing_version, \
    (SELECT count(*) FROM request_attempts a WHERE a.request_id = r.id) AS attempt_count";

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: String,
    client_request_id: Option<String>,
    created_at: String,
    key_id: Uuid,
    key_name: String,
    key_prefix: String,
    requested_model: String,
    selected_provider: String,
    selected_model: String,
    status: String,
    error_kind: Option<String>,
    error_code: Option<String>,
    provider_status: Option<i32>,
    latency_ms: i64,
    reported_input_tokens: Option<i64>,
    reported_output_tokens: Option<i64>,
    reported_total_tokens: Option<i64>,
    usage_source: String,
    estimated_cost: Option<String>,
    pricing_version: Option<Uuid>,
    attempt_count: i64,
}

impl From<SummaryRow> for RequestSummary {
    fn from(row: SummaryRow) -> Self {
        Self {
            id: row.id,
            client_request_id: row.client_request_id,
            created_at: row.created_at,
            key: KeyLabel {
                id: row.key_id,
                name: row.key_name,
                prefix: row.key_prefix,
            },
            requested_model: row.requested_model,
            provider: row.selected_provider,
            model: row.selected_model,
            status: row.status,
            error_kind: row.error_kind,
            error_code: row.error_code,
            provider_status: row.provider_status,
            latency_ms: row.latency_ms,
            input_tokens: row.reported_input_tokens,
            output_tokens: row.reported_output_tokens,
            total_tokens: row.reported_total_tokens,
            usage_source: row.usage_source,
            estimated_cost_usd: row.estimated_cost,
            pricing_version: row.pricing_version,
            attempt_count: row.attempt_count,
        }
    }
}

#[derive(sqlx::FromRow)]
struct AttemptRow {
    attempt_number: i32,
    provider: String,
    model: String,
    status: String,
    error_kind: Option<String>,
    error_code: Option<String>,
    provider_status: Option<i32>,
    error_class: Option<String>,
    retry_decision: String,
    latency_ms: i64,
}

/// `HistoryStore` backed by PostgreSQL.
pub struct PgHistoryStore {
    pool: PgPool,
}

impl PgHistoryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn page(&self, filter: &HistoryFilter) -> Result<HistoryPage, sqlx::Error> {
        // One extra row says whether another page follows.
        let fetch = i64::from(filter.limit) + 1;
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} \
             FROM request_records r JOIN client_api_keys k ON k.id = r.client_api_key_id \
             WHERE ($1::timestamptz IS NULL OR r.created_at >= $1) \
               AND ($2::timestamptz IS NULL OR r.created_at < $2) \
               AND ($3::text IS NULL OR r.selected_provider = $3) \
               AND ($4::text IS NULL OR r.status = $4) \
               AND ($5::text IS NULL OR (r.created_at, r.id) < \
                    (SELECT c.created_at, c.id FROM request_records c WHERE c.id = $5)) \
             ORDER BY r.created_at DESC, r.id DESC \
             LIMIT $6"
        );
        let mut rows: Vec<SummaryRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(filter.from)
            .bind(filter.to)
            .bind(filter.provider.as_deref())
            .bind(filter.status.map(|status| status.as_str()))
            .bind(filter.before.as_deref())
            .bind(fetch)
            .fetch_all(&self.pool)
            .await?;

        let more = rows.len() > filter.limit as usize;
        rows.truncate(filter.limit as usize);
        let next_before = if more {
            rows.last().map(|row| row.id.clone())
        } else {
            None
        };
        Ok(HistoryPage {
            requests: rows.into_iter().map(RequestSummary::from).collect(),
            next_before,
        })
    }

    async fn detail(&self, id: &str) -> Result<Option<RequestDetail>, sqlx::Error> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} \
             FROM request_records r JOIN client_api_keys k ON k.id = r.client_api_key_id \
             WHERE r.id = $1"
        );
        let Some(summary) = sqlx::query_as::<_, SummaryRow>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };

        let attempts: Vec<AttemptRow> = sqlx::query_as(
            "SELECT attempt_number, provider, model, status, error_kind, error_code, \
                    provider_status, error_class, retry_decision, latency_ms \
             FROM request_attempts WHERE request_id = $1 ORDER BY attempt_number",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let price: Option<(String, String, String)> = match summary.pricing_version {
            Some(version) => {
                sqlx::query_as(
                    "SELECT trim_scale(input_usd_per_million)::text, \
                            trim_scale(output_usd_per_million)::text, source_reference \
                     FROM pricing_versions WHERE id = $1",
                )
                .bind(version)
                .fetch_optional(&self.pool)
                .await?
            }
            None => None,
        };

        Ok(Some(RequestDetail {
            summary: summary.into(),
            attempts: attempts
                .into_iter()
                .map(|row| AttemptView {
                    number: row.attempt_number,
                    provider: row.provider,
                    model: row.model,
                    status: row.status,
                    error_kind: row.error_kind,
                    error_code: row.error_code,
                    provider_status: row.provider_status,
                    error_class: row.error_class,
                    retry_decision: row.retry_decision,
                    latency_ms: row.latency_ms,
                })
                .collect(),
            price: price.map(|(input, output, source)| PriceView {
                input_usd_per_million: input,
                output_usd_per_million: output,
                source_reference: source,
            }),
        }))
    }
}

impl HistoryStore for PgHistoryStore {
    fn list<'a>(
        &'a self,
        filter: &'a HistoryFilter,
    ) -> BoxFuture<'a, Result<HistoryPage, HistoryStoreError>> {
        Box::pin(async move {
            self.page(filter)
                .await
                .map_err(|error| HistoryStoreError(error.to_string()))
        })
    }

    fn get<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RequestDetail>, HistoryStoreError>> {
        Box::pin(async move {
            self.detail(id)
                .await
                .map_err(|error| HistoryStoreError(error.to_string()))
        })
    }
}
