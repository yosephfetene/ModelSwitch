//! Versioned prices and estimated cost against a real PostgreSQL (Stage 3
//! slice 3d, ADR-034). The arithmetic is exact `numeric` in the record
//! write, so this is where it is tested.
//!
//! 1. `pricing set` adds a version and closes the previous one; nothing
//!    is ever overwritten.
//! 2. Cost is reported tokens × the price in effect when the request
//!    started, and the record names that version.
//! 3. No price or incomplete usage leaves the cost null, never zero. A
//!    price of zero is a deliberate, real zero.
//! 4. A request with usage but no price is counted, so gaps are visible.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use common::{DB_CHECK_TIMEOUT, FakeProvider, REQUEST_LIMITS};
use modelswitch::accounting::{Cost, Failure, Outcome, RecordStore, RequestRecord};
use modelswitch::cli::keys;
use modelswitch::cli::pricing::{self, SetPrice};
use modelswitch::domain::{Target, Usage};
use modelswitch::metrics::Metrics;
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::{AppState, build_router};

const SOURCE: &str = "https://example.com/pricing";

fn catalog() -> Catalog {
    Catalog::new(
        [
            (
                "groq".to_owned(),
                vec!["m".to_owned(), "n/slash".to_owned()],
            ),
            ("gemini".to_owned(), vec!["m".to_owned()]),
        ],
        [],
    )
}

async fn try_set(pool: &PgPool, model: &str, input: &str, output: &str) -> anyhow::Result<String> {
    let mut out = Vec::new();
    let price = SetPrice {
        provider: "groq",
        model,
        input_per_million: input,
        output_per_million: output,
        source: SOURCE,
    };
    pricing::set(pool, &catalog(), price, &mut out).await?;
    Ok(String::from_utf8(out).unwrap())
}

async fn set(pool: &PgPool, input: &str, output: &str) -> String {
    try_set(pool, "m", input, output).await.unwrap()
}

async fn list(pool: &PgPool) -> String {
    let mut out = Vec::new();
    pricing::list(pool, &mut out).await.unwrap();
    String::from_utf8(out).unwrap()
}

async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pricing_versions")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn create_key(pool: &PgPool) -> (String, Uuid) {
    let mut out = Vec::new();
    keys::create(pool, "pricing", Default::default(), &mut out)
        .await
        .unwrap();
    let key = String::from_utf8(out)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let id = sqlx::query_scalar("SELECT id FROM client_api_keys")
        .fetch_one(pool)
        .await
        .unwrap();
    (key, id)
}

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input.saturating_add(output)),
    }
}

fn record(key_id: Uuid, usage: Usage) -> RequestRecord {
    RequestRecord {
        request_id: format!("req_{}", Uuid::now_v7()),
        client_request_id: None,
        client_api_key_id: key_id,
        requested_model: "groq/m".to_owned(),
        provider: "groq".to_owned(),
        model: "m".to_owned(),
        outcome: Outcome::Succeeded,
        failure: Failure::default(),
        // Started now, so after any price the test has just set.
        latency_ms: 0,
        usage,
        attempts: Vec::new(),
    }
}

/// Writes the record; returns what the store said and (cost, version) as
/// stored, with the cost in its shortest exact form.
async fn write(pool: &PgPool, record: &RequestRecord) -> (Cost, Option<String>, Option<Uuid>) {
    let cost = PgRecordStore::new(pool.clone())
        .insert(record)
        .await
        .unwrap();
    let (stored, version) = stored_cost(pool, &record.request_id).await;
    (cost, stored, version)
}

async fn stored_cost(pool: &PgPool, id: &str) -> (Option<String>, Option<Uuid>) {
    sqlx::query_as(
        "SELECT trim_scale(estimated_cost)::text, pricing_version \
         FROM request_records WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The versions of `groq/m`, oldest first.
async fn versions(pool: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM pricing_versions ORDER BY effective_from")
        .fetch_all(pool)
        .await
        .unwrap()
}

// 1

#[sqlx::test]
async fn set_stores_a_price_with_where_it_came_from(pool: PgPool) {
    let out = set(&pool, "0.10", "0.50").await;

    let id = versions(&pool).await[0];
    assert!(out.contains(&id.to_string()), "{out}");
    let (input, output, source, recent): (String, String, String, bool) = sqlx::query_as(
        "SELECT input_usd_per_million::text, output_usd_per_million::text, source_reference, \
                retrieved_at = effective_from AND effective_to IS NULL FROM pricing_versions",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (input.as_str(), output.as_str(), source.as_str()),
        ("0.10", "0.50", SOURCE)
    );
    assert!(recent);
}

#[sqlx::test]
async fn a_new_price_closes_the_old_one_at_the_same_moment(pool: PgPool) {
    set(&pool, "0.10", "0.50").await;

    set(&pool, "0.20", "1.00").await;

    let rows: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT input_usd_per_million::text, effective_to IS NULL, \
                effective_to IS NOT DISTINCT FROM \
                    lead(effective_from) OVER (ORDER BY effective_from) \
         FROM pricing_versions ORDER BY effective_from",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("0.10".to_owned(), false, true),
            ("0.20".to_owned(), true, true)
        ]
    );
}

