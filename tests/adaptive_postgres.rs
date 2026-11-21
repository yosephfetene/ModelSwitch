//! Adaptive routing data from a real PostgreSQL (ADR-039): live
//! attempts within the window, the latest benchmark with the ceiling
//! applied, and the routing reason stored with each request and shown in
//! history.

mod common;

use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use modelswitch::accounting::{
    AttemptRecord, Failure, Outcome, RecordStore, RequestRecord, RetryDecision,
};
use modelswitch::bench::{Bar, Sample, Workload};
use modelswitch::cli::keys;
use modelswitch::domain::{Target, Usage};
use modelswitch::history::HistoryStore;
use modelswitch::pricing::UsdAmount;
use modelswitch::reliability::FailureClass;
use modelswitch::routing::adaptive::{AdaptivePolicy, LiveStat, RoutingSource};
use modelswitch::storage::history::PgHistoryStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::storage::routing::{PgRoutingSource, live};
use modelswitch::storage::{bench, pricing};

fn target(value: &str) -> Target {
    Target::parse(value).unwrap()
}

fn policy(window_secs: u64) -> AdaptivePolicy {
    AdaptivePolicy {
        bar: Bar::default(),
        max_mean_cost: None,
        window: Duration::from_secs(window_secs),
        min_live_requests: 10,
    }
}

async fn key(pool: &PgPool) -> Uuid {
    let mut out = Vec::new();
    keys::create(pool, "laptop", Default::default(), &mut out)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT id FROM client_api_keys")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn attempt(name: &str, outcome: Outcome, latency_ms: u64) -> AttemptRecord {
    let target = target(name);
    let failed = outcome != Outcome::Succeeded;
    AttemptRecord {
        provider: target.provider,
        model: target.model,
        outcome,
        failure: if outcome == Outcome::Failed {
            Failure {
                kind: Some("status"),
                code: None,
                provider_status: Some(503),
            }
        } else {
            Failure::default()
        },
        class: failed.then_some(FailureClass::Transient),
        decision: if failed {
            RetryDecision::Fallback
        } else {
            RetryDecision::NotRetried
        },
        latency_ms,
    }
}

/// Writes one request through the record store, as the gateway does.
async fn write(
    pool: &PgPool,
    key: Uuid,
    attempts: Vec<AttemptRecord>,
    reason: Option<&str>,
) -> String {
    let last = attempts.last().unwrap().clone();
    let record = RequestRecord {
        request_id: format!("req_{}", Uuid::now_v7()),
        client_request_id: None,
        client_api_key_id: key,
        requested_model: "smart".to_owned(),
        provider: last.provider,
        model: last.model,
        outcome: last.outcome,
        failure: last.failure,
        latency_ms: 0,
        usage: Usage::unavailable(),
        attempts,
        routing_reason: reason.map(str::to_owned),
    };
    PgRecordStore::new(pool.clone())
        .insert(&record)
        .await
        .unwrap();
    record.request_id
}

#[sqlx::test]
async fn live_stats_cover_the_candidates_within_the_window(pool: PgPool) {
    let key = key(&pool).await;
    for latency in [100, 200, 300] {
        write(
            &pool,
            key,
            vec![attempt("p/a", Outcome::Succeeded, latency)],
            None,
        )
        .await;
    }
    write(
        &pool,
        key,
        vec![
            attempt("p/a", Outcome::Failed, 5000),
            attempt("p/b", Outcome::Succeeded, 40),
        ],
        None,
    )
    .await;
    write(
        &pool,
        key,
        vec![attempt("p/a", Outcome::TimedOut, 9000)],
        None,
    )
    .await;
    write(
        &pool,
        key,
        vec![attempt("other/x", Outcome::Succeeded, 1)],
        None,
    )
    .await;
    let old = write(
        &pool,
        key,
        vec![attempt("p/b", Outcome::Succeeded, 7)],
        None,
    )
    .await;
    sqlx::query("UPDATE request_records SET created_at = now() - interval '1 hour' WHERE id = $1")
        .bind(&old)
        .execute(&pool)
        .await
        .unwrap();

    let mut stats = live(&pool, &[target("p/a"), target("p/b")], &policy(900))
        .await
        .unwrap();

    stats.sort_by_key(|stat| stat.target.to_string());
    assert_eq!(
        stats,
        [
            LiveStat {
                target: target("p/a"),
                attempts: 5,
                succeeded: 3,
                p50_ms: Some(200),
            },
            LiveStat {
                target: target("p/b"),
                attempts: 1,
                succeeded: 1,
                p50_ms: Some(40),
            },
        ],
        "failures count, but not in latency; other targets and old requests are left out"
    );
    let wide = live(&pool, &[target("p/b")], &policy(7200)).await.unwrap();
    assert_eq!(
        wide[0].attempts, 2,
        "a wider window reaches the old request"
    );
}

