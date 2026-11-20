//! Benchmark storage and the `bench` commands against a real PostgreSQL
//! (ADR-038): runs are stored whole, priced exactly at run time, kept
//! out of request records, and summarised per candidate.

mod common;

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use common::{FakeProvider, ProviderMode};
use modelswitch::accounting::{Failure, Outcome};
use modelswitch::bench::{Bar, CandidateSummary, RunOptions, Sample, Workload};
use modelswitch::cli;
use modelswitch::domain::{Generation, Target, Usage};
use modelswitch::pricing::{PriceSource, UsdAmount};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::storage::{bench, pricing};

const WORKLOAD: &str = concat!(
    r#"{"id": "q1", "task": "arithmetic", "input": "What is 6 times 7? Answer with only the number.", "expect": {"kind": "number", "answer": 42}}"#,
    "\n",
);

fn workload() -> Workload {
    Workload::parse("test.jsonl", WORKLOAD).unwrap()
}

fn target(value: &str) -> Target {
    Target::parse(value).unwrap()
}

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
    }
}

fn answered(target_name: &str, passed: bool, latency_ms: u64, usage: Usage) -> Sample {
    Sample {
        item_id: "q1".to_owned(),
        repeat: 1,
        target: target(target_name),
        outcome: Outcome::Succeeded,
        passed,
        failure: Failure::default(),
        latency_ms,
        usage,
    }
}

fn failed(target_name: &str) -> Sample {
    Sample {
        outcome: Outcome::Failed,
        passed: false,
        failure: Failure {
            kind: Some("status"),
            code: Some("rate_limit_exceeded".to_owned()),
            provider_status: Some(429),
        },
        usage: Usage::unavailable(),
        ..answered(target_name, false, 60_000, Usage::unavailable())
    }
}

async fn price(pool: &PgPool, target_name: &str, input: &str, output: &str) {
    pricing::set(
        pool,
        &target(target_name),
        &UsdAmount::parse(input).unwrap(),
        &UsdAmount::parse(output).unwrap(),
        &PriceSource::parse("https://example.com/pricing").unwrap(),
    )
    .await
    .unwrap();
}

/// Sample number, model, status, passed, error kind, error code,
/// provider status, latency.
type StoredSample = (
    i32,
    String,
    String,
    bool,
    Option<String>,
    Option<String>,
    Option<i32>,
    i64,
);

#[sqlx::test]
async fn a_run_stores_verdicts_and_numbers_never_text(pool: PgPool) {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_name = 'bench_samples' ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        columns,
        [
            "run_id",
            "sample_number",
            "item_id",
            "repeat",
            "provider",
            "model",
            "status",
            "passed",
            "error_kind",
            "error_code",
            "provider_status",
            "latency_ms",
            "reported_input_tokens",
            "reported_output_tokens",
            "estimated_cost",
            "pricing_version",
        ],
        "no column for prompts or outputs"
    );

    let run = bench::save(
        &pool,
        "fast",
        &workload(),
        1,
        &[answered("p/a", true, 120, usage(10, 2)), failed("p/b")],
    )
    .await
    .unwrap();

    assert_eq!(run.alias, "fast");
    assert_eq!(run.workload_name, "test.jsonl");
    assert_eq!(run.workload_sha256, workload().sha256);
    let rows: Vec<StoredSample> = sqlx::query_as(
        "SELECT sample_number, model, status, passed, error_kind, error_code, \
                    provider_status, latency_ms \
             FROM bench_samples WHERE run_id = $1 ORDER BY sample_number",
    )
    .bind(run.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            (
                1,
                "a".to_owned(),
                "succeeded".to_owned(),
                true,
                None,
                None,
                None,
                120
            ),
            (
                2,
                "b".to_owned(),
                "failed".to_owned(),
                false,
                Some("status".to_owned()),
                Some("rate_limit_exceeded".to_owned()),
                Some(429),
                60_000
            ),
        ]
    );
}

