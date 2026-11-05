//! Per-key monthly budgets (Stage 3 slice 3e, ADR-035).
//!
//! 1. Over budget: 429 `budget_exceeded` with `Retry-After`, no provider
//!    call, no record.
//! 2. A budgeted key cannot use an unpriced candidate: 422
//!    `model_not_priced`. Keys without a budget are unaffected.
//! 3. Checked after request validation and before any provider call. If
//!    it cannot be checked, the request is refused with 503 (fail closed).
//! 4. Spending is this month's `estimated_cost`, in UTC, for this key.
//! 5. A budget is firm, not exact: the request that crosses it completes.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use common::{
    ACCOUNTING_TIMEOUT, BudgetMode, DB_CHECK_TIMEOUT, DbMode, FakeBudgetStore, FakeDatabase,
    FakeKeyStore, FakeProvider, FakeRecordStore, REQUEST_LIMITS, SENSITIVE_ERROR, TestResponse,
    get, send, state_with_records,
};
use modelswitch::accounting::{Failure, Outcome, RecordStore, RequestRecord};
use modelswitch::budget::{BudgetDecision, BudgetStore};
use modelswitch::cli::keys;
use modelswitch::cli::pricing::{self, SetPrice};
use modelswitch::domain::{Target, Usage};
use modelswitch::metrics::Metrics;
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::budgets::PgBudgetStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::{AppState, build_metrics_router, build_router};

fn target(value: &str) -> Target {
    Target::parse(value).unwrap()
}

/// `fast` lists `first/m` then `second/m`.
fn catalog() -> Catalog {
    Catalog::new(
        [
            ("first".to_owned(), vec!["m".to_owned()]),
            ("second".to_owned(), vec!["m".to_owned()]),
        ],
        [(
            "fast".to_owned(),
            vec![target("first/m"), target("second/m")],
        )],
    )
}

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
    }
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

fn providers(first: &FakeProvider) -> Registry {
    let slot = |provider: FakeProvider| ProviderSlot {
        provider: Arc::new(provider),
        timeout: Duration::from_secs(5),
    };
    Registry::new([
        ("first".to_owned(), slot(first.clone())),
        ("second".to_owned(), slot(first.clone())),
    ])
}

struct Harness {
    app: Router,
    key: String,
    key_id: Uuid,
    budgets: FakeBudgetStore,
    provider: FakeProvider,
    records: FakeRecordStore,
    metrics: Arc<Metrics>,
}

fn harness(mode: BudgetMode) -> Harness {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, key_id) = keys.add_key();
    let records = FakeRecordStore::new(DbMode::Up);
    let budgets = FakeBudgetStore::new(mode);
    let provider = FakeProvider::replying("generated", usage(1, 1));
    let mut state = state_with_records(FakeDatabase::new(DbMode::Up), keys, records.clone());
    state.budgets = Arc::new(budgets.clone());
    state.catalog = Arc::new(catalog());
    state.providers = Arc::new(providers(&provider));
    let metrics = Arc::clone(&state.metrics);
    Harness {
        app: build_router(state),
        key,
        key_id,
        budgets,
        provider,
        records,
        metrics,
    }
}

impl Harness {
    async fn post(&self, body: Value) -> TestResponse {
        send(&self.app, request(&self.key, &body)).await
    }

    fn assert_no_work_done(&self) {
        assert!(self.provider.calls().is_empty(), "a provider was called");
        assert!(self.records.records().is_empty(), "a record was written");
    }
}

fn exceeded() -> BudgetMode {
    BudgetMode::Decide(BudgetDecision::Exceeded {
        retry_after_secs: 86_400,
    })
}

// 1

#[tokio::test]
async fn over_budget_is_429_with_retry_after() {
    let h = harness(exceeded());

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    let body = response.json();
    assert_eq!(body["error"]["code"], "budget_exceeded");
    assert_eq!(body["request_id"], response.header("x-request-id").unwrap());
    assert_eq!(response.header("retry-after"), Some("86400"));
    h.assert_no_work_done();
}

#[tokio::test]
async fn refusals_are_counted() {
    let h = harness(exceeded());
    h.post(hello()).await;
    h.post(hello()).await;

    let page = get(&build_metrics_router(h.metrics.clone()), "/metrics")
        .await
        .body;

    assert!(
        page.lines().any(|l| l == "budget_exceeded_total 2"),
        "{page}"
    );
}

