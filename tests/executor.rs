//! The retry and fallback executor (Stage 2 slice 2b, ADR-028), with
//! fake providers. Step 1: fallback down the candidates, stopping on
//! request problems and at `max_attempts`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{FakeProvider, ProviderMode};
use modelswitch::domain::{GenerateRequest, Generation, ProviderError, Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::reliability::{AttemptFailure, Limits, execute};

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

const LIMITS: Limits = Limits {
    max_attempts: 3,
    request_timeout: Duration::from_secs(60),
};

/// Providers `a`, `b`, and `c`, each serving model `m`, behaving as given.
struct Harness {
    registry: Registry,
    providers: Vec<FakeProvider>,
}

fn harness(modes: [ProviderMode; 3]) -> Harness {
    let providers: Vec<FakeProvider> = modes.into_iter().map(FakeProvider::new).collect();
    let registry = Registry::new(["a", "b", "c"].into_iter().zip(&providers).map(
        |(name, provider)| {
            (
                name.to_owned(),
                ProviderSlot {
                    provider: Arc::new(provider.clone()),
                    timeout: PROVIDER_TIMEOUT,
                },
            )
        },
    ));
    Harness {
        registry,
        providers,
    }
}

impl Harness {
    /// How many times each of `a`, `b`, `c` was called.
    fn calls(&self) -> [usize; 3] {
        [0, 1, 2].map(|i| self.providers[i].calls().len())
    }
}

fn targets(names: &[&str]) -> Vec<Target> {
    names
        .iter()
        .map(|name| Target::parse(&format!("{name}/m")).unwrap())
        .collect()
}

fn request() -> GenerateRequest {
    GenerateRequest {
        input: "Hello".to_owned(),
        max_output_tokens: None,
    }
}

fn answer(output: &str) -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: output.to_owned(),
        usage: Usage::unavailable(),
    })
}

fn status(status: u16) -> ProviderMode {
    ProviderMode::Error(ProviderError::Status { status, code: None })
}

fn blocked() -> ProviderMode {
    ProviderMode::Error(ProviderError::Blocked {
        reason: Some("SAFETY".to_owned()),
    })
}

/// The target and whether it succeeded, for each attempt.
fn summary(execution: &modelswitch::reliability::Execution) -> Vec<(String, bool)> {
    execution
        .attempts
        .iter()
        .map(|attempt| (attempt.target.to_string(), attempt.failure.is_none()))
        .collect()
}

#[tokio::test]
async fn first_candidate_answers() {
    let h = harness([answer("from a"), answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from a");
    assert_eq!(summary(&execution), [("a/m".to_owned(), true)]);
    assert_eq!(h.calls(), [1, 0, 0], "later candidates are not called");
}

#[tokio::test]
async fn target_problem_falls_back_to_the_next_candidate() {
    let h = harness([status(429), answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from b");
    assert_eq!(
        summary(&execution),
        [("a/m".to_owned(), false), ("b/m".to_owned(), true)]
    );
    assert_eq!(
        execution.attempts[0].failure,
        Some(AttemptFailure::Provider(ProviderError::Status {
            status: 429,
            code: None
        }))
    );
}

#[tokio::test]
async fn request_problem_stops_without_trying_other_providers() {
    let h = harness([blocked(), answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert!(execution.generation.is_none());
    assert_eq!(summary(&execution), [("a/m".to_owned(), false)]);
    assert_eq!(
        h.calls(),
        [1, 0, 0],
        "a refused prompt must not be shopped around"
    );
}

#[tokio::test]
async fn every_candidate_failing_returns_every_attempt() {
    let h = harness([status(429), status(404), status(401)]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert!(execution.generation.is_none());
    assert_eq!(
        summary(&execution),
        [
            ("a/m".to_owned(), false),
            ("b/m".to_owned(), false),
            ("c/m".to_owned(), false)
        ]
    );
}

#[tokio::test]
async fn attempts_stop_at_max_attempts() {
    let h = harness([status(429), status(429), answer("from c")]);
    let limits = Limits {
        max_attempts: 2,
        ..LIMITS
    };

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), limits).await;

    assert!(
        execution.generation.is_none(),
        "c is past the attempt limit"
    );
    assert_eq!(execution.attempts.len(), 2);
    assert_eq!(h.calls(), [1, 1, 0]);
}

#[tokio::test]
async fn a_pin_has_only_its_own_target() {
    let h = harness([status(429), answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a"]), &h.registry, &request(), LIMITS).await;

    assert!(execution.generation.is_none());
    assert_eq!(h.calls(), [1, 0, 0], "a pin never falls back");
}

#[tokio::test(start_paused = true)]
async fn a_timeout_falls_back_to_the_next_candidate() {
    let h = harness([ProviderMode::Hang, answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from b");
    assert_eq!(
        execution.attempts[0].failure,
        Some(AttemptFailure::TimedOut)
    );
}
