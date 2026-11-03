//! Request records (Stage 1 slice 6 spec, ADR-027), with fake providers
//! and a fake record store.
//!
//! 1. Every request that reached a provider gets one record: success,
//!    provider error, or timeout. Rejected requests get none.
//! 2. The record is written before responding, under a deadline; if it
//!    fails, the client still gets the response, marked
//!    `X-ModelSwitch-Accounting: failed`.
//! 3. A record holds identifiers, outcome, latency, and reported tokens,
//!    never the prompt or the output.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

use common::{
    ACCOUNTING_TIMEOUT, CapturedLogs, DbMode, FakeDatabase, FakeKeyStore, FakeProvider,
    FakeRecordStore, ProviderMode, SENSITIVE_ERROR, TestResponse, send, state_with_records,
};
use modelswitch::accounting::{Failure, Outcome};
use modelswitch::build_router;
use modelswitch::domain::{ProviderError, Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use uuid::Uuid;

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);
const ACCOUNTING: &str = "x-modelswitch-accounting";

struct Harness {
    app: Router,
    key: String,
    key_id: Uuid,
    records: FakeRecordStore,
}

/// Provider `p` serves `m`; alias `fast` points at `p/m`.
fn harness(provider: ProviderMode, records: DbMode) -> Harness {
    CapturedLogs::global();
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, key_id) = keys.add_key();
    let records = FakeRecordStore::new(records);
    let mut state = state_with_records(FakeDatabase::new(DbMode::Up), keys, records.clone());
    state.catalog = Arc::new(Catalog::new(
        [("p".to_owned(), vec!["m".to_owned()])],
        [("fast".to_owned(), vec![Target::parse("p/m").unwrap()])],
    ));
    state.providers = Arc::new(Registry::new([(
        "p".to_owned(),
        ProviderSlot {
            provider: Arc::new(FakeProvider::new(provider)),
            timeout: PROVIDER_TIMEOUT,
        },
    )]));
    Harness {
        app: build_router(state),
        key,
        key_id,
        records,
    }
}

fn replying(output: &str, usage: Usage) -> ProviderMode {
    ProviderMode::Reply(modelswitch::domain::Generation {
        output: output.to_owned(),
        usage,
    })
}

fn reported() -> Usage {
    Usage {
        input_tokens: Some(12),
        output_tokens: Some(3),
        total_tokens: Some(15),
    }
}

impl Harness {
    async fn post_with(&self, key: &str, body: serde_json::Value) -> TestResponse {
        send(
            &self.app,
            Request::post("/v1/generate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .header("x-client-request-id", "order-42")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn post(&self, body: serde_json::Value) -> TestResponse {
        self.post_with(&self.key.clone(), body).await
    }
}

// 1: success
#[tokio::test]
async fn success_writes_one_complete_record() {
    let h = harness(replying("generated", reported()), DbMode::Up);

    let response = h.post(json!({ "model": "fast", "input": "Hello" })).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.header(ACCOUNTING), None);
    let body = response.json();
    let records = h.records.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.request_id, body["id"]);
    assert_eq!(record.client_request_id.as_deref(), Some("order-42"));
    assert_eq!(record.client_api_key_id, h.key_id);
    assert_eq!(record.requested_model, "fast");
    assert_eq!(
        (record.provider.as_str(), record.model.as_str()),
        ("p", "m")
    );
    assert_eq!(record.outcome, Outcome::Succeeded);
    assert_eq!(record.failure, Failure::default());
    assert_eq!(record.usage, reported());
    assert_eq!(record.usage_source(), "provider_reported");
    assert_eq!(record.latency_ms, body["latency_ms"]);
}

// 1: unreported usage stays unknown
#[tokio::test]
async fn unreported_usage_is_recorded_as_unavailable() {
    let h = harness(replying("generated", Usage::unavailable()), DbMode::Up);

    h.post(json!({ "model": "fast", "input": "Hello" })).await;

    let record = &h.records.records()[0];
    assert_eq!(record.usage, Usage::unavailable());
    assert_eq!(record.usage_source(), "unavailable");
}

