//! Metrics (Stage 2, ADR-030): what is counted, and where the page is
//! and is not reachable.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use common::{
    DbMode, FakeDatabase, FakeKeyStore, FakeProvider, FakeRecordStore, ProviderMode, TestResponse,
    get, send, state_with_records,
};
use modelswitch::domain::{Generation, ProviderError, Target, Usage};
use modelswitch::metrics::Metrics;
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use modelswitch::{build_metrics_router, build_router};

/// Alias `fast` lists `first/m` then `second/m`.
struct Harness {
    app: Router,
    metrics_app: Router,
    key: String,
    key_id: String,
}

fn harness(first: ProviderMode, second: ProviderMode, records: DbMode) -> Harness {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, key_id) = keys.add_key();
    let mut state = state_with_records(
        FakeDatabase::new(DbMode::Up),
        keys,
        FakeRecordStore::new(records),
    );
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
    let slot = |mode: ProviderMode| ProviderSlot {
        provider: Arc::new(FakeProvider::new(mode)),
        timeout: Duration::from_secs(5),
    };
    state.providers = Arc::new(Registry::new([
        ("first".to_owned(), slot(first)),
        ("second".to_owned(), slot(second)),
    ]));
    let metrics: Arc<Metrics> = Arc::clone(&state.metrics);
    Harness {
        app: build_router(state),
        metrics_app: build_metrics_router(metrics),
        key,
        key_id: key_id.to_string(),
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

    async fn page(&self) -> String {
        let response = get(&self.metrics_app, "/metrics").await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        response.body
    }
}

fn answer() -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: "OUTPUT-CANARY".to_owned(),
        usage: Usage::unavailable(),
    })
}

fn status(status: u16) -> ProviderMode {
    ProviderMode::Error(ProviderError::Status { status, code: None })
}

fn assert_line(page: &str, line: &str) {
    assert!(page.lines().any(|l| l == line), "missing {line}:\n{page}");
}

#[tokio::test]
async fn counts_the_request_and_every_attempt() {
    let h = harness(status(429), answer(), DbMode::Up);

    h.post(json!({ "model": "fast", "input": "Hi" })).await;

    let page = h.page().await;
    assert_line(&page, "requests_total{outcome=\"succeeded\"} 1");
    assert_line(
        &page,
        "provider_attempts_total{provider=\"first\",outcome=\"failed\"} 1",
    );
    assert_line(
        &page,
        "provider_attempts_total{provider=\"second\",outcome=\"succeeded\"} 1",
    );
    assert_line(&page, "accounting_failures_total 0");
}

#[tokio::test]
async fn a_failed_record_write_is_counted() {
    let h = harness(answer(), answer(), DbMode::Down);

    let response = h.post(json!({ "model": "fast", "input": "Hi" })).await;

    assert_eq!(
        response.status,
        StatusCode::OK,
        "the answer is still delivered"
    );
    let page = h.page().await;
    assert_line(&page, "accounting_failures_total 1");
    assert_line(&page, "requests_total{outcome=\"succeeded\"} 1");
}

#[tokio::test]
async fn rejected_requests_are_not_counted() {
    let h = harness(answer(), answer(), DbMode::Up);

    h.post(json!({ "model": "nope", "input": "Hi" })).await;

    assert!(!h.page().await.contains("requests_total{"));
}

#[tokio::test]
async fn the_page_holds_nothing_a_client_sent() {
    let h = harness(answer(), answer(), DbMode::Up);

    h.post(json!({ "model": "fast", "input": "PROMPT-CANARY" }))
        .await;

    let page = h.page().await;
    assert!(
        page.contains("provider=\"first\""),
        "canary: it was counted"
    );
    for secret in ["PROMPT-CANARY", "OUTPUT-CANARY", "fast", h.key_id.as_str()] {
        assert!(!page.contains(secret), "{secret} reached the metrics page");
    }
}

#[tokio::test]
async fn the_public_router_does_not_serve_metrics() {
    let h = harness(answer(), answer(), DbMode::Up);

    let response = get(&h.app, "/metrics").await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert!(!response.body.contains("accounting_failures_total"));
}

#[tokio::test]
async fn the_page_uses_the_prometheus_content_type_and_needs_no_key() {
    let h = harness(answer(), answer(), DbMode::Up);

    let response = get(&h.metrics_app, "/metrics").await;

    assert_eq!(response.status, StatusCode::OK);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/plain; version=0.0.4")
    );
}
