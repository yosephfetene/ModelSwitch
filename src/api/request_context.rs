//! Per-request identity: the gateway's request ID and the client's
//! optional correlation ID (ADR-011, FR-16).
//!
//! The middleware here wraps every route and fallback:
//!
//! 1. Generates a fresh `req_` + UUIDv7 request ID. A client-sent
//!    `X-Request-Id` is never read: the ID must come from a source the
//!    gateway controls.
//! 2. Validates `X-Client-Request-Id`. Missing is fine; an invalid value
//!    is rejected with 400 and is never logged or echoed (log injection).
//! 3. Runs the request inside a `request` span carrying both IDs, so
//!    every log line it produces can be found by ID.
//! 4. Sets `X-Request-Id` (and echoes a valid correlation ID) on every
//!    response, and completes `ApiError` bodies with the same request ID.

use std::fmt;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::Instrument;
use uuid::Uuid;

use crate::api::error::ApiError;

pub const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
pub const CLIENT_REQUEST_ID: HeaderName = HeaderName::from_static("x-client-request-id");

const CLIENT_REQUEST_ID_MAX_LEN: usize = 128;

/// Gateway-generated request ID: `req_` + a UUIDv7, which sorts by
/// creation time and is unique thanks to its random bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub fn generate() -> Self {
        Self(format!("req_{}", Uuid::now_v7()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated client correlation ID: 1--128 characters from
/// `[A-Za-z0-9._:-]`, so it is safe to log, store, and echo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRequestId(String);

impl ClientRequestId {
    pub fn parse(value: &str) -> Result<Self, ClientRequestIdError> {
        if value.is_empty() {
            return Err(ClientRequestIdError::Empty);
        }
        if value.len() > CLIENT_REQUEST_ID_MAX_LEN {
            return Err(ClientRequestIdError::TooLong);
        }
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-');
        if !value.chars().all(allowed) {
            return Err(ClientRequestIdError::InvalidCharacter);
        }
        Ok(Self(value.to_owned()))
    }

    /// Reads the header. `Ok(None)` when it is absent; sending it more
    /// than once is rejected, since it would be unclear which to echo.
    pub fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, ClientRequestIdError> {
        let mut values = headers.get_all(CLIENT_REQUEST_ID).iter();
        let Some(value) = values.next() else {
            return Ok(None);
        };
        if values.next().is_some() {
            return Err(ClientRequestIdError::Repeated);
        }
        let value = value
            .to_str()
            .map_err(|_| ClientRequestIdError::InvalidCharacter)?;
        Self::parse(value).map(Some)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a correlation ID was rejected. Deliberately carries no part of
/// the rejected value, so it is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClientRequestIdError {
    #[error("empty")]
    Empty,
    #[error("longer than 128 characters")]
    TooLong,
    #[error("contains a character outside [A-Za-z0-9._:-]")]
    InvalidCharacter,
    #[error("sent more than once")]
    Repeated,
}

pub async fn request_context(mut request: Request, next: Next) -> Response {
    let request_id = RequestId::generate();
    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        client_request_id = tracing::field::Empty,
    );

    let client_request_id = ClientRequestId::from_headers(request.headers());
    let mut response = match &client_request_id {
        Ok(client_request_id) => {
            if let Some(id) = client_request_id {
                span.record("client_request_id", id.as_str());
                request.extensions_mut().insert(id.clone());
            }
            request.extensions_mut().insert(request_id.clone());
            next.run(request).instrument(span).await
        }
        Err(reason) => {
            span.in_scope(|| tracing::info!(%reason, "rejected X-Client-Request-Id"));
            ApiError::InvalidRequest(
                "X-Client-Request-Id must be 1-128 characters from A-Z a-z 0-9 . _ : -",
            )
            .into_response()
        }
    };

    if let Some(error) = response.extensions_mut().remove::<ApiError>() {
        response = error_response(response, &error, &request_id);
    }
    let headers = response.headers_mut();
    headers.insert(REQUEST_ID, header_value(request_id.as_str()));
    if let Ok(Some(id)) = &client_request_id {
        headers.insert(CLIENT_REQUEST_ID, header_value(id.as_str()));
    }
    response
}

/// Replaces the (empty) body of an `ApiError` response with the JSON
/// envelope, keeping its status and any headers such as `Allow`.
fn error_response(response: Response, error: &ApiError, request_id: &RequestId) -> Response {
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(CONTENT_LENGTH);
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let body = error.envelope(request_id.as_str()).to_string();
    Response::from_parts(parts, Body::from(body))
}

/// Both IDs are restricted to visible ASCII, so this cannot fail.
fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("IDs are valid header values")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_is_req_prefixed_uuid_v7() {
        let id = RequestId::generate();

        let uuid = id.as_str().strip_prefix("req_").expect("req_ prefix");
        assert_eq!(Uuid::parse_str(uuid).unwrap().get_version_num(), 7);
    }

    #[test]
    fn accepts_allowed_characters_up_to_128() {
        for value in ["a", "order-42", "svc.api:retry_1", &"x".repeat(128)] {
            assert!(ClientRequestId::parse(value).is_ok(), "{value:?}");
        }
    }

    #[test]
    fn rejects_empty_too_long_and_bad_characters() {
        let cases = [
            ("", ClientRequestIdError::Empty),
            (&"x".repeat(129), ClientRequestIdError::TooLong),
            ("has space", ClientRequestIdError::InvalidCharacter),
            ("a\tb", ClientRequestIdError::InvalidCharacter),
            ("semi;colon", ClientRequestIdError::InvalidCharacter),
            ("caf\u{e9}", ClientRequestIdError::InvalidCharacter),
        ];
        for (value, expected) in cases {
            assert_eq!(ClientRequestId::parse(value), Err(expected), "{value:?}");
        }
    }

    #[test]
    fn missing_header_is_none_and_repeated_header_is_rejected() {
        let mut headers = HeaderMap::new();
        assert_eq!(ClientRequestId::from_headers(&headers), Ok(None));

        headers.append(CLIENT_REQUEST_ID, HeaderValue::from_static("a"));
        headers.append(CLIENT_REQUEST_ID, HeaderValue::from_static("b"));
        assert_eq!(
            ClientRequestId::from_headers(&headers),
            Err(ClientRequestIdError::Repeated)
        );
    }
}
