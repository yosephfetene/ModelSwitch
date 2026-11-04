//! Request records: one row per request that reached a provider
//! (ADR-009, ADR-027).
//!
//! A record says who asked, what ran, how it ended, how long it took,
//! and the token counts the provider reported. It never holds the prompt
//! or the output (ADR-005). Requests rejected before any provider call
//! (401, 400, 413, 422) get no record: they did no work for anyone.
//!
//! Each record carries its attempts, one per provider call (ADR-003,
//! ADR-029).

use uuid::Uuid;

use crate::domain::{BoxFuture, ProviderError, Usage, UsageSource};
use crate::reliability::{Attempt, AttemptFailure, FailureClass};

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

/// How an attempt ended, in the terms records use.
pub fn outcome_of(failure: Option<&AttemptFailure>) -> (Outcome, Failure) {
    match failure {
        None => (Outcome::Succeeded, Failure::default()),
        Some(AttemptFailure::Provider(error)) => (Outcome::Failed, Failure::from_error(error)),
        Some(AttemptFailure::TimedOut) => (Outcome::TimedOut, Failure::default()),
    }
}

/// What the executor did after an attempt (ADR-029).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Nothing further: it succeeded, or the failure allows no other try.
    NotRetried,
    /// Called the same target again.
    Retry,
    /// Moved on to another target.
    Fallback,
    /// Another try was allowed, but no candidates, attempts, or time were
    /// left.
    Exhausted,
}

impl RetryDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRetried => "not_retried",
            Self::Retry => "retry",
            Self::Fallback => "fallback",
            Self::Exhausted => "exhausted",
        }
    }
}

/// One provider call of a recorded request. Like the request record, it
/// holds identifiers and numbers, never provider text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub provider: String,
    pub model: String,
    pub outcome: Outcome,
    pub failure: Failure,
    /// `None` when it succeeded.
    pub class: Option<FailureClass>,
    pub decision: RetryDecision,
    pub latency_ms: u64,
}

/// The records for an execution's attempts, in order. What happened
/// after each attempt follows from the attempt after it.
pub fn attempt_records(attempts: &[Attempt]) -> Vec<AttemptRecord> {
    attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| {
            let class = attempt.failure.as_ref().map(AttemptFailure::class);
            let decision = match (class, attempts.get(index + 1)) {
                (None, _) => RetryDecision::NotRetried,
                (Some(_), Some(next)) if next.target == attempt.target => RetryDecision::Retry,
                (Some(_), Some(_)) => RetryDecision::Fallback,
                (Some(class), None) if class.fall_back() => RetryDecision::Exhausted,
                (Some(_), None) => RetryDecision::NotRetried,
            };
            let (outcome, failure) = outcome_of(attempt.failure.as_ref());
            AttemptRecord {
                provider: attempt.target.provider.clone(),
                model: attempt.target.model.clone(),
                outcome,
                failure,
                class,
                decision,
                latency_ms: attempt.latency.as_millis() as u64,
            }
        })
        .collect()
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
    /// Every provider call made for this request, in order (ADR-003).
    pub attempts: Vec<AttemptRecord>,
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
    use std::time::Duration;

    use crate::domain::Target;

    use super::*;

    fn status(status: u16) -> AttemptFailure {
        AttemptFailure::Provider(ProviderError::Status { status, code: None })
    }

    fn failed(target: &str, failure: AttemptFailure) -> Attempt {
        let target = Target::parse(target).unwrap();
        Attempt::failed(&target, failure, Duration::from_millis(7))
    }

    fn succeeded(target: &str) -> Attempt {
        Attempt::succeeded(&Target::parse(target).unwrap(), Duration::from_millis(7))
    }

    fn decisions(attempts: &[Attempt]) -> Vec<RetryDecision> {
        attempt_records(attempts)
            .iter()
            .map(|record| record.decision)
            .collect()
    }

    #[test]
    fn decisions_follow_from_the_next_attempt() {
        use RetryDecision::*;

        assert_eq!(decisions(&[succeeded("a/m")]), [NotRetried]);
        assert_eq!(
            decisions(&[
                failed("a/m", status(503)),
                failed("a/m", status(503)),
                succeeded("b/m"),
            ]),
            [Retry, Fallback, NotRetried]
        );
    }

    #[test]
    fn the_last_failure_is_exhausted_only_if_another_try_was_allowed() {
        assert_eq!(
            decisions(&[failed("a/m", status(429))]),
            [RetryDecision::Exhausted]
        );
        assert_eq!(
            decisions(&[failed("a/m", AttemptFailure::TimedOut)]),
            [RetryDecision::Exhausted]
        );
        assert_eq!(
            decisions(&[failed("a/m", status(400))]),
            [RetryDecision::NotRetried],
            "a request problem allows no other try"
        );
    }

    #[test]
    fn attempt_records_carry_class_failure_and_latency() {
        let records = attempt_records(&[failed("a/m", status(429)), succeeded("b/m")]);

        assert_eq!(records[0].provider, "a");
        assert_eq!(records[0].outcome, Outcome::Failed);
        assert_eq!(records[0].class, Some(FailureClass::TargetProblem));
        assert_eq!(records[0].failure.provider_status, Some(429));
        assert_eq!(records[0].latency_ms, 7);
        assert_eq!(records[1].outcome, Outcome::Succeeded);
        assert_eq!(records[1].class, None);
    }

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
