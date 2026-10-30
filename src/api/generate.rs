//! `POST /v1/generate` (API.md).
//!
//! Order of work, cheapest and safest first: authentication (route
//! layer), then body size, content type, and JSON validation, then model
//! resolution. Only a fully valid request reaches a provider, and every
//! provider call runs under that provider's deadline.
//!
//! Prompts and outputs are never logged (ADR-005); log lines carry the
//! target, latency, and outcome.

use std::time::Instant;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::error::ApiError;
use crate::api::request_context::RequestId;
use crate::app::AppState;
use crate::domain::{GenerateRequest, Generation, UsageSource};

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
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<GenerateResponse>, ApiError> {
    let started = Instant::now();
    let body = parse_body(&headers, body)?;

    let targets = state
        .catalog
        .resolve(&body.model, body.routing.fallback)
        .map_err(|_| {
            tracing::info!("generate: unknown model");
            ApiError::UnknownModel
        })?;
    // Trying further candidates after a failure is the reliability
    // executor's job (Stage 2); for now only the first is called.
    let target = &targets[0];
    let Some(slot) = state.providers.get(&target.provider) else {
        tracing::error!(provider = %target.provider, "generate: resolved provider is not registered");
        return Err(ApiError::Internal);
    };

    let request = GenerateRequest {
        input: body.input,
        max_output_tokens: body.max_output_tokens,
    };
    let call = slot.provider.generate(&target.model, &request);
    let Generation { output, usage } = match tokio::time::timeout(slot.timeout, call).await {
        Ok(Ok(generation)) => generation,
        Ok(Err(error)) => {
            tracing::warn!(%target, %error, "generate: provider failed");
            return Err(ApiError::ProviderError);
        }
        Err(_) => {
            tracing::warn!(
                %target,
                timeout_ms = slot.timeout.as_millis() as u64,
                "generate: provider timed out"
            );
            return Err(ApiError::UpstreamTimeout);
        }
    };

    let latency_ms = started.elapsed().as_millis() as u64;
    tracing::info!(%target, latency_ms, "generate: succeeded");
    Ok(Json(GenerateResponse {
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
    }))
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
