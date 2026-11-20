//! Running a workload against candidates (ADR-038).
//!
//! Every call is pinned to one candidate, runs under that provider's
//! deadline, and is made exactly once: no retries and no fallback, so a
//! sample measures that candidate alone. Candidates are interleaved item
//! by item, so a slow minute or a quota reset affects them alike.

use std::time::Duration;

use tokio::time::Instant;

use super::workload::Workload;
use crate::accounting::{Failure, Outcome};
use crate::domain::{GenerateRequest, Target, Usage};
use crate::providers::Registry;

/// Output tokens each answer may use. Every answer key is short.
pub const MAX_OUTPUT_TOKENS: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    /// How many times each item is asked of each candidate.
    pub repeats: u32,
    /// The least time between the start of one call and the next, to
    /// stay within free-tier request limits.
    pub min_interval: Duration,
}

/// One graded call. Holds no prompt and no output, by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub item_id: String,
    /// 1 for the first time the item is asked.
    pub repeat: u32,
    pub target: Target,
    pub outcome: Outcome,
    /// Answered, and the answer passed its check.
    pub passed: bool,
    pub failure: Failure,
    pub latency_ms: u64,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("{0} is not a configured provider")]
    UnknownProvider(String),
    #[error("repeats must be at least 1")]
    NoRepeats,
}

/// Asks every item of `workload` of every target, `repeats` times, and
/// grades each answer. Returns the samples in the order they were taken.
pub async fn run(
    registry: &Registry,
    targets: &[Target],
    workload: &Workload,
    options: &RunOptions,
) -> Result<Vec<Sample>, RunError> {
    if options.repeats == 0 {
        return Err(RunError::NoRepeats);
    }
    // Every target is checked before the first call, so a typo cannot
    // waste a partial run.
    let slots = targets
        .iter()
        .map(|target| {
            registry
                .get(&target.provider)
                .map(|slot| (target, slot))
                .ok_or_else(|| RunError::UnknownProvider(target.provider.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut samples = Vec::new();
    let mut last_start: Option<Instant> = None;
    for repeat in 1..=options.repeats {
        for item in &workload.items {
            let request = GenerateRequest {
                input: item.input.clone(),
                max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            };
            for &(target, slot) in &slots {
                if let Some(last) = last_start {
                    tokio::time::sleep_until(last + options.min_interval).await;
                }
                let started = Instant::now();
                last_start = Some(started);
                let result = tokio::time::timeout(
                    slot.timeout,
                    slot.provider.generate(&target.model, &request),
                )
                .await;
                let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let (outcome, passed, failure, usage) = match result {
                    Ok(Ok(generation)) => (
                        Outcome::Succeeded,
                        item.expect.passes(&generation.output),
                        Failure::default(),
                        generation.usage,
                    ),
                    Ok(Err(error)) => (
                        Outcome::Failed,
                        false,
                        Failure::from_error(&error),
                        Usage::unavailable(),
                    ),
                    Err(_elapsed) => (
                        Outcome::TimedOut,
                        false,
                        Failure::default(),
                        Usage::unavailable(),
                    ),
                };
                tracing::info!(
                    item = %item.id,
                    repeat,
                    target = %target,
                    outcome = outcome.as_str(),
                    passed,
                    latency_ms,
                    "benchmark sample"
                );
                samples.push(Sample {
                    item_id: item.id.clone(),
                    repeat,
                    target: target.clone(),
                    outcome,
                    passed,
                    failure,
                    latency_ms,
                    usage,
                });
            }
        }
    }
    Ok(samples)
}
