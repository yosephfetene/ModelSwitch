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

use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
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
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnknownModel => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
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
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnknownModel => "unknown_model",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
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
            Self::PayloadTooLarge => "The request body is too large.",
            Self::UnknownModel => "The model is not a configured alias or provider/model pin.",
            Self::NotFound => "No route matches this path.",
            Self::MethodNotAllowed => "This path does not support this HTTP method.",
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
        if self == Self::Unauthorized {
            // RFC 6750: tell the client how to authenticate.
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
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
            (ApiError::PayloadTooLarge, 413, "payload_too_large"),
            (ApiError::UnknownModel, 422, "unknown_model"),
            (ApiError::NotFound, 404, "not_found"),
            (ApiError::MethodNotAllowed, 405, "method_not_allowed"),
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
