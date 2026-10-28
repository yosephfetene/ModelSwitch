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

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// The request is malformed (bad header, body, or field).
    #[error("invalid request: {0}")]
    InvalidRequest(&'static str),
    /// No route matches the path.
    #[error("not found")]
    NotFound,
    /// The path exists but not for this HTTP method.
    #[error("method not allowed")]
    MethodNotAllowed,
    /// A bug or unexpected failure inside the gateway.
    #[error("internal error")]
    Internal,
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable, machine-readable code that clients branch on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Internal => "internal_error",
        }
    }

    /// Human-readable message; may change between releases.
    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidRequest(message) => message,
            Self::NotFound => "No route matches this path.",
            Self::MethodNotAllowed => "This path does not support this HTTP method.",
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
            (ApiError::NotFound, 404, "not_found"),
            (ApiError::MethodNotAllowed, 405, "method_not_allowed"),
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
