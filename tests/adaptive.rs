//! Adaptive aliases on the request path (Stage 5, ADR-039): ordered from
//! in-memory routing data, failover kept, the reason recorded, and the
//! configured order whenever data is missing. No database: routing data
//! comes from a fake source. The PostgreSQL source is tested in
//! `tests/adaptive_postgres.rs`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use common::{
    CapturedLogs, DbMode, FakeDatabase, FakeKeyStore, FakeProvider, FakeRecordStore, ProviderMode,
    TestResponse, send, state_with_records,
};
use modelswitch::bench::{Bar, CandidateSummary};
use modelswitch::build_router;
use modelswitch::domain::{BoxFuture, ProviderError, Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use modelswitch::routing::adaptive::{
    AdaptivePolicy, AdaptiveRouter, LiveStat, RoutingData, RoutingSource, RoutingSourceError,
};

fn target(value: &str) -> Target {
    Target::parse(value).unwrap()
}

/// A source that returns whatever the test last set.
#[derive(Clone, Default)]
struct FakeSource(Arc<Mutex<RoutingData>>);

impl FakeSource {
    fn set(&self, data: RoutingData) {
        *self.0.lock().unwrap() = data;
    }
}

impl RoutingSource for FakeSource {
    fn load<'a>(
        &'a self,
        _: &'a [Target],
        _: &'a str,
        _: &'a AdaptivePolicy,
    ) -> BoxFuture<'a, Result<RoutingData, RoutingSourceError>> {
        let data = self.0.lock().unwrap().clone();
        Box::pin(async move { Ok(data) })
    }
}

fn benchmarked(name: &str, passed: u32, p50: u64) -> CandidateSummary {
    CandidateSummary {
        target: target(name),
        samples: 120,
        passed,
        p50_ms: Some(p50),
        p95_ms: Some(p50 * 2),
        mean_cost_usd: None,
        unpriced: 0,
        within_ceiling: None,
    }
}

fn policy() -> AdaptivePolicy {
    AdaptivePolicy {
        bar: Bar::default(),
        max_mean_cost: None,
        window: Duration::from_secs(900),
        min_live_requests: 10,
    }
}

/// Alias `smart` (adaptive) and `plain` (configured order) both list
/// `first/a` then `second/b`.
struct Harness {
    app: Router,
    key: String,
    first: FakeProvider,
    second: FakeProvider,
    records: FakeRecordStore,
    router: Arc<AdaptiveRouter>,
    source: FakeSource,
}

fn harness_with(first: FakeProvider, second: FakeProvider) -> Harness {
    CapturedLogs::global();
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let records = FakeRecordStore::new(DbMode::Up);
    let mut state = state_with_records(FakeDatabase::new(DbMode::Up), keys, records.clone());
    let candidates = vec![target("first/a"), target("second/b")];
    state.catalog = Arc::new(Catalog::new(
        [
            ("first".to_owned(), vec!["a".to_owned()]),
            ("second".to_owned(), vec!["b".to_owned()]),
        ],
        [
            ("smart".to_owned(), candidates.clone()),
            ("plain".to_owned(), candidates.clone()),
        ],
    ));
    let slot = |provider: &FakeProvider| ProviderSlot {
        provider: Arc::new(provider.clone()),
        timeout: Duration::from_secs(5),
    };
    state.providers = Arc::new(Registry::new([
        ("first".to_owned(), slot(&first)),
        ("second".to_owned(), slot(&second)),
    ]));
    let router = Arc::new(AdaptiveRouter::new([(
        "smart".to_owned(),
        (candidates, policy()),
    )]));
    state.adaptive = Arc::clone(&router);
    Harness {
        app: build_router(state),
        key,
        first,
        second,
        records,
        router,
        source: FakeSource::default(),
    }
}

fn answering() -> Harness {
    harness_with(
        FakeProvider::replying("from first", Usage::unavailable()),
        FakeProvider::replying("from second", Usage::unavailable()),
    )
}

impl Harness {
    async fn load(&self, data: RoutingData) {
        self.source.set(data);
        self.router
            .refresh(&self.source, Duration::from_secs(1))
            .await;
    }

    async fn post(&self, body: Value) -> TestResponse {
        let request = Request::post("/v1/generate")
            .header("authorization", format!("Bearer {}", self.key))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        send(&self.app, request).await
    }

    fn last_reason(&self) -> Option<String> {
        self.records
            .records()
            .last()
            .unwrap()
            .routing_reason
            .clone()
    }
}

/// `second/b` benchmarks faster than `first/a`; both are eligible.
fn second_is_faster() -> RoutingData {
    RoutingData {
        bench: Some(vec![
            benchmarked("first/a", 120, 900),
            benchmarked("second/b", 120, 300),
        ]),
        live: vec![],
    }
}