// 2

#[tokio::test]
async fn an_unpriced_model_is_422_for_a_budgeted_key() {
    let h = harness(BudgetMode::Decide(BudgetDecision::Unpriced));

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.json()["error"]["code"], "model_not_priced");
    assert_eq!(response.header("retry-after"), None);
    h.assert_no_work_done();
}

// 3

#[tokio::test]
async fn an_allowed_request_is_served() {
    let h = harness(BudgetMode::Decide(BudgetDecision::Allowed));

    assert_eq!(h.post(hello()).await.status, StatusCode::OK);
    assert_eq!(h.provider.calls().len(), 1);
}

#[tokio::test]
async fn the_check_covers_every_candidate_the_request_may_use() {
    let h = harness(BudgetMode::Decide(BudgetDecision::Allowed));

    h.post(hello()).await;
    h.post(json!({ "model": "fast", "input": "Hi", "routing": { "fallback": false } }))
        .await;

    assert_eq!(
        h.budgets.calls(),
        [
            (h.key_id, vec![target("first/m"), target("second/m")]),
            (h.key_id, vec![target("first/m")]),
        ]
    );
}

#[tokio::test]
async fn invalid_requests_never_reach_the_check() {
    let h = harness(exceeded());

    let invalid = h.post(json!({ "model": "fast" })).await;
    let unknown = h.post(json!({ "model": "nope", "input": "Hi" })).await;

    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown.json()["error"]["code"], "unknown_model");
    assert!(h.budgets.calls().is_empty());
}

#[tokio::test]
async fn a_failed_check_refuses_the_request() {
    let h = harness(BudgetMode::Down);

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !response.body.contains(SENSITIVE_ERROR),
        "{}",
        response.body
    );
    h.assert_no_work_done();
}

#[tokio::test(start_paused = true)]
async fn a_hanging_check_refuses_the_request_within_its_deadline() {
    let h = harness(BudgetMode::Hang);
    let started = tokio::time::Instant::now();

    let response = h.post(hello()).await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(started.elapsed(), ACCOUNTING_TIMEOUT);
    h.assert_no_work_done();
}

// 4: against PostgreSQL

async fn create_key(pool: &PgPool, name: &str) -> (String, Uuid) {
    let mut out = Vec::new();
    keys::create(pool, name, Default::default(), &mut out)
        .await
        .unwrap();
    let key = String::from_utf8(out)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let id = sqlx::query_scalar("SELECT id FROM client_api_keys WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    (key, id)
}

async fn set_budget(pool: &PgPool, id: Uuid, budget: &str) {
    keys::set_budget(pool, id, Some(budget), &mut Vec::new())
        .await
        .unwrap();
}

async fn set_price(pool: &PgPool, provider: &str, input: &str, output: &str) {
    let price = SetPrice {
        provider,
        model: "m",
        input_per_million: input,
        output_per_million: output,
        source: "https://example.com/pricing",
    };
    pricing::set(pool, &catalog(), price, &mut Vec::new())
        .await
        .unwrap();
}

/// Records a finished request; a 1,000,000-token input at $1 per million
/// makes `tokens` read as millionths of a dollar.
async fn spend(pool: &PgPool, key_id: Uuid, tokens: u32) -> String {
    let id = format!("req_{}", Uuid::now_v7());
    let record = RequestRecord {
        request_id: id.clone(),
        client_request_id: None,
        client_api_key_id: key_id,
        requested_model: "fast".to_owned(),
        provider: "first".to_owned(),
        model: "m".to_owned(),
        outcome: Outcome::Succeeded,
        failure: Failure::default(),
        latency_ms: 0,
        usage: usage(tokens, 0),
        attempts: Vec::new(),
    };
    PgRecordStore::new(pool.clone())
        .insert(&record)
        .await
        .unwrap();
    id
}

async fn decide(pool: &PgPool, key_id: Uuid) -> BudgetDecision {
    PgBudgetStore::new(pool.clone())
        .check(key_id, &[target("first/m")])
        .await
        .unwrap()
}

fn is_exceeded(decision: BudgetDecision) -> bool {
    matches!(decision, BudgetDecision::Exceeded { .. })
}

#[sqlx::test]
async fn a_key_without_a_budget_is_always_allowed(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "free").await;

    assert_eq!(decide(&pool, key_id).await, BudgetDecision::Allowed);
}

