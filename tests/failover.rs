//! Retries and fallback through `POST /v1/generate` (ADR-028): what the
//! client sees and what is recorded. The executor's own rules are tested
//! in `tests/executor.rs`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use common::{
    CapturedLogs, DbMode, FakeDatabase, FakeKeyStore, FakeProvider, FakeRecordStore, ProviderMode,
    TestResponse, send, state_with_records,
};
use modelswitch::accounting::Outcome;
use modelswitch::build_router;
use modelswitch::domain::{Generation, ProviderError, Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;

const ATTEMPTS: &str = "x-modelswitch-attempts";

/// Alias `fast` lists `first/m` then `second/m`.
struct Harness {
    app: Router,
    key: String,
    first: FakeProvider,
    second: FakeProvider,
    records: FakeRecordStore,
}

fn harness(first: FakeProvider, second: FakeProvider) -> Harness {
    CapturedLogs::global();
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let records = FakeRecordStore::new(DbMode::Up);
    let mut state = state_with_records(FakeDatabase::new(DbMode::Up), keys, records.clone());
    let target = |value: &str| Target::parse(value).unwrap();
    state.catalog = Arc::new(Catalog::new(
        [
            ("first".to_owned(), vec!["m".to_owned()]),
            ("second".to_owned(), vec!["m".to_owned()]),
        ],
        [(
            "fast".to_owned(),
            vec![target("first/m"), target("second/m")],
        )],
    ));
    let slot = |provider: &FakeProvider| ProviderSlot {
        provider: Arc::new(provider.clone()),
        timeout: Duration::from_secs(5),
    };
    state.providers = Arc::new(Registry::new([
        ("first".to_owned(), slot(&first)),
        ("second".to_owned(), slot(&second)),
    ]));
    Harness {
        app: build_router(state),
        key,
        first,
        second,
        records,
    }
}

impl Harness {
    async fn post(&self, body: Value) -> TestResponse {
        send(
            &self.app,
            Request::post("/v1/generate")
                .header("authorization", format!("Bearer {}", self.key))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    fn calls(&self) -> (usize, usize) {
        (self.first.calls().len(), self.second.calls().len())
    }
}

fn answer(output: &str) -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: output.to_owned(),
        usage: Usage {
            input_tokens: Some(4),
            output_tokens: Some(2),
            total_tokens: Some(6),
        },
    })
}

fn status(status: u16) -> ProviderMode {
    ProviderMode::Error(ProviderError::Status {
        status,
        code: Some("provider_code_canary".to_owned()),
    })
}

fn provider(mode: ProviderMode) -> FakeProvider {
    FakeProvider::new(mode)
}

#[tokio::test]
async fn one_attempt_is_reported_on_a_plain_success() {
    let h = harness(provider(answer("from first")), provider(answer("second")));

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header(ATTEMPTS), Some("1"));
    assert_eq!(h.calls(), (1, 0));
}

#[tokio::test]
async fn fallback_answers_and_reports_who_ran() {
    let h = harness(provider(status(429)), provider(answer("from second")));

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let body = response.json();
    assert_eq!(body["output"], "from second");
    assert_eq!(body["provider"], "second");
    assert_eq!(body["usage"]["total_tokens"], 6);
    assert_eq!(response.header(ATTEMPTS), Some("2"));

    let records = h.records.records();
    assert_eq!(records.len(), 1, "one record per request, not per attempt");
    assert_eq!(records[0].provider, "second");
    assert_eq!(records[0].outcome, Outcome::Succeeded);
    assert_eq!(records[0].usage.total_tokens, Some(6));
}

#[tokio::test(start_paused = true)]
async fn a_transient_failure_is_retried_invisibly() {
    let h = harness(
        FakeProvider::scripted([status(503)], answer("from first")),
        provider(answer("from second")),
    );

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["provider"], "first");
    assert_eq!(response.header(ATTEMPTS), Some("2"));
    assert_eq!(h.calls(), (2, 0));
}

#[tokio::test(start_paused = true)]
async fn when_all_fail_the_client_gets_the_last_error_without_details() {
    let h = harness(provider(status(429)), provider(ProviderMode::Hang));

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(
        response.status,
        StatusCode::GATEWAY_TIMEOUT,
        "the last failure"
    );
    assert_eq!(response.json()["error"]["code"], "upstream_timeout");
    assert_eq!(response.header(ATTEMPTS), Some("2"));
    for detail in ["first", "second", "provider_code_canary"] {
        assert!(
            !response.body.contains(detail),
            "{detail} reached the client"
        );
    }

    let records = h.records.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].provider, "second");
    assert_eq!(records[0].outcome, Outcome::TimedOut);
}

#[tokio::test(start_paused = true)]
async fn every_failed_attempt_is_logged() {
    let h = harness(provider(status(429)), provider(status(404)));

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    let id = response.header("x-request-id").unwrap();
    let logs = CapturedLogs::global().text();
    for target in ["first/m", "second/m"] {
        assert!(
            logs.lines().any(|line| line.contains(id)
                && line.contains(target)
                && line.contains("provider failed")),
            "{target} not logged for {id}:\n{logs}"
        );
    }
}

#[tokio::test]
async fn fallback_false_stays_on_the_first_candidate() {
    let h = harness(provider(status(429)), provider(answer("from second")));

    let response = h
        .post(json!({ "model": "fast", "input": "Hi", "routing": { "fallback": false } }))
        .await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    assert_eq!(h.calls(), (1, 0));
}

#[tokio::test]
async fn a_pin_never_falls_back() {
    let h = harness(provider(status(429)), provider(answer("from second")));

    let response = h.post(json!({ "model": "first/m", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    assert_eq!(h.calls(), (1, 0));
}

#[tokio::test]
async fn a_blocked_prompt_reaches_no_other_provider() {
    let blocked = ProviderMode::Error(ProviderError::Blocked {
        reason: Some("SAFETY".to_owned()),
    });
    let h = harness(provider(blocked), provider(answer("from second")));

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    assert_eq!(response.header(ATTEMPTS), Some("1"));
    assert_eq!(h.calls(), (1, 0));
    assert_eq!(
        h.records.records()[0].failure.code.as_deref(),
        Some("SAFETY")
    );
}
