//! The executor: tries a request's candidate targets with retries and
//! fallback, within the attempt and time limits (ADR-028).

use std::time::Duration;

use super::{FailureClass, classify};
use crate::domain::{GenerateRequest, Generation, ProviderError, Target};
use crate::providers::Registry;

/// How many times a transient failure is retried on the same target
/// before falling back: one quick second chance (ADR-028).
pub const RETRIES_PER_TARGET: usize = 1;

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

/// Calls `target` once under its provider's timeout. A target whose
/// provider is not registered fails as a request error; the handler
/// checks registration before calling the executor, so this is only a
/// safety net.
async fn call_once(
    providers: &Registry,
    target: &Target,
    request: &GenerateRequest,
) -> Result<Generation, AttemptFailure> {
    let Some(slot) = providers.get(&target.provider) else {
        return Err(AttemptFailure::Provider(ProviderError::Request(
            "provider is not registered".to_owned(),
        )));
    };
    let call = slot.provider.generate(&target.model, request);
    match tokio::time::timeout(slot.timeout, call).await {
        Ok(Ok(generation)) => Ok(generation),
        Ok(Err(error)) => Err(AttemptFailure::Provider(error)),
        Err(_) => Err(AttemptFailure::TimedOut),
    }
}

/// Tries `targets` in order until one answers (ADR-028).
///
/// Rules:
///
/// - Stop once `limits.max_attempts` attempts have been made.
/// - On success: record it and return the answer.
/// - On failure: record it. If its class allows retrying the same target
///   and this target has been retried fewer than `RETRIES_PER_TARGET`
///   times, call it again. Otherwise, if its class allows falling back,
///   go on to the next target; if not, stop.
/// - If no target answers, return every attempt and no answer.
pub async fn execute(
    targets: &[Target],
    providers: &Registry,
    request: &GenerateRequest,
    limits: Limits,
) -> Execution {
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

            match call_once(providers, target, request).await {
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
                        // Transient, with a retry left: same target again.
                        retries += 1;
                        continue;
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
