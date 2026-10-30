//! API key authentication for `/v1/*` routes (ADR-012).
//!
//! - Every bad key (missing, malformed, unknown, revoked) gets the same
//!   401, so a client cannot learn which; the reason goes to the logs.
//! - Keys that do not have the exact key shape are rejected without a
//!   database query, so junk cannot load PostgreSQL.
//! - Fails closed: if the key cannot be checked (database down or past
//!   its deadline), the request is refused with 503, never let through.
//! - No cache: every request asks the database, so revocation is
//!   immediate.
//!
//! Neither the key nor the `Authorization` header is ever logged.

use std::fmt;

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::api::request_context::RequestSpan;
use crate::app::AppState;
use crate::auth::api_key::{KeyHash, looks_like_key};

/// The key that authenticated this request, for handlers and request
/// records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedKey {
    pub id: Uuid,
}

/// Why a key was rejected. Logged, never sent to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection {
    Missing,
    Malformed,
    Unknown,
    Revoked,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Unknown => "unknown",
            Self::Revoked => "revoked",
        })
    }
}

pub async fn require_api_key(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let hash = match presented_key(request.headers()) {
        Ok(key) => KeyHash::of(key),
        Err(reason) => return reject(reason, None),
    };

    let lookup = tokio::time::timeout(state.auth_timeout, state.keys.find_by_hash(hash)).await;
    match lookup {
        Ok(Ok(Some(key))) if !key.revoked => {
            if let Some(RequestSpan(span)) = request.extensions().get::<RequestSpan>() {
                span.record("key_id", tracing::field::display(key.id));
            }
            request
                .extensions_mut()
                .insert(AuthenticatedKey { id: key.id });
            next.run(request).await
        }
        Ok(Ok(Some(key))) => reject(Rejection::Revoked, Some(key.id)),
        Ok(Ok(None)) => reject(Rejection::Unknown, None),
        Ok(Err(error)) => {
            tracing::warn!(%error, "auth: key lookup failed");
            ApiError::ServiceUnavailable.into_response()
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = state.auth_timeout.as_millis() as u64,
                "auth: key lookup timed out"
            );
            ApiError::ServiceUnavailable.into_response()
        }
    }
}

/// Extracts the key from `Authorization: Bearer <key>`. The scheme is
/// case-insensitive (RFC 7235); the key must have the exact key shape.
fn presented_key(headers: &HeaderMap) -> Result<&str, Rejection> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let value = values.next().ok_or(Rejection::Missing)?;
    if values.next().is_some() {
        return Err(Rejection::Malformed);
    }
    let (scheme, key) = value
        .to_str()
        .ok()
        .and_then(|value| value.split_once(' '))
        .ok_or(Rejection::Malformed)?;
    if !scheme.eq_ignore_ascii_case("bearer") || !looks_like_key(key) {
        return Err(Rejection::Malformed);
    }
    Ok(key)
}

fn reject(reason: Rejection, key_id: Option<Uuid>) -> Response {
    match key_id {
        Some(key_id) => tracing::info!(%reason, %key_id, "auth: rejected API key"),
        None => tracing::info!(%reason, "auth: rejected API key"),
    }
    ApiError::Unauthorized.into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const KEY: &str = "ms_dev_RV3umJ6w6EViulBHHhYjJPP5ldyeww4mWnmKpU2m2bP";

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn extracts_a_bearer_key_with_any_scheme_case() {
        for scheme in ["Bearer", "bearer", "BEARER"] {
            let value = format!("{scheme} {KEY}");
            assert_eq!(presented_key(&headers(&[&value])), Ok(KEY), "{value}");
        }
    }

    #[test]
    fn classifies_missing_and_malformed_headers() {
        assert_eq!(presented_key(&headers(&[])), Err(Rejection::Missing));
        for value in [
            KEY,
            "Bearer",
            "Bearer hello",
            &format!("Basic {KEY}"),
            &format!("Bearer  {KEY}"),
            &format!("Bearer {KEY} extra"),
        ] {
            assert_eq!(
                presented_key(&headers(&[value])),
                Err(Rejection::Malformed),
                "{value:?}"
            );
        }
        let bearer = format!("Bearer {KEY}");
        assert_eq!(
            presented_key(&headers(&[&bearer, &bearer])),
            Err(Rejection::Malformed)
        );
    }
}