#[sqlx::test]
async fn a_price_only_closes_its_own_model(pool: PgPool) {
    set(&pool, "0.10", "0.50").await;

    try_set(&pool, "n/slash", "0.20", "1.00").await.unwrap();

    let current: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pricing_versions WHERE effective_to IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current, 2);
}

#[sqlx::test]
async fn set_refuses_a_model_that_is_not_configured(pool: PgPool) {
    let error = try_set(&pool, "typo", "0.10", "0.50").await.unwrap_err();

    assert!(error.to_string().contains("groq/typo"), "{error}");
    assert_eq!(count(&pool).await, 0);
}

#[sqlx::test]
async fn set_refuses_bad_values_before_writing(pool: PgPool) {
    for (input, output) in [("NaN", "0.50"), ("0.10", "1e5"), ("", "0.50")] {
        assert!(try_set(&pool, "m", input, output).await.is_err());
    }

    assert_eq!(count(&pool).await, 0);
}

#[sqlx::test]
async fn list_shows_current_and_past_prices(pool: PgPool) {
    assert!(list(&pool).await.contains("No prices yet"));
    set(&pool, "0.10", "0.50").await;
    set(&pool, "0.20", "1.00").await;

    let out = list(&pool).await;

    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("0.10") && !lines[0].contains("current"));
    assert!(lines[1].contains("0.20") && lines[1].contains("current"));
    assert!(out.contains("groq/m") && out.contains(SOURCE), "{out}");
}

#[sqlx::test]
async fn the_schema_rejects_a_second_current_price_and_bad_numbers(pool: PgPool) {
    set(&pool, "0.10", "0.50").await;
    let insert = |provider: &'static str, input: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO pricing_versions (provider, model, input_usd_per_million, \
                     output_usd_per_million, source_reference) \
                 VALUES ($1, 'm', $2::text::numeric, 1, 'x')",
            )
            .bind(provider)
            .bind(input)
            .execute(&pool)
            .await
        }
    };

    assert!(insert("groq", "1").await.is_err(), "two current prices");
    assert!(insert("other", "-1").await.is_err(), "negative");
    assert!(insert("other", "NaN").await.is_err(), "NaN");
    assert!(insert("other", "1000000").await.is_err(), "too large");
}

#[sqlx::test]
async fn the_schema_rejects_a_cost_without_its_price(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    let row = record(key_id, Usage::unavailable());
    write(&pool, &row).await;

    let result = sqlx::query("UPDATE request_records SET estimated_cost = 1")
        .execute(&pool)
        .await;

    assert!(result.is_err(), "a cost with no pricing version was stored");
}

// 2

#[sqlx::test]
async fn cost_is_tokens_times_price_exactly(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0.10", "0.50").await;

    let (cost, stored, version) = write(&pool, &record(key_id, usage(1_000, 500))).await;

    assert_eq!(cost, Cost::Estimated);
    assert_eq!(stored.as_deref(), Some("0.00035"));
    assert_eq!(version, Some(versions(&pool).await[0]));
}

#[sqlx::test]
async fn the_smallest_and_largest_costs_stay_exact(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0.000000001", "999999.999999999").await;

    let (_, smallest, _) = write(&pool, &record(key_id, usage(1, 0))).await;
    let (_, largest, _) = write(&pool, &record(key_id, usage(0, u32::MAX))).await;

    assert_eq!(smallest.as_deref(), Some("0.000000000000001"));
    assert_eq!(largest.as_deref(), Some("4294967294.999995705032705"));
}

#[sqlx::test]
async fn the_price_used_is_the_one_in_effect_when_the_request_started(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "1", "0").await;
    sqlx::query("UPDATE pricing_versions SET effective_from = now() - interval '2 hours'")
        .execute(&pool)
        .await
        .unwrap();
    set(&pool, "2", "0").await;
    let versions = versions(&pool).await;

    let mut started_an_hour_ago = record(key_id, usage(1_000_000, 0));
    started_an_hour_ago.latency_ms = 3_600_000;
    let started_just_now = record(key_id, usage(1_000_000, 0));

    let (_, old_cost, old_version) = write(&pool, &started_an_hour_ago).await;
    let (_, new_cost, new_version) = write(&pool, &started_just_now).await;

    assert_eq!(
        (old_cost.as_deref(), old_version),
        (Some("1"), Some(versions[0]))
    );
    assert_eq!(
        (new_cost.as_deref(), new_version),
        (Some("2"), Some(versions[1]))
    );
}

#[sqlx::test]
async fn an_old_estimate_survives_a_price_change(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "1", "0").await;
    let row = record(key_id, usage(1_000_000, 0));
    let (_, before, version) = write(&pool, &row).await;

    set(&pool, "9", "0").await;

    assert_eq!(stored_cost(&pool, &row.request_id).await, (before, version));
}

