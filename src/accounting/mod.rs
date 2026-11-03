//! Request records: one row per request that reached a provider
//! (ADR-009, ADR-027).
//!
//! A record says who asked, what ran, how it ended, how long it took,
//! and the token counts the provider reported. It never holds the prompt
//! or the output (ADR-005). Requests rejected before any provider call
//! (401, 400, 413, 422) get no record: they did no work for anyone.

use uuid::Uuid;

use crate::domain::{BoxFuture, ProviderError, Usage, UsageSource};

/// How a recorded request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    /// The provider answered with an error or an unusable response.
    Failed,
    /// The provider did not answer before its deadline.
    TimedOut,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        }
    }
}

/// Why a failed request failed, from the provider error. Only short
/// identifiers: provider messages never reach a record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Failure {
    /// `request`, `status`, `invalid_response`, or `blocked`.
    pub kind: Option<&'static str>,
    /// The provider's short code, such as `rate_limit_exceeded` or `SAFETY`.
    pub code: Option<String>,
    /// The provider's HTTP status, when it sent one.
    pub provider_status: Option<u16>,
}

impl Failure {
    pub fn from_error(error: &ProviderError) -> Self {
        match error {
            ProviderError::Request(_) => Self {
                kind: Some("request"),
                ..Self::default()
            },
            ProviderError::Status { status, code } => Self {
                kind: Some("status"),
                code: code.clone(),
                provider_status: Some(*status),
            },
            ProviderError::InvalidResponse(_) => Self {
                kind: Some("invalid_response"),
                ..Self::default()
            },
            ProviderError::Blocked { reason } => Self {
                kind: Some("blocked"),
                code: reason.clone(),
                provider_status: None,
            },
        }
    }
}

/// One request that reached a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    /// The gateway request ID (`req_` + UUIDv7).
    pub request_id: String,
    pub client_request_id: Option<String>,
    pub client_api_key_id: Uuid,
    /// The alias or pin as the client sent it.
    pub requested_model: String,
    /// What actually ran.
    pub provider: String,
    pub model: String,
    pub outcome: Outcome,
    pub failure: Failure,
    pub latency_ms: u64,
    /// As reported; unknown counts stay `None` (ADR-010).
    pub usage: Usage,
}

impl RequestRecord {
    pub fn usage_source(&self) -> &'static str {
        match self.usage.source() {
            UsageSource::ProviderReported => "provider_reported",
            UsageSource::Unavailable => "unavailable",
        }
    }
}

/// Why a record could not be written. For logs only.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RecordStoreError(pub String);

/// The test seam between the request path and the database, so tests can
/// simulate a database that is down or hanging.
pub trait RecordStore: Send + Sync {
    fn insert<'a>(
        &'a self,
        record: &'a RequestRecord,
    ) -> BoxFuture<'a, Result<(), RecordStoreError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_keep_only_identifiers() {
        let cases = [
            (
                ProviderError::Request("connection refused to 10.0.0.1".to_owned()),
                Failure {
                    kind: Some("request"),
                    ..Failure::default()
                },
            ),
            (
                ProviderError::Status {
                    status: 429,
                    code: Some("rate_limit_exceeded".to_owned()),
                },
                Failure {
                    kind: Some("status"),
                    code: Some("rate_limit_exceeded".to_owned()),
                    provider_status: Some(429),
                },
            ),
            (
                ProviderError::InvalidResponse("not a chat completion: ...".to_owned()),
                Failure {
                    kind: Some("invalid_response"),
                    ..Failure::default()
                },
            ),
            (
                ProviderError::Blocked {
                    reason: Some("SAFETY".to_owned()),
                },
                Failure {
                    kind: Some("blocked"),
                    code: Some("SAFETY".to_owned()),
                    provider_status: None,
                },
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(Failure::from_error(&error), expected, "{error}");
        }
    }
}