#[sqlx::test]
async fn spending_is_compared_exactly(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_price(&pool, "first", "1", "0").await;
    set_budget(&pool, key_id, "0.000003").await;

    spend(&pool, key_id, 2).await;
    assert_eq!(
        decide(&pool, key_id).await,
        BudgetDecision::Allowed,
        "under budget"
    );

    spend(&pool, key_id, 1).await;
    assert!(
        is_exceeded(decide(&pool, key_id).await),
        "exactly at budget"
    );
}

#[sqlx::test]
async fn a_budget_of_zero_stops_the_key(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_price(&pool, "first", "1", "0").await;
    set_budget(&pool, key_id, "0").await;

    assert!(is_exceeded(decide(&pool, key_id).await));
}

#[sqlx::test]
async fn another_keys_spending_does_not_count(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "mine").await;
    let (_, other_id) = create_key(&pool, "other").await;
    set_price(&pool, "first", "1", "0").await;
    set_budget(&pool, key_id, "0.000001").await;

    spend(&pool, other_id, 5).await;

    assert_eq!(decide(&pool, key_id).await, BudgetDecision::Allowed);
}

#[sqlx::test]
async fn raising_or_clearing_a_budget_applies_at_once(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_price(&pool, "first", "1", "0").await;
    set_budget(&pool, key_id, "0.000001").await;
    spend(&pool, key_id, 1).await;
    assert!(is_exceeded(decide(&pool, key_id).await));

    set_budget(&pool, key_id, "1").await;
    assert_eq!(decide(&pool, key_id).await, BudgetDecision::Allowed);

    set_budget(&pool, key_id, "0.000001").await;
    keys::set_budget(&pool, key_id, None, &mut Vec::new())
        .await
        .unwrap();
    assert_eq!(decide(&pool, key_id).await, BudgetDecision::Allowed);
}

/// Moves a record, and the prices, to a moment given as SQL.
async fn move_record(pool: &PgPool, id: &str, moment: &str) {
    let sql = format!("UPDATE request_records SET created_at = {moment} WHERE id = $1");
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// A store whose connections use a time zone behind UTC with daylight
/// saving, so a month worked out in the session's zone is visibly wrong.
/// Set after connecting, because SQLx itself asks for UTC at startup.
fn store_far_from_utc(pool: &PgPool) -> PgBudgetStore {
    let pool = PgPoolOptions::new()
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET TIME ZONE 'America/New_York'")
                    .execute(&mut *connection)
                    .await
                    .map(|_| ())
            })
        })
        .connect_lazy_with((*pool.connect_options()).clone());
    PgBudgetStore::new(pool)
}

const MONTH_START: &str = "date_trunc('month', now(), 'UTC')";

#[sqlx::test]
async fn only_this_utc_month_counts_whatever_the_session_time_zone(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_price(&pool, "first", "1", "0").await;
    set_budget(&pool, key_id, "0.000001").await;
    let id = spend(&pool, key_id, 1).await;
    let targets = [target("first/m")];

    for store in [PgBudgetStore::new(pool.clone()), store_far_from_utc(&pool)] {
        move_record(
            &pool,
            &id,
            &format!("{MONTH_START} - interval '1 microsecond'"),
        )
        .await;
        assert_eq!(
            store.check(key_id, &targets).await.unwrap(),
            BudgetDecision::Allowed,
            "last month"
        );

        move_record(&pool, &id, MONTH_START).await;
        assert!(
            is_exceeded(store.check(key_id, &targets).await.unwrap()),
            "the first moment of this month"
        );
    }
}

#[sqlx::test]
async fn retry_after_is_the_time_until_next_month_in_utc(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_price(&pool, "first", "1", "0").await;
    set_budget(&pool, key_id, "0").await;
    let targets = [target("first/m")];
    let until_next_month: i64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM \
             ((date_trunc('month', now() AT TIME ZONE 'UTC') + interval '1 month') \
              AT TIME ZONE 'UTC') - now())::bigint",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    for store in [PgBudgetStore::new(pool.clone()), store_far_from_utc(&pool)] {
        let BudgetDecision::Exceeded { retry_after_secs } =
            store.check(key_id, &targets).await.unwrap()
        else {
            panic!("not exceeded");
        };

        let difference = retry_after_secs as i64 - until_next_month;
        assert!(difference.abs() <= 5, "off by {difference} seconds");
    }
}