#[sqlx::test]
async fn samples_are_priced_exactly_at_the_run_time(pool: PgPool) {
    price(&pool, "p/a", "0.5", "1.5").await;

    let run = bench::save(
        &pool,
        "fast",
        &workload(),
        1,
        &[
            answered("p/a", true, 100, usage(1000, 200)),
            answered(
                "p/a",
                true,
                100,
                Usage {
                    output_tokens: None,
                    ..usage(1000, 0)
                },
            ),
            answered("p/unpriced", true, 100, usage(1000, 200)),
            failed("p/a"),
        ],
    )
    .await
    .unwrap();

    let costs: Vec<(Option<String>, bool)> = sqlx::query_as(
        "SELECT trim_scale(estimated_cost)::text, pricing_version IS NOT NULL \
         FROM bench_samples WHERE run_id = $1 ORDER BY sample_number",
    )
    .bind(run.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        costs,
        [
            (Some("0.0008".to_owned()), true),
            (None, false),
            (None, false),
            (None, false),
        ]
    );
    let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM request_records")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(requests, 0, "benchmarks never become request records");
}

#[sqlx::test]
async fn the_latest_run_is_summarised_per_candidate(pool: PgPool) {
    price(&pool, "p/a", "1", "1").await;
    bench::save(&pool, "fast", &workload(), 1, &[failed("p/a")])
        .await
        .unwrap();
    let mut newest: Vec<Sample> = (1..=10)
        .map(|n| answered("p/a", n != 10, n * 100, usage(10, 0)))
        .collect();
    newest.push(failed("p/a"));
    newest.push(answered("p/b", true, 50, usage(1, 1)));
    let saved = bench::save(&pool, "fast", &workload(), 1, &newest)
        .await
        .unwrap();
    bench::save(&pool, "other", &workload(), 1, &[failed("p/a")])
        .await
        .unwrap();

    let (run, summaries) = bench::latest(&pool, "fast", None).await.unwrap().unwrap();

    assert_eq!(run.id, saved.id);
    assert_eq!(
        summaries,
        [
            CandidateSummary {
                target: target("p/a"),
                samples: 11,
                passed: 9,
                p50_ms: Some(500),
                p95_ms: Some(1000),
                mean_cost_usd: Some("0.00001".to_owned()),
                unpriced: 0,
                within_ceiling: None,
            },
            CandidateSummary {
                target: target("p/b"),
                samples: 1,
                passed: 1,
                p50_ms: Some(50),
                p95_ms: Some(50),
                mean_cost_usd: None,
                unpriced: 1,
                within_ceiling: None,
            },
        ]
    );
    assert!(bench::latest(&pool, "never", None).await.unwrap().is_none());
}

#[sqlx::test]
async fn the_ceiling_compares_the_exact_mean(pool: PgPool) {
    price(&pool, "p/a", "1", "0").await;
    // Costs 0.00001, 0.00002, 0.00002: mean 0.0000166666...
    bench::save(
        &pool,
        "fast",
        &workload(),
        1,
        &[
            answered("p/a", true, 1, usage(10, 0)),
            answered("p/a", true, 1, usage(20, 0)),
            answered("p/a", true, 1, usage(20, 0)),
        ],
    )
    .await
    .unwrap();
    let within = async |ceiling: &str| {
        let ceiling = UsdAmount::parse(ceiling).unwrap();
        let (_, summaries) = bench::latest(&pool, "fast", Some(&ceiling))
            .await
            .unwrap()
            .unwrap();
        (
            summaries[0].mean_cost_usd.clone(),
            summaries[0].within_ceiling,
        )
    };

    assert_eq!(
        within("0.000016666").await,
        (Some("0.000016667".to_owned()), Some(false))
    );
    assert_eq!(within("0.000016667").await.1, Some(true));
    assert_eq!(within("1").await.1, Some(true));

    // A mean exactly at the ceiling is within it: 0.00001 and 0.00003.
    bench::save(
        &pool,
        "exact",
        &workload(),
        1,
        &[
            answered("p/a", true, 1, usage(10, 0)),
            answered("p/a", true, 1, usage(30, 0)),
        ],
    )
    .await
    .unwrap();
    let ceiling = UsdAmount::parse("0.00002").unwrap();
    let (_, summaries) = bench::latest(&pool, "exact", Some(&ceiling))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summaries[0].mean_cost_usd.as_deref(), Some("0.00002"));
    assert_eq!(summaries[0].within_ceiling, Some(true));
}

fn reply(output: &str) -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: output.to_owned(),
        usage: usage(10, 1),
    })
}

fn registry(providers: &[(&str, FakeProvider)]) -> Registry {
    Registry::new(providers.iter().map(|(name, fake)| {
        let slot = ProviderSlot {
            provider: Arc::new(fake.clone()),
            timeout: Duration::from_secs(5),
        };
        ((*name).to_owned(), slot)
    }))
}