#[tokio::test]
async fn an_adaptive_alias_tries_the_fastest_eligible_candidate_first() {
    let h = answering();
    h.load(second_is_faster()).await;

    let response = h.post(json!({ "model": "smart", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.json()["provider"], "second");
    assert_eq!(response.json()["model"], "smart");
    assert!(h.first.calls().is_empty());
    assert_eq!(
        h.last_reason().as_deref(),
        Some("second/b first: fastest of 2 eligible by benchmark p50 300 ms")
    );
}

#[tokio::test]
async fn without_fallback_only_the_adaptive_first_choice_is_tried() {
    let h = harness_with(
        FakeProvider::replying("from first", Usage::unavailable()),
        FakeProvider::new(ProviderMode::Error(ProviderError::Status {
            status: 401,
            code: None,
        })),
    );
    h.load(second_is_faster()).await;

    let response = h
        .post(json!({ "model": "smart", "input": "Hi", "routing": { "fallback": false } }))
        .await;

    assert_eq!(
        response.status,
        StatusCode::BAD_GATEWAY,
        "{}",
        response.body
    );
    assert_eq!(h.second.calls().len(), 1);
    assert!(h.first.calls().is_empty(), "no fallback");
}

#[tokio::test]
async fn failover_still_reaches_every_candidate() {
    let h = harness_with(
        FakeProvider::replying("from first", Usage::unavailable()),
        FakeProvider::new(ProviderMode::Error(ProviderError::Status {
            status: 401,
            code: None,
        })),
    );
    // first/a is below the bar, so it is tried last, but still tried.
    h.load(RoutingData {
        bench: Some(vec![
            benchmarked("first/a", 10, 100),
            benchmarked("second/b", 120, 300),
        ]),
        live: vec![],
    })
    .await;

    let response = h.post(json!({ "model": "smart", "input": "Hi" })).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.json()["provider"], "first");
    assert_eq!(response.header("x-modelswitch-attempts"), Some("2"));
    let record = h.records.records().pop().unwrap();
    assert_eq!(record.attempts[0].provider, "second");
    assert_eq!(record.attempts[1].provider, "first");
    assert_eq!(
        record.routing_reason.as_deref(),
        Some(
            "second/b first: fastest of 1 eligible by benchmark p50 300 ms; \
             not eligible: first/a (below success bar)"
        )
    );
}

#[tokio::test]
async fn without_routing_data_the_configured_order_is_used_and_said() {
    let h = answering();

    let response = h.post(json!({ "model": "smart", "input": "Hi" })).await;

    assert_eq!(response.json()["provider"], "first");
    assert_eq!(
        h.last_reason().as_deref(),
        Some("configured order: no current routing data")
    );
}

#[tokio::test]
async fn other_aliases_and_pins_are_untouched_and_record_no_reason() {
    let h = answering();
    h.load(second_is_faster()).await;

    for model in ["plain", "first/a"] {
        let response = h.post(json!({ "model": model, "input": "Hi" })).await;

        assert_eq!(response.json()["provider"], "first", "{model}");
        assert_eq!(h.last_reason(), None, "{model}");
    }
}

#[tokio::test]
async fn a_slowdown_seen_live_moves_traffic_away_and_back() {
    let h = answering();
    let bench = vec![
        benchmarked("first/a", 120, 200),
        benchmarked("second/b", 120, 400),
    ];
    h.load(RoutingData {
        bench: Some(bench.clone()),
        live: vec![],
    })
    .await;
    let provider = async || {
        let response = h.post(json!({ "model": "smart", "input": "Hi" })).await;
        response.json()["provider"].as_str().unwrap().to_owned()
    };
    assert_eq!(provider().await, "first", "fastest when benchmarked");

    // first/a slows down: its live median is now far above second's.
    let live_first = |p50| LiveStat {
        target: target("first/a"),
        attempts: 40,
        succeeded: 40,
        p50_ms: Some(p50),
    };
    h.load(RoutingData {
        bench: Some(bench.clone()),
        live: vec![live_first(2500)],
    })
    .await;
    assert_eq!(provider().await, "second");
    assert_eq!(
        h.last_reason().as_deref(),
        Some("second/b first: fastest of 2 eligible by benchmark p50 400 ms")
    );

    // It recovers.
    h.load(RoutingData {
        bench: Some(bench),
        live: vec![live_first(150)],
    })
    .await;
    assert_eq!(provider().await, "first");
    assert_eq!(
        h.last_reason().as_deref(),
        Some("first/a first: fastest of 2 eligible by live p50 150 ms over 40 requests")
    );
}
