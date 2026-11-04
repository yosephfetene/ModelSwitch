//! `POST /v1/generate` (API.md).
//!
//! Order of work, cheapest and safest first: authentication (route
//! layer), then body size, content type, and JSON validation, then model
//! resolution. Only a fully valid request reaches a provider. The
//! executor then tries the candidates, with retries and fallback, within
//! the attempt and time limits (ADR-028).
//!
//! Every request that reaches a provider gets a request record, written
//! under its own deadline before responding (ADR-027).
//!
//! Prompts and outputs are never logged (ADR-005); log lines carry the
//! target, latency, and outcome.

// Tokio's clock, the one the provider deadline runs on, so latency and
// deadlines agree (and tests can pause it).
use tokio::time::Instant;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::accounting::{Outcome, RequestRecord, attempt_records, outcome_of};
use crate::api::auth::AuthenticatedKey;
use crate::api::error::ApiError;
use crate::api::request_context::{ClientRequestId, RequestId};
use crate::app::AppState;
use crate::domain::{GenerateRequest, Generation, Usage, UsageSource};
use crate::reliability::{Attempt, AttemptFailure, Execution, execute};

/// Set to `failed` when the request record could not be written
/// (ADR-009).
pub const ACCOUNTING_HEADER: HeaderName = HeaderName::from_static("x-modelswitch-accounting");

/// How many provider attempts the request took (ADR-028).
pub const ATTEMPTS_HEADER: HeaderName = HeaderName::from_static("x-modelswitch-attempts");

/// Largest accepted request body.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_MODEL_CHARS: usize = 256;
const MAX_OUTPUT_TOKENS: u32 = 100_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/generate", post(generate))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerateBody {
    model: String,
    input: String,
    #[serde(default)]
    routing: RoutingBody,
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingBody {
    #[serde(default = "fallback_default")]
    fallback: bool,
}

impl Default for RoutingBody {
    fn default() -> Self {
        Self {
            fallback: fallback_default(),
        }
    }
}

fn fallback_default() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct GenerateResponse {
    id: String,
    model: String,
    provider: String,
    resolved_model: String,
    output: String,
    usage: UsageBody,
    latency_ms: u64,
}

#[derive(Debug, Serialize)]
struct UsageBody {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    total_tokens: Option<u32>,
    source: &'static str,
}

async fn generate(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    client_request_id: Option<Extension<ClientRequestId>>,
    key: Option<Extension<AuthenticatedKey>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let started = Instant::now();
    let body = match parse_body(&headers, body) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };

    let targets = match state.catalog.resolve(&body.model, body.routing.fallback) {
        Ok(targets) => targets,
        Err(_) => {
            tracing::info!("generate: unknown model");
            return ApiError::UnknownModel.into_response();
        }
    };
    // Configuration guarantees every candidate has a provider, so a gap
    // is a wiring bug, not something to retry around.
    let unregistered = targets
        .iter()
        .find(|target| state.providers.get(&target.provider).is_none());
    if let Some(target) = unregistered {
        tracing::error!(provider = %target.provider, "generate: resolved provider is not registered");
        return ApiError::Internal.into_response();
    }
    // Set by the route's auth layer; its absence is a wiring bug.
    let Some(Extension(key)) = key else {
        tracing::error!("generate: no authenticated key on the request");
        return ApiError::Internal.into_response();
    };

    let request = GenerateRequest {
        input: body.input,
        max_output_tokens: body.max_output_tokens,
    };
    let Execution {
        attempts,
        generation,
    } = execute(&targets, &state.providers, &request, state.limits).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    log_failed_attempts(&attempts);

    // The last attempt is what ran in the end: the one that answered, or
    // the failure to report.
    let Some(last) = attempts.last() else {
        tracing::error!("generate: the executor made no attempt");
        return ApiError::Internal.into_response();
    };
    let target = &last.target;
    let (outcome, failure) = outcome_of(last.failure.as_ref());
    let mut record = RequestRecord {
        request_id: request_id.as_str().to_owned(),
        client_request_id: client_request_id.map(|Extension(id)| id.as_str().to_owned()),
        client_api_key_id: key.id,
        requested_model: body.model.clone(),
        provider: target.provider.clone(),
        model: target.model.clone(),
        outcome,
        failure,
        latency_ms,
        usage: Usage::unavailable(),
        attempts: attempt_records(&attempts),
    };
    let reply = match generation {
        Some(Generation { output, usage }) => {
            tracing::info!(%target, latency_ms, attempts = attempts.len(), "generate: succeeded");
            record.usage = usage;
            Ok(GenerateResponse {
                id: request_id.as_str().to_owned(),
                model: body.model,
                provider: target.provider.clone(),
                resolved_model: target.model.clone(),
                output,
                usage: UsageBody {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens: usage.total_tokens,
                    source: match usage.source() {
                        UsageSource::ProviderReported => "provider_reported",
                        UsageSource::Unavailable => "unavailable",
                    },
                },
                latency_ms,
            })
        }
        None if outcome == Outcome::TimedOut => Err(ApiError::UpstreamTimeout),
        None => Err(ApiError::ProviderError),
    };

    // The record is written before responding, so the response can say
    // whether it was; the answer is delivered either way (ADR-009).
    let recorded = write_record(&state, &record).await;
    state.metrics.record_request(&record);
    if !recorded {
        state.metrics.record_accounting_failure();
    }
    let mut response = match reply {
        Ok(reply) => Json(reply).into_response(),
        Err(error) => error.into_response(),
    };
    response
        .headers_mut()
        .insert(ATTEMPTS_HEADER, HeaderValue::from(attempts.len()));
    if !recorded {
        response
            .headers_mut()
            .insert(ACCOUNTING_HEADER, HeaderValue::from_static("failed"));
    }
    response
}