#[sqlx::test]
async fn only_the_exact_provider_and_model_are_priced(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0.10", "0.50").await;

    let mut same_model_elsewhere = record(key_id, usage(1_000, 500));
    same_model_elsewhere.provider = "gemini".to_owned();
    let mut same_provider_other_model = record(key_id, usage(1_000, 500));
    same_provider_other_model.model = "n/slash".to_owned();

    for row in [same_model_elsewhere, same_provider_other_model] {
        let result = write(&pool, &row).await;

        assert_eq!(result, (Cost::Unpriced, None, None), "{}", row.model);
    }
}

#[sqlx::test]
async fn a_request_from_before_the_first_price_stays_unpriced(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0.10", "0.50").await;
    let mut started_before = record(key_id, usage(1_000, 500));
    started_before.latency_ms = 3_600_000;

    let result = write(&pool, &started_before).await;

    assert_eq!(result, (Cost::Unpriced, None, None));
}

// 3

#[sqlx::test]
async fn no_price_leaves_the_cost_unknown(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;

    let result = write(&pool, &record(key_id, usage(1_000, 500))).await;

    assert_eq!(result, (Cost::Unpriced, None, None));
}

#[sqlx::test]
async fn incomplete_usage_leaves_the_cost_unknown(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0.10", "0.50").await;
    let incomplete = [
        Usage::unavailable(),
        Usage {
            input_tokens: Some(1_000),
            ..Usage::unavailable()
        },
        Usage {
            output_tokens: Some(500),
            ..Usage::unavailable()
        },
        Usage {
            total_tokens: Some(1_500),
            ..Usage::unavailable()
        },
    ];

    for usage in incomplete {
        let result = write(&pool, &record(key_id, usage)).await;

        assert_eq!(result, (Cost::NoUsage, None, None), "{usage:?}");
    }
}

#[sqlx::test]
async fn a_failed_request_has_no_cost(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0.10", "0.50").await;
    let mut failed = record(key_id, Usage::unavailable());
    failed.outcome = Outcome::TimedOut;

    let result = write(&pool, &failed).await;

    assert_eq!(result, (Cost::NoUsage, None, None));
}

#[sqlx::test]
async fn a_price_of_zero_is_a_real_zero(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    set(&pool, "0", "0").await;

    let (cost, stored, version) = write(&pool, &record(key_id, usage(1_000, 500))).await;

    assert_eq!(cost, Cost::Estimated);
    assert_eq!(stored.as_deref(), Some("0"));
    assert!(version.is_some());
}

// 4

/// `POST /v1/generate` with a real database and a provider reporting
/// `usage`; returns the status, the stored cost, and the metrics page.
async fn generate(pool: &PgPool, usage: Usage) -> (StatusCode, Option<String>, String) {
    let (key, _) = create_key(pool).await;
    let metrics = Arc::new(Metrics::default());
    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: common::REAL_DATABASE_TIMEOUT,
        budgets: common::no_budgets(),
        records: Arc::new(PgRecordStore::new(pool.clone())),
        accounting_timeout: common::REAL_DATABASE_TIMEOUT,
        limits: REQUEST_LIMITS,
        metrics: Arc::clone(&metrics),
        rate_limiter: common::generous_rate_limiter(),
        concurrency: common::generous_concurrency(),
        catalog: Arc::new(Catalog::new(
            [("groq".to_owned(), vec!["m".to_owned()])],
            [("default".to_owned(), vec![Target::parse("groq/m").unwrap()])],
        )),
        providers: Arc::new(Registry::new([(
            "groq".to_owned(),
            ProviderSlot {
                provider: Arc::new(FakeProvider::replying("generated", usage)),
                timeout: Duration::from_secs(5),
            },
        )])),
    };
    let response = common::send(
        &build_router(state),
        Request::post("/v1/generate")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "model": "default", "input": "Hi" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        response.header("x-modelswitch-accounting"),
        None,
        "the record was not written"
    );
    let id = response.json()["id"].as_str().unwrap().to_owned();
    let (cost, _) = stored_cost(pool, &id).await;
    (response.status, cost, metrics.render())
}

fn unpriced(page: &str) -> &str {
    page.lines()
        .find(|line| line.starts_with("unpriced_requests_total "))
        .unwrap_or_else(|| panic!("no counter:\n{page}"))
}

#[sqlx::test]
async fn a_generated_answer_records_its_cost(pool: PgPool) {
    set(&pool, "0.10", "0.50").await;

    let (status, cost, page) = generate(&pool, usage(1_000, 500)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cost.as_deref(), Some("0.00035"));
    assert_eq!(unpriced(&page), "unpriced_requests_total 0");
}

#[sqlx::test]
async fn usage_without_a_price_is_counted_and_still_answered(pool: PgPool) {
    let (status, cost, page) = generate(&pool, usage(1_000, 500)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cost, None);
    assert_eq!(unpriced(&page), "unpriced_requests_total 1");
}

#[sqlx::test]
async fn no_usage_is_not_a_missing_price(pool: PgPool) {
    let (status, cost, page) = generate(&pool, Usage::unavailable()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cost, None);
    assert_eq!(unpriced(&page), "unpriced_requests_total 0");
}