#[sqlx::test]
async fn any_candidate_without_a_current_price_is_refused(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_budget(&pool, key_id, "1").await;
    set_price(&pool, "first", "1", "0").await;
    let store = PgBudgetStore::new(pool.clone());
    let both = [target("first/m"), target("second/m")];

    assert_eq!(
        store.check(key_id, &both).await.unwrap(),
        BudgetDecision::Unpriced,
        "the fallback has no price"
    );
    assert_eq!(
        store.check(key_id, &both[..1]).await.unwrap(),
        BudgetDecision::Allowed,
        "only the priced target"
    );

    set_price(&pool, "second", "0", "0").await;
    assert_eq!(
        store.check(key_id, &both).await.unwrap(),
        BudgetDecision::Allowed,
        "a free model is a priced model"
    );
}

#[sqlx::test]
async fn a_price_for_another_model_does_not_count(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_budget(&pool, key_id, "1").await;
    set_price(&pool, "first", "1", "0").await;

    let decision = PgBudgetStore::new(pool.clone())
        .check(key_id, &[target("first/other")])
        .await
        .unwrap();

    assert_eq!(decision, BudgetDecision::Unpriced);
}

#[sqlx::test]
async fn a_closed_price_is_not_a_current_price(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_budget(&pool, key_id, "1").await;
    set_price(&pool, "first", "1", "0").await;
    sqlx::query("UPDATE pricing_versions SET effective_to = now() + interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(decide(&pool, key_id).await, BudgetDecision::Unpriced);
}

#[sqlx::test]
async fn a_key_without_a_budget_may_use_unpriced_models(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;

    assert_eq!(decide(&pool, key_id).await, BudgetDecision::Allowed);
}

#[sqlx::test]
async fn a_spent_budget_is_reported_before_a_missing_price(pool: PgPool) {
    let (_, key_id) = create_key(&pool, "k").await;
    set_budget(&pool, key_id, "0").await;

    assert!(is_exceeded(decide(&pool, key_id).await));
}

// 5

fn real_state(pool: &PgPool, provider: &FakeProvider) -> AppState {
    AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: common::REAL_DATABASE_TIMEOUT,
        budgets: Arc::new(PgBudgetStore::new(pool.clone())),
        records: Arc::new(PgRecordStore::new(pool.clone())),
        accounting_timeout: common::REAL_DATABASE_TIMEOUT,
        limits: REQUEST_LIMITS,
        metrics: Default::default(),
        rate_limiter: common::generous_rate_limiter(),
        concurrency: common::generous_concurrency(),
        catalog: Arc::new(catalog()),
        providers: Arc::new(providers(provider)),
    }
}

#[sqlx::test]
async fn the_request_that_crosses_the_budget_completes_and_the_next_is_refused(pool: PgPool) {
    let (key, key_id) = create_key(&pool, "k").await;
    set_price(&pool, "first", "0.10", "0.50").await;
    set_price(&pool, "second", "0.10", "0.50").await;
    // Each request costs $0.00035.
    set_budget(&pool, key_id, "0.0005").await;
    let provider = FakeProvider::replying("generated", usage(1_000, 500));
    let app = build_router(real_state(&pool, &provider));

    let first = send(&app, request(&key, &hello())).await;
    let second = send(&app, request(&key, &hello())).await;
    let third = send(&app, request(&key, &hello())).await;

    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(second.status, StatusCode::OK, "still under when it started");
    assert_eq!(third.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(third.json()["error"]["code"], "budget_exceeded");
    assert_eq!(provider.calls().len(), 2);
    let spent: String =
        sqlx::query_scalar("SELECT trim_scale(sum(estimated_cost))::text FROM request_records")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(spent, "0.0007", "over by less than one request's cost");
}

#[sqlx::test]
async fn a_budgeted_key_on_an_unpriced_model_is_refused_end_to_end(pool: PgPool) {
    let (key, key_id) = create_key(&pool, "k").await;
    set_budget(&pool, key_id, "1").await;
    let provider = FakeProvider::replying("generated", usage(1_000, 500));
    let app = build_router(real_state(&pool, &provider));

    let response = send(&app, request(&key, &hello())).await;

    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(provider.calls().is_empty());
}
