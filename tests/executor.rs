//! The retry and fallback executor (Stage 2 slice 2b, ADR-028), with
//! fake providers.
//!
//! - Step 1: fallback down the candidates, stopping on request problems
//!   and at `max_attempts`.
//! - Step 2: a transient failure is retried once on the same target,
//!   then the executor falls back.
//! - Step 3: the whole request stays within `request_timeout`.
//! - Step 4: a jittered wait before a same-target retry, never past the
//!   deadline.
//!
//! Retry tests run on a paused clock, so later steps can add backoff
//! waits without slowing them down.

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
    harness_with(modes.map(FakeProvider::new))
}

fn harness_with(providers: [FakeProvider; 3]) -> Harness {
    let providers = Vec::from(providers);
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

// Step 2: one retry of the same target for transient failures.

fn calls_sequence(execution: &modelswitch::reliability::Execution) -> Vec<String> {
    execution
        .attempts
        .iter()
        .map(|attempt| attempt.target.to_string())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn transient_failure_is_retried_on_the_same_target() {
    let h = harness_with([
        FakeProvider::scripted([status(503)], answer("from a, second try")),
        FakeProvider::new(answer("from b")),
        FakeProvider::new(answer("from c")),
    ]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(
        execution.generation.as_ref().unwrap().output,
        "from a, second try"
    );
    assert_eq!(
        summary(&execution),
        [("a/m".to_owned(), false), ("a/m".to_owned(), true)]
    );
    assert_eq!(
        h.calls(),
        [2, 0, 0],
        "the preferred provider got its second chance"
    );
}

#[tokio::test(start_paused = true)]
async fn a_connection_failure_is_retried_too() {
    let h = harness_with([
        FakeProvider::scripted(
            [ProviderMode::Fail("connection reset".to_owned())],
            answer("from a"),
        ),
        FakeProvider::new(answer("from b")),
        FakeProvider::new(answer("from c")),
    ]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from a");
    assert_eq!(h.calls(), [2, 0, 0]);
}

#[tokio::test(start_paused = true)]
async fn a_second_transient_failure_falls_back() {
    let h = harness([status(503), answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from b");
    assert_eq!(calls_sequence(&execution), ["a/m", "a/m", "b/m"]);
    assert_eq!(h.calls(), [2, 1, 0], "only one retry per target");
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_hits_a_target_problem_falls_back() {
    let h = harness_with([
        FakeProvider::scripted([status(503)], status(429)),
        FakeProvider::new(answer("from b")),
        FakeProvider::new(answer("from c")),
    ]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from b");
    assert_eq!(calls_sequence(&execution), ["a/m", "a/m", "b/m"]);
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_hits_a_request_problem_stops() {
    let h = harness_with([
        FakeProvider::scripted([status(503)], blocked()),
        FakeProvider::new(answer("from b")),
        FakeProvider::new(answer("from c")),
    ]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert!(execution.generation.is_none());
    assert_eq!(
        h.calls(),
        [2, 0, 0],
        "a refused prompt must not be shopped around"
    );
}

#[tokio::test(start_paused = true)]
async fn retries_count_toward_max_attempts() {
    let h = harness([status(503), status(503), answer("from c")]);

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert!(
        execution.generation.is_none(),
        "c is past the attempt limit"
    );
    assert_eq!(calls_sequence(&execution), ["a/m", "a/m", "b/m"]);
    assert_eq!(h.calls(), [2, 1, 0]);
}

#[tokio::test(start_paused = true)]
async fn a_pin_is_retried_once_but_never_falls_back() {
    let h = harness([status(503), answer("from b"), answer("from c")]);

    let execution = execute(&targets(&["a"]), &h.registry, &request(), LIMITS).await;

    assert!(execution.generation.is_none());
    assert_eq!(h.calls(), [2, 0, 0]);
}

// Step 3: the whole request stays within `request_timeout`. Each provider
// times out after PROVIDER_TIMEOUT (5 s).

#[tokio::test(start_paused = true)]
async fn hanging_candidates_stop_at_the_request_deadline() {
    let h = harness([ProviderMode::Hang, ProviderMode::Hang, ProviderMode::Hang]);
    let limits = Limits {
        max_attempts: 3,
        request_timeout: Duration::from_secs(8),
    };
    let started = tokio::time::Instant::now();

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), limits).await;

    // a gets 5 s; b gets only the 3 s left; c is never called.
    assert_eq!(started.elapsed(), Duration::from_secs(8));
    assert_eq!(calls_sequence(&execution), ["a/m", "b/m"]);
    assert_eq!(h.calls(), [1, 1, 0]);
    assert_eq!(
        execution.attempts[1].failure,
        Some(AttemptFailure::TimedOut)
    );
}

#[tokio::test(start_paused = true)]
async fn a_deadline_shorter_than_the_provider_timeout_cuts_the_attempt() {
    let h = harness([ProviderMode::Hang, answer("from b"), answer("from c")]);
    let limits = Limits {
        max_attempts: 3,
        request_timeout: Duration::from_secs(2),
    };
    let started = tokio::time::Instant::now();

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), limits).await;

    assert_eq!(
        started.elapsed(),
        Duration::from_secs(2),
        "not the provider's 5 s"
    );
    assert!(execution.generation.is_none(), "no time left for b");
    assert_eq!(h.calls(), [1, 0, 0]);
}

#[tokio::test(start_paused = true)]
async fn time_left_after_a_timeout_still_reaches_the_next_candidate() {
    let h = harness([ProviderMode::Hang, answer("from b"), answer("from c")]);
    let limits = Limits {
        max_attempts: 3,
        request_timeout: Duration::from_secs(8),
    };

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), limits).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from b");
}

// Step 4: a jittered wait before retrying the same target, never past the
// request deadline. The paused clock makes the waits exact and instant.

#[tokio::test(start_paused = true)]
async fn a_retry_waits_100_to_300_ms_first() {
    let h = harness_with([
        FakeProvider::scripted([status(503)], answer("from a")),
        FakeProvider::new(answer("from b")),
        FakeProvider::new(answer("from c")),
    ]);
    let started = tokio::time::Instant::now();

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from a");
    let waited = started.elapsed();
    assert!(
        (Duration::from_millis(100)..=Duration::from_millis(300)).contains(&waited),
        "{waited:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn falling_back_needs_no_wait() {
    let h = harness([status(429), answer("from b"), answer("from c")]);
    let started = tokio::time::Instant::now();

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), LIMITS).await;

    assert_eq!(execution.generation.as_ref().unwrap().output, "from b");
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_retry_wait_past_the_deadline_falls_back_instead() {
    let h = harness([status(503), answer("from b"), answer("from c")]);
    let limits = Limits {
        max_attempts: 3,
        request_timeout: Duration::from_millis(50),
    };
    let started = tokio::time::Instant::now();

    let execution = execute(&targets(&["a", "b", "c"]), &h.registry, &request(), limits).await;

    assert_eq!(
        execution.generation.as_ref().unwrap().output,
        "from b",
        "the 100 ms wait did not fit, so b was tried at once"
    );
    assert_eq!(calls_sequence(&execution), ["a/m", "b/m"]);
    assert_eq!(started.elapsed(), Duration::ZERO);
}