#[sqlx::test]
async fn bench_run_stores_every_call_and_ranks_the_candidates(pool: PgPool) {
    let registry = registry(&[
        ("right", FakeProvider::new(reply("42"))),
        ("wrong", FakeProvider::new(reply("41"))),
    ]);
    let candidates = [target("wrong/w"), target("right/r")];
    let options = RunOptions {
        repeats: 30,
        min_interval: Duration::ZERO,
    };
    let mut out = Vec::new();

    cli::bench::run(
        &pool,
        &registry,
        "fast",
        &candidates,
        &workload(),
        options,
        &mut out,
    )
    .await
    .unwrap();

    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("2 candidates x 1 items x 30 repeats = 60 calls"),
        "{out}"
    );
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM bench_samples")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 60);
    let table: Vec<&str> = out
        .lines()
        .skip_while(|line| !line.starts_with("RANK"))
        .skip(1)
        .collect();
    assert_eq!(table.len(), 2, "{out}");
    assert!(table[0].starts_with("1     right/r"), "{out}");
    assert!(
        table[0].contains("30/30 (100.0%)") && table[0].ends_with("eligible"),
        "{out}"
    );
    assert!(table[1].starts_with("-     wrong/w"), "{out}");
    assert!(
        table[1].contains("0/30 (0.0%)") && table[1].ends_with("below success bar"),
        "{out}"
    );
    assert!(
        table.iter().all(|line| line.contains("unknown")),
        "unpriced: {out}"
    );
}

#[sqlx::test]
async fn bench_report_applies_the_bar_and_ceiling(pool: PgPool) {
    price(&pool, "p/cheap", "1", "1").await;
    price(&pool, "p/dear", "100", "100").await;
    let samples: Vec<Sample> = (1..=30)
        .flat_map(|n| {
            [
                answered("p/dear", true, 100 + n, usage(10, 0)),
                answered("p/cheap", n > 2, 300 + n, usage(10, 0)),
            ]
        })
        .collect();
    bench::save(&pool, "fast", &workload(), 30, &samples)
        .await
        .unwrap();
    let candidates = [target("p/dear"), target("p/cheap")];
    let report = async |bar: Bar, ceiling: Option<&str>| {
        let ceiling = ceiling.map(|c| UsdAmount::parse(c).unwrap());
        let mut out = Vec::new();
        cli::bench::report(&pool, "fast", &candidates, &bar, ceiling.as_ref(), &mut out)
            .await
            .unwrap();
        String::from_utf8(out).unwrap()
    };
    let verdicts = |out: &str| -> Vec<String> {
        out.lines()
            .skip_while(|line| !line.starts_with("RANK"))
            .skip(1)
            .map(|line| {
                line.split("  ").next().unwrap().to_owned()
                    + " "
                    + line.rsplit("  ").next().unwrap()
            })
            .collect()
    };

    let default = report(Bar::default(), None).await;
    assert!(
        default.contains(
            "Bar: at least 90% success over at least 30 samples; mean cost ceiling: none."
        ),
        "{default}"
    );
    assert_eq!(
        verdicts(&default),
        ["1 eligible", "2 eligible"],
        "{default}"
    );
    assert!(
        default.lines().any(|l| l.starts_with("1     p/dear")),
        "faster first: {default}"
    );

    let undecided = report(
        Bar {
            min_samples: 1000,
            ..Bar::default()
        },
        None,
    )
    .await;
    let names: Vec<_> = undecided
        .lines()
        .skip_while(|line| !line.starts_with("RANK"))
        .skip(1)
        .map(|line| line.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(
        names,
        ["p/dear", "p/cheap"],
        "configured order: {undecided}"
    );

    let strict = report(
        Bar {
            min_success_percent: 95,
            ..Bar::default()
        },
        None,
    )
    .await;
    assert_eq!(
        verdicts(&strict),
        ["1 eligible", "- below success bar"],
        "{strict}"
    );

    let capped = report(Bar::default(), Some("0.0001")).await;
    assert_eq!(
        verdicts(&capped),
        ["1 eligible", "- over cost ceiling"],
        "{capped}"
    );
    assert!(
        capped.lines().any(|l| l.starts_with("1     p/cheap")),
        "{capped}"
    );

    let mut none = Vec::new();
    cli::bench::report(&pool, "slow", &[], &Bar::default(), None, &mut none)
        .await
        .unwrap();
    assert!(
        String::from_utf8(none)
            .unwrap()
            .starts_with("No benchmark runs for alias \"slow\"")
    );
}
