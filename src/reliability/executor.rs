//! The executor: tries a request's candidate targets with retries and
//! fallback, within the attempt and time limits (ADR-028).

use std::time::Duration;

// Tokio's clock, the one deadlines run on, so tests can pause it.
use tokio::time::Instant;

use super::{FailureClass, classify};
use crate::domain::{GenerateRequest, Generation, ProviderError, Target};
use crate::providers::Registry;

/// How many times a transient failure is retried on the same target
/// before falling back: one quick second chance (ADR-028).
pub const RETRIES_PER_TARGET: usize = 1;

/// The wait before the first retry of a target is random within this
/// range, so retries from many clients do not arrive as one wave; each
/// further retry of the same target doubles it (ADR-028).
const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_millis(300);

/// How far one request may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Attempts across all retries and fallbacks.
    pub max_attempts: usize,
    /// Wall-clock budget for the whole request.
    pub request_timeout: Duration,
}

/// Why one attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptFailure {
    /// The provider answered with an error.
    Provider(ProviderError),
    /// The provider did not answer before the attempt's deadline.
    TimedOut,
}

impl AttemptFailure {
    /// A timeout used its whole deadline and may have been billed, so it
    /// is never retried on the same target, but another may answer.
    pub fn class(&self) -> FailureClass {
        match self {
            Self::Provider(error) => classify(error),
            Self::TimedOut => FailureClass::TargetProblem,
        }
    }
}

/// One call to one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub target: Target,
    /// `None` when it succeeded.
    pub failure: Option<AttemptFailure>,
}

impl Attempt {
    pub fn succeeded(target: &Target) -> Self {
        Self {
            target: target.clone(),
            failure: None,
        }
    }

    pub fn failed(target: &Target, failure: AttemptFailure) -> Self {
        Self {
            target: target.clone(),
            failure: Some(failure),
        }
    }
}

/// Everything the executor did for one request.
#[derive(Debug)]
pub struct Execution {
    /// In order. The last one is the answer, or the error to report.
    pub attempts: Vec<Attempt>,
    /// The answer, if an attempt succeeded.
    pub generation: Option<Generation>,
}

/// Calls `target` once, for at most its provider's timeout and at most
/// `budget` (the time left for the whole request). A target whose
/// provider is not registered fails as a request error; the handler
/// checks registration before calling the executor, so this is only a
/// safety net.
async fn call_once(
    providers: &Registry,
    target: &Target,
    request: &GenerateRequest,
    budget: Duration,
) -> Result<Generation, AttemptFailure> {
    let Some(slot) = providers.get(&target.provider) else {
        return Err(AttemptFailure::Provider(ProviderError::Request(
            "provider is not registered".to_owned(),
        )));
    };
    let timeout = slot.timeout.min(budget);
    let call = slot.provider.generate(&target.model, request);
    match tokio::time::timeout(timeout, call).await {
        Ok(Ok(generation)) => Ok(generation),
        Ok(Err(error)) => Err(AttemptFailure::Provider(error)),
        Err(_) => Err(AttemptFailure::TimedOut),
    }
}

/// The wait before retry number `retry` (1 for the first) of a target:
/// `BACKOFF_MIN..=BACKOFF_MAX`, doubled for each retry after the first,
/// placed in that range by `random`.
fn backoff(retry: usize, random: u64) -> Duration {
    let doublings = u32::try_from(retry.saturating_sub(1))
        .unwrap_or(u32::MAX)
        .min(10);
    let factor = 1u32 << doublings;
    let (min, max) = (BACKOFF_MIN * factor, BACKOFF_MAX * factor);
    let span_ms = u64::try_from((max - min).as_millis()).unwrap_or(u64::MAX);
    min + Duration::from_millis(random % (span_ms + 1))
}

/// A random number for jitter. If the system's random source fails, the
/// shortest wait is used: jitter is a courtesy, not a security property.
fn random_u64() -> u64 {
    let mut bytes = [0; 8];
    match getrandom::fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes),
        Err(_) => 0,
    }
}

/// Tries `targets` in order until one answers (ADR-028).
///
/// Rules:
///
/// - Stop once `limits.max_attempts` attempts have been made, or once
///   `limits.request_timeout` has passed since the request started.
/// - Each call gets at most the time left.
/// - On success: record it and return the answer.
/// - On failure: record it. If its class allows retrying the same target
///   and this target has been retried fewer than `RETRIES_PER_TARGET`
///   times, wait `backoff` and call it again, unless the wait would not
///   fit in the time left. Otherwise, if its class allows falling back,
///   go on to the next target, with no wait; if not, stop.
/// - If no target answers, return every attempt and no answer.
pub async fn execute(
    targets: &[Target],
    providers: &Registry,
    request: &GenerateRequest,
    limits: Limits,
) -> Execution {
    let started = Instant::now();
    let mut attempts = Vec::new();

    for target in targets {
        // Each target gets its own retry, so the counter restarts here.
        let mut retries = 0;

        loop {
            if attempts.len() >= limits.max_attempts {
                // Inside the inner loop, `break` would only move to the
                // next target. Out of attempts means stop everything.
                return Execution {
                    attempts,
                    generation: None,
                };
            }

            // `saturating_sub`: past the deadline is zero time left, never
            // a negative duration (which would panic).
            let remaining = limits.request_timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Execution {
                    attempts,
                    generation: None,
                };
            }

            match call_once(providers, target, request, remaining).await {
                Ok(generation) => {
                    attempts.push(Attempt::succeeded(target));
                    return Execution {
                        attempts,
                        generation: Some(generation),
                    };
                }
                Err(failure) => {
                    let class = failure.class();
                    attempts.push(Attempt::failed(target, failure));

                    if class.retry_same_target() && retries < RETRIES_PER_TARGET {
                        // Transient, with a retry left: wait a little, then
                        // the same target again. If the wait would not fit
                        // in the time left, skip the retry and fall back,
                        // which needs no wait.
                        retries += 1;
                        let wait = backoff(retries, random_u64());
                        let remaining = limits.request_timeout.saturating_sub(started.elapsed());
                        if wait < remaining {
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                    }
                    if class.fall_back() {
                        // Leave the inner loop: on to the next target.
                        break;
                    }
                    // A request problem: nobody else may receive it.
                    return Execution {
                        attempts,
                        generation: None,
                    };
                }
            }
        }
    }

    Execution {
        attempts,
        generation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn first_retry_waits_100_to_300_ms() {
        assert_eq!(backoff(1, 0), MS(100));
        assert_eq!(backoff(1, 200), MS(300));
        assert_eq!(backoff(1, 201), MS(100), "wraps within the range");
        for random in [7, 99, 12_345, u64::MAX] {
            let wait = backoff(1, random);
            assert!((MS(100)..=MS(300)).contains(&wait), "{wait:?}");
        }
    }

    #[test]
    fn each_further_retry_doubles_the_range() {
        assert_eq!((backoff(2, 0), backoff(2, 400)), (MS(200), MS(600)));
        assert_eq!((backoff(3, 0), backoff(3, 800)), (MS(400), MS(1200)));
    }

    #[test]
    fn the_doubling_is_capped() {
        assert_eq!(backoff(usize::MAX, 0), backoff(11, 0));
    }
}
