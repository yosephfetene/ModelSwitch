//! The benchmark runner against fake providers (Stage 5, ADR-038): each
//! candidate pinned, called once per sample, under its deadline, graded
//! against the answer key, and never logging prompts or outputs. Also
//! checks the shipped workload file. Storage is tested in
//! `tests/bench_postgres.rs`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use common::{CapturedLogs, FakeProvider, ProviderMode};
use modelswitch::accounting::Outcome;
use modelswitch::bench::run::MAX_OUTPUT_TOKENS;
use modelswitch::bench::{Expect, RunError, RunOptions, Task, Workload, run};
use modelswitch::domain::{Generation, ProviderError, Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};

const TIMEOUT: Duration = Duration::from_secs(10);

const WORKLOAD: &str = concat!(
    r#"{"id": "q1", "task": "arithmetic", "input": "PROMPT-ONE What is 6 times 7?", "expect": {"kind": "number", "answer": 42}}"#,
    "\n",
    r#"{"id": "q2", "task": "format", "input": "PROMPT-TWO Capital of France?", "expect": {"kind": "exact", "answer": "Paris"}}"#,
    "\n",
);

fn workload() -> Workload {
    Workload::parse("test.jsonl", WORKLOAD).unwrap()
}

fn usage() -> Usage {
    Usage {
        input_tokens: Some(12),
        output_tokens: Some(3),
        total_tokens: Some(15),
    }
}

fn reply(output: &str) -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: output.to_owned(),
        usage: usage(),
    })
}

/// Also installs the log capture, which must precede any log line.
fn registry(providers: &[(&str, &FakeProvider)]) -> Registry {
    CapturedLogs::global();
    Registry::new(providers.iter().map(|(name, fake)| {
        let slot = ProviderSlot {
            provider: Arc::new((*fake).clone()),
            timeout: TIMEOUT,
        };
        ((*name).to_owned(), slot)
    }))
}

fn target(value: &str) -> Target {
    Target::parse(value).unwrap()
}

fn options(repeats: u32) -> RunOptions {
    RunOptions {
        repeats,
        min_interval: Duration::ZERO,
    }
}

/// Answers each prompt correctly: 42 to the first, Paris to the second.
fn answering_correctly() -> FakeProvider {
    FakeProvider::scripted(
        [reply("42"), reply("Paris"), reply("42"), reply("Paris")],
        reply("42"),
    )
}

#[tokio::test]
async fn candidates_are_interleaved_item_by_item_and_repeated() {
    let a = answering_correctly();
    let b = answering_correctly();
    let registry = registry(&[("a", &a), ("b", &b)]);

    let samples = run(
        &registry,
        &[target("a/m1"), target("b/m2")],
        &workload(),
        &options(2),
    )
    .await
    .unwrap();

    let order: Vec<_> = samples
        .iter()
        .map(|s| (s.repeat, s.item_id.as_str(), s.target.to_string()))
        .collect();
    assert_eq!(
        order,
        [
            (1, "q1", "a/m1".to_owned()),
            (1, "q1", "b/m2".to_owned()),
            (1, "q2", "a/m1".to_owned()),
            (1, "q2", "b/m2".to_owned()),
            (2, "q1", "a/m1".to_owned()),
            (2, "q1", "b/m2".to_owned()),
            (2, "q2", "a/m1".to_owned()),
            (2, "q2", "b/m2".to_owned()),
        ]
    );
    assert!(
        samples
            .iter()
            .all(|s| s.passed && s.outcome == Outcome::Succeeded)
    );
    assert!(samples.iter().all(|s| s.usage == usage()));

    let calls = a.calls();
    assert_eq!(calls.len(), 4);
    assert!(calls.iter().all(|(model, _)| model == "m1"), "pinned");
    assert!(calls[0].1.input.starts_with("PROMPT-ONE"));
    assert!(calls[1].1.input.starts_with("PROMPT-TWO"));
    assert!(
        calls
            .iter()
            .all(|(_, request)| request.max_output_tokens == Some(MAX_OUTPUT_TOKENS))
    );
}

#[tokio::test]
async fn a_wrong_answer_is_answered_but_not_passed() {
    let fake = FakeProvider::new(reply("The answer is 42."));
    let registry = registry(&[("p", &fake)]);

    let samples = run(&registry, &[target("p/m")], &workload(), &options(1))
        .await
        .unwrap();

    assert_eq!(samples.len(), 2);
    for sample in samples {
        assert_eq!(sample.outcome, Outcome::Succeeded);
        assert!(!sample.passed, "{}", sample.item_id);
        assert_eq!(sample.usage, usage(), "usage still counts");
    }
}

