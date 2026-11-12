//! `GET /admin/api/requests` and `GET /admin/api/requests/{id}`: request
//! history for the dashboard (ADR-036). Behind the admin session.
//!
//! Every filter is checked before the database is asked, and each read
//! runs under the admin deadline; if it fails or times out the answer is
//! `503`, never a partial page. Costs are exact decimal strings, because
//! a JSON number would round them.

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::accounting::Outcome;
use crate::api::admin::AdminState;
use crate::api::error::ApiError;
use crate::history::{
    DEFAULT_PAGE_SIZE, HistoryFilter, MAX_PAGE_SIZE, RequestDetail, RequestSummary, is_request_id,
};

const MAX_PROVIDER_CHARS: usize = 64;

pub fn router() -> Router<AdminState> {
    Router::new()
        .route("/admin/api/requests", get(list))
        .route("/admin/api/requests/{id}", get(detail))
}

async fn list(
    State(state): State<AdminState>,
    query: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(params) =
        query.map_err(|_| ApiError::InvalidRequest("The query string could not be read."))?;
    let filter = parse_filter(&params)?;

    let page = tokio::time::timeout(state.database_timeout, state.history.list(&filter))
        .await
        .map_err(|_| unavailable("timed out"))?
        .map_err(|error| unavailable(&error.to_string()))?;

    Ok(Json(json!({
        "requests": page.requests.iter().map(summary_json).collect::<Vec<_>>(),
        "next_before": page.next_before,
    })))
}

async fn detail(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // An ID that cannot exist needs no query.
    if !is_request_id(&id) {
        return Err(ApiError::RequestNotFound);
    }
    let found = tokio::time::timeout(state.database_timeout, state.history.get(&id))
        .await
        .map_err(|_| unavailable("timed out"))?
        .map_err(|error| unavailable(&error.to_string()))?;
    match found {
        Some(detail) => Ok(Json(detail_json(&detail))),
        None => Err(ApiError::RequestNotFound),
    }
}

fn unavailable(reason: &str) -> ApiError {
    tracing::warn!(reason, "admin: history read failed");
    ApiError::ServiceUnavailable
}

/// Turns the query string into a filter, or names the first bad field.
pub fn parse_filter(params: &HashMap<String, String>) -> Result<HistoryFilter, ApiError> {
    if let Some(unknown) = params.keys().find(|key| {
        !matches!(
            key.as_str(),
            "from" | "to" | "provider" | "status" | "before" | "limit"
        )
    }) {
        tracing::debug!(parameter = %unknown.chars().take(32).collect::<String>(), "unknown");
        return Err(ApiError::InvalidRequest(
            "Allowed parameters are from, to, provider, status, before, and limit.",
        ));
    }

    let time = |name: &str, message| -> Result<Option<DateTime<Utc>>, ApiError> {
        params
            .get(name)
            .map(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|_| ApiError::InvalidRequest(message))
            })
            .transpose()
    };
    let from = time(
        "from",
        "\"from\" must be an RFC 3339 time, such as 2026-11-12T00:00:00Z.",
    )?;
    let to = time(
        "to",
        "\"to\" must be an RFC 3339 time, such as 2026-11-12T00:00:00Z.",
    )?;
    if let (Some(from), Some(to)) = (from, to)
        && from >= to
    {
        return Err(ApiError::InvalidRequest("\"from\" must be before \"to\"."));
    }

    let provider = params
        .get("provider")
        .map(|value| {
            let valid = !value.is_empty()
                && value.chars().count() <= MAX_PROVIDER_CHARS
                && value.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                });
            if valid {
                Ok(value.clone())
            } else {
                Err(ApiError::InvalidRequest(
                    "\"provider\" must be a provider name: lowercase letters, digits, - and _.",
                ))
            }
        })
        .transpose()?;

    let status = params
        .get("status")
        .map(|value| match value.as_str() {
            "succeeded" => Ok(Outcome::Succeeded),
            "failed" => Ok(Outcome::Failed),
            "timed_out" => Ok(Outcome::TimedOut),
            _ => Err(ApiError::InvalidRequest(
                "\"status\" must be succeeded, failed, or timed_out.",
            )),
        })
        .transpose()?;

    let before = params
        .get("before")
        .map(|value| {
            if is_request_id(value) {
                Ok(value.clone())
            } else {
                Err(ApiError::InvalidRequest(
                    "\"before\" must be a request ID from \"next_before\".",
                ))
            }
        })
        .transpose()?;

    let limit = match params.get("limit") {
        None => DEFAULT_PAGE_SIZE,
        Some(value) => value
            .parse::<u32>()
            .ok()
            .filter(|limit| (1..=MAX_PAGE_SIZE).contains(limit))
            .ok_or(ApiError::InvalidRequest(
                "\"limit\" must be between 1 and 200.",
            ))?,
    };

    Ok(HistoryFilter {
        from,
        to,
        provider,
        status,
        before,
        limit,
    })
}

fn summary_json(summary: &RequestSummary) -> Value {
    let error = summary.error_kind.as_ref().map(|kind| {
        json!({
            "kind": kind,
            "code": summary.error_code,
            "provider_status": summary.provider_status,
        })
    });
    let cost = summary
        .estimated_cost_usd
        .as_ref()
        .map(|usd| json!({ "usd": usd, "pricing_version": summary.pricing_version.map(|v| v.to_string()) }));
    json!({
        "id": summary.id,
        "client_request_id": summary.client_request_id,
        "created_at": summary.created_at,
        "key": {
            "id": summary.key.id.to_string(),
            "name": summary.key.name,
            "prefix": summary.key.prefix,
        },
        "requested_model": summary.requested_model,
        "provider": summary.provider,
        "model": summary.model,
        "status": summary.status,
        "error": error,
        "latency_ms": summary.latency_ms,
        "usage": {
            "input_tokens": summary.input_tokens,
            "output_tokens": summary.output_tokens,
            "total_tokens": summary.total_tokens,
            "source": summary.usage_source,
        },
        "cost": cost,
        "attempt_count": summary.attempt_count,
    })
}

fn detail_json(detail: &RequestDetail) -> Value {
    let mut value = summary_json(&detail.summary);
    value["attempts"] = detail
        .attempts
        .iter()
        .map(|attempt| {
            let error = attempt.error_kind.as_ref().map(|kind| {
                json!({
                    "kind": kind,
                    "code": attempt.error_code,
                    "provider_status": attempt.provider_status,
                    "class": attempt.error_class,
                })
            });
            json!({
                "number": attempt.number,
                "provider": attempt.provider,
                "model": attempt.model,
                "status": attempt.status,
                "error": error,
                "retry_decision": attempt.retry_decision,
                "latency_ms": attempt.latency_ms,
            })
        })
        .collect();
    value["price"] = detail.price.as_ref().map_or(Value::Null, |price| {
        json!({
            "input_usd_per_million": price.input_usd_per_million,
            "output_usd_per_million": price.output_usd_per_million,
            "source": price.source_reference,
        })
    });
    value
}
