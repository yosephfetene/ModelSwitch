//! Per-key rate limits through the HTTP stack (Stage 3 slice 3a,
//! ADR-031). The token bucket itself is unit-tested in `rate_limit`.
//!
//! 1. Over the limit: 429 `rate_limited` with `Retry-After`, no provider
//!    call, no record.
//! 2. Checked right after authentication: bad keys get 401 and spend
//!    nothing; bad requests from a valid key do spend a token.
//! 3. One token per client request: retries and fallback are free.

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
use modelswitch::rate_limit::{RateLimit, TokenBucketLimiter};
use modelswitch::routing::Catalog;
use modelswitch::{build_metrics_router, build_router};

/// Alias `fast` lists `first/m` then `second/m`; 60 per minute is one
/// token per second.
struct Harness {
    app: Router,
    key: String,
    other_key: String,
    keys: FakeKeyStore,
    first: FakeProvider,
    records: FakeRecordStore,
    metrics: Arc<Metrics>,
}

fn harness(burst: u32, first: FakeProvider) -> Harness {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let (other_key, _) = keys.add_key();
    let records = FakeRecordStore::new(DbMode::Up);
    let mut state =
        state_with_records(FakeDatabase::new(DbMode::Up), keys.clone(), records.clone());
    state.rate_limiter = Arc::new(TokenBucketLimiter::new(RateLimit {
        requests_per_minute: 60,
        burst,
    }));
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
    let slot = |provider: FakeProvider| ProviderSlot {
        provider: Arc::new(provider),
        timeout: Duration::from_secs(5),
    };
    state.providers = Arc::new(Registry::new([
        ("first".to_owned(), slot(first.clone())),
        ("second".to_owned(), slot(FakeProvider::new(answer()))),
    ]));
    let metrics = Arc::clone(&state.metrics);
    Harness {
        app: build_router(state),
        key,
        other_key,
        keys,
        first,
        records,
        metrics,
    }
}

fn answer() -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: "generated".to_owned(),
        usage: Usage::unavailable(),
    })
}

fn working() -> FakeProvider {
    FakeProvider::new(answer())
}

fn status(status: u16) -> ProviderMode {
    ProviderMode::Error(ProviderError::Status { status, code: None })
}

fn hello() -> Value {
    json!({ "model": "fast", "input": "Hi" })
}

impl Harness {
    async fn post_with(&self, key: &str, body: Value) -> TestResponse {
        send(
            &self.app,
            Request::post("/v1/generate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn post(&self, body: Value) -> TestResponse {
        self.post_with(&self.key, body).await
    }
}

#[tokio::test(start_paused = true)]
async fn over_the_limit_is_429_with_retry_after() {
    let h = harness(2, working());

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    let body = response.json();
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["request_id"], response.header("x-request-id").unwrap());
    assert_eq!(response.header("retry-after"), Some("1"));
}

#[tokio::test(start_paused = true)]
async fn a_limited_request_reaches_no_provider_and_writes_no_record() {
    let h = harness(1, working());
    h.post(hello()).await;

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h.first.calls().len(), 1, "only the allowed request");
    assert_eq!(h.records.records().len(), 1, "only the allowed request");
}

#[tokio::test(start_paused = true)]
async fn waiting_retry_after_is_enough() {
    let h = harness(1, working());
    h.post(hello()).await;
    let limited = h.post(hello()).await;
    let wait: u64 = limited.header("retry-after").unwrap().parse().unwrap();

    tokio::time::advance(Duration::from_secs(wait)).await;

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn keys_are_limited_separately() {
    let h = harness(1, working());
    h.post(hello()).await;

    let limited = h.post(hello()).await;
    let other = h.post_with(&h.other_key, hello()).await;

    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(other.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_bad_key_gets_401_and_spends_nothing() {
    let h = harness(1, working());
    let revoked = h.other_key.clone();
    h.keys.revoke(&revoked);

    for _ in 0..3 {
        let response = h.post_with(&revoked, hello()).await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn bad_requests_from_a_valid_key_spend_tokens() {
    let h = harness(1, working());

    let invalid = h.post(json!({ "model": "fast" })).await;
    let next = h.post(hello()).await;

    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(next.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(start_paused = true)]
async fn retries_and_fallback_are_free() {
    // Two attempts on `first`, then `second` answers: three provider
    // calls for one client request.
    let h = harness(2, FakeProvider::new(status(503)));

    let response = h.post(hello()).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("x-modelswitch-attempts"), Some("3"));

    assert_eq!(
        h.post(hello()).await.status,
        StatusCode::OK,
        "the first request spent one token, not three"
    );
}

#[tokio::test(start_paused = true)]
async fn health_is_never_limited() {
    let h = harness(1, working());
    h.post(hello()).await;
    assert_eq!(h.post(hello()).await.status, StatusCode::TOO_MANY_REQUESTS);

    assert_eq!(get(&h.app, "/health/live").await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn refused_requests_are_counted() {
    let h = harness(1, working());
    h.post(hello()).await;
    h.post(hello()).await;
    h.post(hello()).await;

    let page = get(&build_metrics_router(h.metrics.clone()), "/metrics")
        .await
        .body;

    assert!(page.lines().any(|l| l == "rate_limited_total 2"), "{page}");
    assert!(
        page.lines()
            .any(|l| l == "requests_total{outcome=\"succeeded\"} 1"),
        "{page}"
    );
}
