//! Per-key concurrency limits through the HTTP stack (Stage 3 slice 3b,
//! ADR-032). The seat counting itself is unit-tested in `concurrency`.
//!
//! 1. Seats full: an immediate 429 `concurrency_limited` with no
//!    `Retry-After`, no provider call, no record.
//! 2. A seat comes back however the request ends: success, failure,
//!    an invalid request, or the client going away.
//! 3. One seat per client request: retries and fallback share it.
//! 4. Checked after the rate limit: a refused request still arrived, so
//!    it spends a token.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use common::{
    DbMode, FakeDatabase, FakeKeyStore, FakeProvider, FakeRecordStore, ProviderMode, TestResponse,
    get, send, state_with_records,
};
use modelswitch::concurrency::ConcurrencyLimiter;
use modelswitch::domain::{Generation, ProviderError, Target, Usage};
use modelswitch::metrics::Metrics;
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::rate_limit::{RateLimit, TokenBucketLimiter};
use modelswitch::routing::Catalog;
use modelswitch::{build_metrics_router, build_router};

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

/// Alias `fast` lists `first/m` then `second/m`.
struct Harness {
    app: Router,
    key: String,
    other_key: String,
    first: FakeProvider,
    records: FakeRecordStore,
    metrics: Arc<Metrics>,
}

fn harness(seats: u32, first: FakeProvider) -> Harness {
    harness_with(seats, 1_000, first)
}

fn harness_with(seats: u32, burst: u32, first: FakeProvider) -> Harness {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let (other_key, _) = keys.add_key();
    let records = FakeRecordStore::new(DbMode::Up);
    let mut state = state_with_records(FakeDatabase::new(DbMode::Up), keys, records.clone());
    state.concurrency = Arc::new(ConcurrencyLimiter::new(seats));
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
        timeout: PROVIDER_TIMEOUT,
    };
    state.providers = Arc::new(Registry::new([
        ("first".to_owned(), slot(first.clone())),
        ("second".to_owned(), slot(working())),
    ]));
    let metrics = Arc::clone(&state.metrics);
    Harness {
        app: build_router(state),
        key,
        other_key,
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

fn hanging() -> FakeProvider {
    FakeProvider::new(ProviderMode::Hang)
}

/// Hangs on the first call, then answers.
fn hanging_once() -> FakeProvider {
    FakeProvider::scripted([ProviderMode::Hang], answer())
}

fn status(status: u16) -> ProviderMode {
    ProviderMode::Error(ProviderError::Status { status, code: None })
}

fn hello() -> Value {
    json!({ "model": "fast", "input": "Hi" })
}

fn request(key: &str, body: &Value) -> Request<Body> {
    Request::post("/v1/generate")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

impl Harness {
    async fn post_with(&self, key: &str, body: Value) -> TestResponse {
        send(&self.app, request(key, &body)).await
    }

    async fn post(&self, body: Value) -> TestResponse {
        self.post_with(&self.key, body).await
    }

    /// Starts a request in the background and returns once it has reached
    /// the first provider, so it is holding its seat.
    async fn start_request(&self) -> JoinHandle<TestResponse> {
        let calls = self.first.calls().len();
        let app = self.app.clone();
        let request = request(&self.key, &hello());
        let handle = tokio::spawn(async move { send(&app, request).await });
        // Capped, so a request that never arrives fails instead of hanging.
        for _ in 0..1_000 {
            if self.first.calls().len() > calls {
                return handle;
            }
            tokio::task::yield_now().await;
        }
        panic!("the request never reached the provider");
    }
}

#[tokio::test(start_paused = true)]
async fn seats_full_is_429_without_retry_after() {
    let h = harness(1, hanging());
    let _running = h.start_request().await;

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    let body = response.json();
    assert_eq!(body["error"]["code"], "concurrency_limited");
    assert_eq!(body["request_id"], response.header("x-request-id").unwrap());
    assert_eq!(
        response.header("retry-after"),
        None,
        "nobody knows when a seat frees up"
    );
}

#[tokio::test(start_paused = true)]
async fn a_key_can_run_exactly_its_seats() {
    let h = harness(2, hanging());
    let _one = h.start_request().await;
    let _two = h.start_request().await;

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(start_paused = true)]
async fn a_refused_request_reaches_no_provider_and_writes_no_record() {
    let h = harness(1, hanging());
    let _running = h.start_request().await;

    h.post(hello()).await;

    assert_eq!(h.first.calls().len(), 1, "only the running request");
    assert!(h.records.records().is_empty(), "nothing has finished yet");
}

#[tokio::test(start_paused = true)]
async fn a_refusal_does_not_wait_for_a_seat() {
    let h = harness(1, hanging());
    let _running = h.start_request().await;
    let started = tokio::time::Instant::now();

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_finished_request_frees_its_seat() {
    let h = harness(1, hanging_once());
    let running = h.start_request().await;
    assert_eq!(h.post(hello()).await.status, StatusCode::TOO_MANY_REQUESTS);

    // Past the provider's deadline, so it falls back and finishes.
    tokio::time::advance(PROVIDER_TIMEOUT).await;
    assert_eq!(running.await.unwrap().status, StatusCode::OK);

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_failed_request_frees_its_seat() {
    // Other 4xx statuses stop the request without trying anyone else.
    let h = harness(1, FakeProvider::scripted([status(400)], answer()));

    assert_eq!(h.post(hello()).await.status, StatusCode::BAD_GATEWAY);

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn an_invalid_request_frees_its_seat() {
    let h = harness(1, working());

    let invalid = h.post(json!({ "model": "fast" })).await;

    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_client_going_away_frees_its_seat() {
    let h = harness(1, hanging_once());
    let running = h.start_request().await;
    assert_eq!(h.post(hello()).await.status, StatusCode::TOO_MANY_REQUESTS);

    // Hyper drops the request's future when the client disconnects.
    running.abort();
    assert!(running.await.is_err_and(|error| error.is_cancelled()));

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn keys_have_separate_seats() {
    let h = harness(1, hanging_once());
    let _running = h.start_request().await;

    let limited = h.post(hello()).await;
    let other = h.post_with(&h.other_key, hello()).await;

    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(other.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn retries_and_fallback_share_one_seat() {
    // Two attempts on `first`, then `second` answers: three provider
    // calls for one client request.
    let h = harness(1, FakeProvider::new(status(503)));

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("x-modelswitch-attempts"), Some("3"));
}

#[tokio::test(start_paused = true)]
async fn a_refused_request_still_spends_a_rate_limit_token() {
    let h = harness_with(1, 2, hanging());
    let _running = h.start_request().await;

    let refused = h.post(hello()).await;
    let next = h.post(hello()).await;

    assert_eq!(refused.json()["error"]["code"], "concurrency_limited");
    assert_eq!(next.json()["error"]["code"], "rate_limited");
}

#[tokio::test(start_paused = true)]
async fn health_is_never_limited() {
    let h = harness(1, hanging());
    let _running = h.start_request().await;

    assert_eq!(get(&h.app, "/health/live").await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn refused_requests_are_counted_apart_from_rate_limits() {
    let h = harness(1, hanging());
    let _running = h.start_request().await;
    h.post(hello()).await;
    h.post(hello()).await;

    let page = get(&build_metrics_router(h.metrics.clone()), "/metrics")
        .await
        .body;

    for line in ["concurrency_limited_total 2", "rate_limited_total 0"] {
        assert!(page.lines().any(|l| l == line), "missing {line}:\n{page}");
    }
}
