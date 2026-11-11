//! The single place where errors become HTTP responses (ADR-019).
//!
//! An `ApiError` response carries only its status and the error itself
//! (as a response extension). The request-context middleware turns that
//! into the JSON envelope, because only it knows the request ID:
//!
//! ``` json
//! { "error": { "code": "not_found", "message": "..." }, "request_id": "req_..." }
//! ```
//!
//! Public messages are `&'static str`, so a client-supplied value can
//! never be formatted into a response by accident. Whoever produces an
//! `Internal` error logs the detail; the client only ever sees the
//! generic message.

use axum::http::header::{RETRY_AFTER, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// No valid dashboard session: never logged in, logged out, or
    /// expired (ADR-036).
    #[error("login required")]
    LoginRequired,
    /// A dashboard login with the wrong admin token.
    #[error("login failed")]
    LoginFailed,
    /// Too many failed dashboard logins; logins are paused.
    #[error("login throttled")]
    LoginThrottled { retry_after_secs: u64 },
    /// A dashboard request that changes state did not come from the
    /// dashboard's own page (its `Origin` is missing or another site).
    #[error("forbidden")]
    Forbidden,
    /// The request is malformed (bad header, body, or field).
    #[error("invalid request: {0}")]
    InvalidRequest(&'static str),
    /// Missing, malformed, unknown, or revoked API key. Deliberately one
    /// variant: clients must not learn which (the reason is logged).
    #[error("unauthorized")]
    Unauthorized,
    /// The request body exceeds the size limit.
    #[error("payload too large")]
    PayloadTooLarge,
    /// The requested model is neither a configured alias nor a configured
    /// `provider/model` pin.
    #[error("unknown model")]
    UnknownModel,
    /// No route matches the path.
    #[error("not found")]
    NotFound,
    /// The path exists but not for this HTTP method.
    #[error("method not allowed")]
    MethodNotAllowed,
    /// This API key is over its rate limit.
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },
    /// This API key has too many requests running. No `Retry-After`:
    /// nobody knows when one will finish (ADR-032).
    #[error("concurrency limited")]
    ConcurrencyLimited,
    /// This API key has spent its budget for the month (ADR-035).
    #[error("budget exceeded")]
    BudgetExceeded { retry_after_secs: u64 },
    /// This API key has a budget, but a candidate model has no price, so
    /// its spending could not be counted (ADR-035).
    #[error("model not priced")]
    ModelNotPriced,
    /// A dependency needed before any provider call (the database) is
    /// down or too slow. Temporary: clients should retry later.
    #[error("service unavailable")]
    ServiceUnavailable,
    /// The provider failed or answered unusably. Its own error text is
    /// logged, never returned (FR-09).
    #[error("provider error")]
    ProviderError,
    /// The provider did not answer before its deadline.
    #[error("upstream timeout")]
    UpstreamTimeout,
    /// A bug or unexpected failure inside the gateway.
    #[error("internal error")]
    Internal,
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized | Self::LoginRequired | Self::LoginFailed => {
                StatusCode::UNAUTHORIZED
            }
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnknownModel | Self::ModelNotPriced => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::RateLimited { .. }
            | Self::ConcurrencyLimited
            | Self::BudgetExceeded { .. }
            | Self::LoginThrottled { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::ProviderError => StatusCode::BAD_GATEWAY,
            Self::UpstreamTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable, machine-readable code that clients branch on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::LoginRequired => "login_required",
            Self::LoginFailed => "login_failed",
            Self::LoginThrottled { .. } => "login_throttled",
            Self::Forbidden => "forbidden",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnknownModel => "unknown_model",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::RateLimited { .. } => "rate_limited",
            Self::ConcurrencyLimited => "concurrency_limited",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::ModelNotPriced => "model_not_priced",
            Self::ServiceUnavailable => "service_unavailable",
            Self::ProviderError => "provider_error",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::Internal => "internal_error",
        }
    }

    /// Human-readable message; may change between releases.
    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidRequest(message) => message,
            Self::Unauthorized => "A valid API key is required.",
            Self::LoginRequired => "Log in to the dashboard first.",
            Self::LoginFailed => "The admin token is not valid.",
            Self::LoginThrottled { .. } => "Too many failed logins. Retry later.",
            Self::Forbidden => "This request must come from the dashboard's own page.",
            Self::PayloadTooLarge => "The request body is too large.",
            Self::UnknownModel => "The model is not a configured alias or provider/model pin.",
            Self::NotFound => "No route matches this path.",
            Self::MethodNotAllowed => "This path does not support this HTTP method.",
            Self::RateLimited { .. } => "Too many requests for this API key. Retry later.",
            Self::ConcurrencyLimited => "This API key has too many requests running. Retry later.",
            Self::BudgetExceeded { .. } => "This API key has spent its budget for this month.",
            Self::ModelNotPriced => "This API key has a budget, but the model has no price.",
            Self::ServiceUnavailable => "The gateway is temporarily unavailable. Retry later.",
            Self::ProviderError => "The selected provider failed to generate a response.",
            Self::UpstreamTimeout => "The selected provider did not respond before the deadline.",
            Self::Internal => "The gateway hit an unexpected error.",
        }
    }

    /// The public JSON envelope for this error.
    pub(crate) fn envelope(&self, request_id: &str) -> serde_json::Value {
        serde_json::to_value(Envelope {
            error: Body {
                code: self.code(),
                message: self.message(),
            },
            request_id,
        })
        .expect("error envelope serializes")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = self.status().into_response();
        match self {
            // RFC 6750: tell the client how to authenticate.
            Self::Unauthorized => {
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            // RFC 9110: tell the client when to come back.
            Self::RateLimited { retry_after_secs }
            | Self::BudgetExceeded { retry_after_secs }
            | Self::LoginThrottled { retry_after_secs } => {
                response
                    .headers_mut()
                    .insert(RETRY_AFTER, HeaderValue::from(retry_after_secs));
            }
            _ => {}
        }
        response.extensions_mut().insert(self);
        response
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    error: Body,
    request_id: &'a str,
}

#[derive(Serialize)]
struct Body {
    code: &'static str,
    message: &'static str,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_variant_maps_to_its_status_and_code() {
        let cases = [
            (ApiError::InvalidRequest("x"), 400, "invalid_request"),
            (ApiError::Unauthorized, 401, "unauthorized"),
            (ApiError::LoginRequired, 401, "login_required"),
            (ApiError::LoginFailed, 401, "login_failed"),
            (
                ApiError::LoginThrottled {
                    retry_after_secs: 1,
                },
                429,
                "login_throttled",
            ),
            (ApiError::Forbidden, 403, "forbidden"),
            (ApiError::PayloadTooLarge, 413, "payload_too_large"),
            (ApiError::UnknownModel, 422, "unknown_model"),
            (ApiError::NotFound, 404, "not_found"),
            (ApiError::MethodNotAllowed, 405, "method_not_allowed"),
            (
                ApiError::RateLimited {
                    retry_after_secs: 1,
                },
                429,
                "rate_limited",
            ),
            (ApiError::ConcurrencyLimited, 429, "concurrency_limited"),
            (
                ApiError::BudgetExceeded {
                    retry_after_secs: 1,
                },
                429,
                "budget_exceeded",
            ),
            (ApiError::ModelNotPriced, 422, "model_not_priced"),
            (ApiError::ServiceUnavailable, 503, "service_unavailable"),
            (ApiError::ProviderError, 502, "provider_error"),
            (ApiError::UpstreamTimeout, 504, "upstream_timeout"),
            (ApiError::Internal, 500, "internal_error"),
        ];
        for (error, status, code) in cases {
            assert_eq!(error.status().as_u16(), status, "{error:?}");
            assert_eq!(error.code(), code, "{error:?}");
        }
    }

    #[test]
    fn envelope_has_the_documented_shape() {
        let envelope = ApiError::NotFound.envelope("req_test");

        assert_eq!(
            envelope,
            json!({
                "error": { "code": "not_found", "message": "No route matches this path." },
                "request_id": "req_test"
            })
        );
    }
}