#[tokio::test]
async fn a_failure_is_recorded_once_and_never_retried() {
    let fake = FakeProvider::scripted(
        [ProviderMode::Error(ProviderError::Status {
            status: 503,
            code: Some("overloaded".to_owned()),
        })],
        reply("Paris"),
    );
    let registry = registry(&[("p", &fake)]);

    let samples = run(&registry, &[target("p/m")], &workload(), &options(1))
        .await
        .unwrap();

    assert_eq!(fake.calls().len(), 2, "one call per sample");
    let failed = &samples[0];
    assert_eq!(failed.item_id, "q1");
    assert_eq!(failed.outcome, Outcome::Failed);
    assert!(!failed.passed);
    assert_eq!(failed.failure.kind, Some("status"));
    assert_eq!(failed.failure.code.as_deref(), Some("overloaded"));
    assert_eq!(failed.failure.provider_status, Some(503));
    assert_eq!(failed.usage, Usage::unavailable());
    assert!(samples[1].passed, "the next item still runs");
}

#[tokio::test(start_paused = true)]
async fn a_hung_call_times_out_at_the_provider_deadline() {
    let fake = FakeProvider::scripted([ProviderMode::Hang], reply("Paris"));
    let registry = registry(&[("p", &fake)]);

    let samples = run(&registry, &[target("p/m")], &workload(), &options(1))
        .await
        .unwrap();

    let timed_out = &samples[0];
    assert_eq!(timed_out.outcome, Outcome::TimedOut);
    assert!(!timed_out.passed);
    assert_eq!(timed_out.latency_ms, 10_000);
    assert_eq!(timed_out.failure, Default::default());
    assert_eq!(samples[1].outcome, Outcome::Succeeded);
}

#[tokio::test(start_paused = true)]
async fn calls_are_spaced_by_the_minimum_interval() {
    let fake = FakeProvider::new(reply("42"));
    let registry = registry(&[("p", &fake)]);
    let started = Instant::now();

    let samples = run(
        &registry,
        &[target("p/m")],
        &workload(),
        &RunOptions {
            repeats: 2,
            min_interval: Duration::from_secs(6),
        },
    )
    .await
    .unwrap();

    assert_eq!(samples.len(), 4);
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(18),
        "3 gaps, none before the first"
    );
}

#[tokio::test]
async fn an_unknown_provider_stops_the_run_before_any_call() {
    let fake = FakeProvider::new(reply("42"));
    let registry = registry(&[("p", &fake)]);

    let result = run(
        &registry,
        &[target("p/m"), target("missing/m")],
        &workload(),
        &options(1),
    )
    .await;

    assert_eq!(result, Err(RunError::UnknownProvider("missing".to_owned())));
    assert!(fake.calls().is_empty());
    assert_eq!(
        run(&registry, &[target("p/m")], &workload(), &options(0)).await,
        Err(RunError::NoRepeats)
    );
}

#[tokio::test]
async fn prompts_and_outputs_never_reach_the_logs() {
    let fake = FakeProvider::new(reply("SECRET-OUTPUT-TEXT"));
    let registry = registry(&[("logcheck", &fake)]);

    run(&registry, &[target("logcheck/m")], &workload(), &options(1))
        .await
        .unwrap();

    let text = CapturedLogs::global().text();
    let lines: Vec<_> = text.lines().filter(|l| l.contains("logcheck/m")).collect();
    assert_eq!(lines.len(), 2, "one line per sample:\n{text}");
    assert!(
        lines
            .iter()
            .all(|l| l.contains("item=q") && l.contains("latency_ms="))
    );
    assert!(!text.contains("SECRET-OUTPUT-TEXT"), "{text}");
    assert!(
        !text.contains("PROMPT-ONE") && !text.contains("PROMPT-TWO"),
        "{text}"
    );
}

/// The answer an ideal model would give, for checking the answer keys.
fn ideal_answer(expect: &Expect) -> String {
    match expect {
        Expect::Exact { answer, .. } => answer.clone(),
        Expect::Number { answer } => answer.to_string(),
        Expect::Json { fields } => Value::Object(fields.clone()).to_string(),
    }
}

#[test]
fn the_shipped_workload_is_balanced_and_its_keys_are_satisfiable() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/bench/workload-v1.jsonl");
    let text = std::fs::read_to_string(path).unwrap();

    let workload = Workload::parse("workload-v1.jsonl", &text).unwrap();

    assert_eq!(workload.items.len(), 40);
    for task in [
        Task::Extraction,
        Task::Classification,
        Task::Arithmetic,
        Task::Format,
    ] {
        let count = workload.items.iter().filter(|i| i.task == task).count();
        assert_eq!(count, 10, "{task:?}");
    }
    for item in &workload.items {
        assert!(
            item.expect.passes(&ideal_answer(&item.expect)),
            "{}",
            item.id
        );
        assert!(!item.expect.passes(""), "{}", item.id);
        assert!(
            item.input.contains("only") || item.input.contains("Only"),
            "{}: every prompt says what to answer with",
            item.id
        );
    }
}
