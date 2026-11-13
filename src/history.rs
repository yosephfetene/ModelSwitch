//! Request history for the dashboard (Stage 4, ADR-036).
//!
//! A read-only view of request records and their attempts. Like the
//! records themselves, it holds identifiers, numbers, and times, never a
//! prompt, an output, or an API key: keys appear only by ID, name, and
//! display prefix.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::accounting::Outcome;
use crate::domain::BoxFuture;

/// Most requests one page may hold.
pub const MAX_PAGE_SIZE: u32 = 200;
/// Requests per page when none is asked for.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Which requests to list. Newest first; every bound is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryFilter {
    /// Started at or after this moment.
    pub from: Option<DateTime<Utc>>,
    /// Started before this moment.
    pub to: Option<DateTime<Utc>>,
    /// The provider that answered last (the record's selected provider).
    pub provider: Option<String>,
    pub status: Option<Outcome>,
    /// Only requests older than this one: the previous page's
    /// `next_before`.
    pub before: Option<String>,
    /// 1 to `MAX_PAGE_SIZE`.
    pub limit: u32,
}

impl Default for HistoryFilter {
    fn default() -> Self {
        Self {
            from: None,
            to: None,
            provider: None,
            status: None,
            before: None,
            limit: DEFAULT_PAGE_SIZE,
        }
    }
}

/// The key that sent a request, as the dashboard may show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLabel {
    pub id: Uuid,
    pub name: String,
    /// The display prefix, such as `ms_dev_7Hq2`: never the key.
    pub prefix: String,
}

/// One recorded request, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSummary {
    pub id: String,
    pub client_request_id: Option<String>,
    /// When the request started, in UTC with milliseconds.
    pub created_at: String,
    pub key: KeyLabel,
    /// The alias or pin as the client sent it.
    pub requested_model: String,
    /// What answered last.
    pub provider: String,
    pub model: String,
    /// `succeeded`, `failed`, or `timed_out`.
    pub status: String,
    pub error_kind: Option<String>,
    pub error_code: Option<String>,
    pub provider_status: Option<i32>,
    pub latency_ms: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    /// `provider_reported` or `unavailable`.
    pub usage_source: String,
    /// Exact decimal USD, such as `0.0000375`; `None` when unknown.
    pub estimated_cost_usd: Option<String>,
    pub pricing_version: Option<Uuid>,
    pub attempt_count: i64,
}

/// One provider call of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub number: i32,
    pub provider: String,
    pub model: String,
    /// `succeeded`, `failed`, or `timed_out`.
    pub status: String,
    pub error_kind: Option<String>,
    pub error_code: Option<String>,
    pub provider_status: Option<i32>,
    /// `transient`, `target_problem`, or `request_problem`.
    pub error_class: Option<String>,
    /// `not_retried`, `retry`, `fallback`, or `exhausted`.
    pub retry_decision: String,
    pub latency_ms: i64,
}

/// The price a cost estimate used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceView {
    pub input_usd_per_million: String,
    pub output_usd_per_million: String,
    pub source_reference: String,
}

/// One request with everything recorded about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDetail {
    pub summary: RequestSummary,
    /// In the order they were made.
    pub attempts: Vec<AttemptView>,
    pub price: Option<PriceView>,
}

/// A page of requests, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub requests: Vec<RequestSummary>,
    /// Pass as `before` for the next page; `None` on the last page.
    pub next_before: Option<String>,
}

/// Why history could not be read. For logs only.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HistoryStoreError(pub String);

/// The test seam between the dashboard API and the database. The caller
/// bounds each read with its own deadline.
pub trait HistoryStore: Send + Sync {
    fn list<'a>(
        &'a self,
        filter: &'a HistoryFilter,
    ) -> BoxFuture<'a, Result<HistoryPage, HistoryStoreError>>;

    /// `None` when no request has this ID.
    fn get<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RequestDetail>, HistoryStoreError>>;
}

/// Whether `id` has the shape of a gateway request ID: `req_` and a
/// UUIDv7. Anything else cannot be in the database, so it needs no query.
pub fn is_request_id(id: &str) -> bool {
    id.strip_prefix("req_")
        .and_then(|uuid| Uuid::try_parse(uuid).ok())
        .is_some_and(|uuid| {
            uuid.get_version_num() == 7 && id.len() == 40 && id == id.to_ascii_lowercase()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_req_and_a_lowercase_uuid_v7() {
        let good = format!("req_{}", Uuid::now_v7());
        assert!(is_request_id(&good));

        for bad in [
            String::new(),
            Uuid::now_v7().to_string(),
            "req_6f9619ff-8b86-4011-b42d-00c04fc964ff".to_owned(),
            format!("req_{}", Uuid::now_v7().simple()),
            good.to_ascii_uppercase().replace("REQ_", "req_"),
            format!("{good}x"),
            "req_' OR 1=1 --".to_owned(),
        ] {
            assert!(!is_request_id(&bad), "{bad}");
        }
    }
}