/// One line per failed attempt. Provider names and codes stop here and in
/// the records: clients see only the final error (ADR-028).
fn log_failed_attempts(attempts: &[Attempt]) {
    for attempt in attempts {
        let target = &attempt.target;
        match &attempt.failure {
            Some(AttemptFailure::Provider(error)) => {
                tracing::warn!(%target, %error, "generate: provider failed");
            }
            Some(AttemptFailure::TimedOut) => {
                tracing::warn!(%target, "generate: provider timed out");
            }
            None => {}
        }
    }
}

/// Writes the record under the accounting deadline. A failure is logged
/// (`accounting_write_failed`, with the request ID from the span) and
/// reported to the caller, never turned into an error response.
async fn write_record(state: &AppState, record: &RequestRecord) -> bool {
    match tokio::time::timeout(state.accounting_timeout, state.records.insert(record)).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            tracing::error!(%error, outcome = record.outcome.as_str(), "accounting_write_failed");
            false
        }
        Err(_) => {
            tracing::error!(
                timeout_ms = state.accounting_timeout.as_millis() as u64,
                outcome = record.outcome.as_str(),
                "accounting_write_failed: timed out"
            );
            false
        }
    }
}

/// Validates everything about the body before any routing work. Messages
/// are fixed strings: the client's own values are never echoed back.
fn parse_body(
    headers: &HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<GenerateBody, ApiError> {
    let body = body.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge
        } else {
            ApiError::InvalidRequest("The request body could not be read.")
        }
    })?;
    let is_json = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err(ApiError::InvalidRequest(
            "Content-Type must be application/json.",
        ));
    }

    let body: GenerateBody = serde_json::from_slice(&body).map_err(|_| {
        ApiError::InvalidRequest(
            "The body must be a JSON object with string fields \"model\" and \"input\" \
             and only the documented optional fields.",
        )
    })?;
    if body.model.trim().is_empty() || body.model.chars().count() > MAX_MODEL_CHARS {
        return Err(ApiError::InvalidRequest(
            "\"model\" must be a non-empty alias or provider/model pin of at most 256 characters.",
        ));
    }
    if body.input.trim().is_empty() {
        return Err(ApiError::InvalidRequest("\"input\" must not be empty."));
    }
    if body
        .max_output_tokens
        .is_some_and(|tokens| tokens == 0 || tokens > MAX_OUTPUT_TOKENS)
    {
        return Err(ApiError::InvalidRequest(
            "\"max_output_tokens\" must be between 1 and 100000.",
        ));
    }
    Ok(body)
}