// 1: provider error
#[tokio::test]
async fn provider_error_writes_a_failed_record_with_its_code() {
    let error = ProviderError::Status {
        status: 429,
        code: Some("rate_limit_exceeded".to_owned()),
    };
    let h = harness(ProviderMode::Error(error), DbMode::Up);

    let response = h.post(json!({ "model": "p/m", "input": "Hello" })).await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    let records = h.records.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.requested_model, "p/m");
    assert_eq!(record.outcome, Outcome::Failed);
    assert_eq!(record.failure.kind, Some("status"));
    assert_eq!(record.failure.code.as_deref(), Some("rate_limit_exceeded"));
    assert_eq!(record.failure.provider_status, Some(429));
    assert_eq!(record.usage, Usage::unavailable());
}

// 1: timeout
#[tokio::test(start_paused = true)]
async fn provider_timeout_writes_a_timed_out_record() {
    let h = harness(ProviderMode::Hang, DbMode::Up);

    let response = tokio::time::timeout(
        PROVIDER_TIMEOUT * 2,
        h.post(json!({ "model": "fast", "input": "Hello" })),
    )
    .await
    .expect("the provider deadline must end the request");

    assert_eq!(response.status, StatusCode::GATEWAY_TIMEOUT);
    let records = h.records.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, Outcome::TimedOut);
    assert_eq!(records[0].failure, Failure::default());
    assert!(records[0].latency_ms >= PROVIDER_TIMEOUT.as_millis() as u64);
}

// 1: rejected requests did no work, so they get no record
#[tokio::test]
async fn rejected_requests_write_no_record() {
    let h = harness(replying("generated", reported()), DbMode::Up);
    let (unknown_key, _) = FakeKeyStore::new(DbMode::Up).add_key();

    let unknown_model = h.post(json!({ "model": "nope", "input": "Hello" })).await;
    let invalid_body = h.post(json!({ "model": "fast" })).await;
    let bad_key = h
        .post_with(&unknown_key, json!({ "model": "fast", "input": "Hello" }))
        .await;

    assert_eq!(unknown_model.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid_body.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_key.status, StatusCode::UNAUTHORIZED);
    assert!(h.records.records().is_empty());
}

// 2: accounting down after a paid-for answer
#[tokio::test]
async fn accounting_failure_still_delivers_the_answer_marked_failed() {
    let h = harness(replying("generated", reported()), DbMode::Down);

    let response = h.post(json!({ "model": "fast", "input": "Hello" })).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.json()["output"], "generated");
    assert_eq!(response.header(ACCOUNTING), Some("failed"));
    assert!(!response.body.contains(SENSITIVE_ERROR));
    let id = response.header("x-request-id").unwrap();
    let logs = CapturedLogs::global().text();
    assert!(
        logs.lines()
            .any(|line| line.contains(id) && line.contains("accounting_write_failed")),
        "accounting failure not logged for {id}:\n{logs}"
    );
}

// 2: accounting hangs: the answer waits at most the accounting deadline
#[tokio::test(start_paused = true)]
async fn hanging_accounting_delays_the_answer_by_at_most_its_deadline() {
    let h = harness(replying("generated", reported()), DbMode::Hang);
    let started = tokio::time::Instant::now();

    let response = tokio::time::timeout(
        ACCOUNTING_TIMEOUT * 2,
        h.post(json!({ "model": "fast", "input": "Hello" })),
    )
    .await
    .expect("the accounting deadline must release the response");

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header(ACCOUNTING), Some("failed"));
    assert!(started.elapsed() >= ACCOUNTING_TIMEOUT);
}

// 2: failures are marked too, so a client knows the failure went unrecorded
#[tokio::test]
async fn accounting_failure_marks_error_responses_too() {
    let h = harness(ProviderMode::Fail("boom".to_owned()), DbMode::Down);

    let response = h.post(json!({ "model": "fast", "input": "Hello" })).await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    assert_eq!(response.json()["error"]["code"], "provider_error");
    assert_eq!(response.header(ACCOUNTING), Some("failed"));
}

// 3: never the prompt or the output
#[tokio::test]
async fn records_never_hold_prompt_or_output() {
    let h = harness(replying("OUTPUT-CANARY-51d2", reported()), DbMode::Up);

    let response = h
        .post(json!({ "model": "fast", "input": "PROMPT-CANARY-0b7e" }))
        .await;

    assert_eq!(response.status, StatusCode::OK);
    let stored = format!("{:?}", h.records.records());
    assert!(stored.contains("fast"), "canary: the record was captured");
    for secret in ["PROMPT-CANARY-0b7e", "OUTPUT-CANARY-51d2"] {
        assert!(!stored.contains(secret), "{secret} reached the record");
    }
}