#[sqlx::test]
async fn the_source_loads_the_latest_benchmark_with_the_ceiling(pool: PgPool) {
    let source = PgRoutingSource::new(pool.clone());
    let candidates = [target("p/a")];
    let mut capped = policy(900);
    capped.max_mean_cost = Some(UsdAmount::parse("0.000001").unwrap());

    let before = source.load(&candidates, "smart", &capped).await.unwrap();
    assert_eq!(before.bench, None, "never benchmarked");

    pricing::set(
        &pool,
        &target("p/a"),
        &UsdAmount::parse("1").unwrap(),
        &UsdAmount::parse("1").unwrap(),
        &modelswitch::pricing::PriceSource::parse("https://example.com").unwrap(),
    )
    .await
    .unwrap();
    let workload = Workload::parse(
        "w.jsonl",
        r#"{"id": "q1", "task": "format", "input": "Say only x.", "expect": {"kind": "exact", "answer": "x"}}"#,
    )
    .unwrap();
    let sample = Sample {
        item_id: "q1".to_owned(),
        repeat: 1,
        target: target("p/a"),
        outcome: Outcome::Succeeded,
        passed: true,
        failure: Failure::default(),
        latency_ms: 50,
        usage: Usage {
            input_tokens: Some(10),
            output_tokens: Some(0),
            total_tokens: Some(10),
        },
    };
    bench::save(&pool, "smart", &workload, 1, &[sample])
        .await
        .unwrap();

    let after = source.load(&candidates, "smart", &capped).await.unwrap();

    let summaries = after.bench.unwrap();
    assert_eq!(summaries[0].target, target("p/a"));
    assert_eq!(summaries[0].p50_ms, Some(50));
    assert_eq!(
        summaries[0].within_ceiling,
        Some(false),
        "0.00001 > 0.000001"
    );
    assert!(
        source
            .load(&candidates, "other", &capped)
            .await
            .unwrap()
            .bench
            .is_none(),
        "benchmarks belong to their alias"
    );
}

#[sqlx::test]
async fn the_routing_reason_is_stored_and_shown_in_history(pool: PgPool) {
    let key = key(&pool).await;
    let reason = "p/a first: fastest of 1 eligible by benchmark p50 50 ms";
    let adaptive = write(
        &pool,
        key,
        vec![attempt("p/a", Outcome::Succeeded, 10)],
        Some(reason),
    )
    .await;
    let plain = write(
        &pool,
        key,
        vec![attempt("p/a", Outcome::Succeeded, 10)],
        None,
    )
    .await;
    let history = PgHistoryStore::new(pool.clone());

    let detail = history.get(&adaptive).await.unwrap().unwrap();
    assert_eq!(detail.routing_reason.as_deref(), Some(reason));
    let detail = history.get(&plain).await.unwrap().unwrap();
    assert_eq!(detail.routing_reason, None);

    let too_long = "x".repeat(257);
    let refused = sqlx::query("UPDATE request_records SET routing_reason = $1 WHERE id = $2")
        .bind(&too_long)
        .bind(&plain)
        .execute(&pool)
        .await;
    assert!(
        refused.is_err(),
        "the schema caps a reason at 256 characters"
    );
}
