//! Request history against a real PostgreSQL (Stage 4, ADR-036): order,
//! paging, filters, exact costs, attempts, and UTC times.

mod common;

use std::collections::HashSet;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use modelswitch::accounting::{
    AttemptRecord, Failure, Outcome, RecordStore, RequestRecord, RetryDecision,
};
use modelswitch::cli::keys;
use modelswitch::cli::pricing::{self, SetPrice};
use modelswitch::domain::Usage;
use modelswitch::history::{HistoryFilter, HistoryPage, HistoryStore};
use modelswitch::reliability::FailureClass;
use modelswitch::routing::Catalog;
use modelswitch::storage::history::PgHistoryStore;
use modelswitch::storage::request_records::PgRecordStore;

async fn create_key(pool: &PgPool, name: &str) -> (Uuid, String) {
    let mut out = Vec::new();
    keys::create(pool, name, Default::default(), &mut out)
        .await
        .unwrap();
    sqlx::query_as("SELECT id, key_prefix FROM client_api_keys WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn request_id() -> String {
    format!("req_{}", Uuid::now_v7())
}

/// A row written directly, so the test chooses its start time.
async fn insert_at(pool: &PgPool, key: Uuid, provider: &str, status: &str, at: &str) -> String {
    let id = request_id();
    insert_with_id(pool, &id, key, provider, status, at).await;
    id
}

async fn insert_with_id(
    pool: &PgPool,
    id: &str,
    key: Uuid,
    provider: &str,
    status: &str,
    at: &str,
) {
    sqlx::query(
        "INSERT INTO request_records (id, client_api_key_id, requested_model, \
             selected_provider, selected_model, status, error_kind, latency_ms, \
             usage_source, created_at) \
         VALUES ($1, $2, 'fast', $3, 'm', $4, \
                 CASE WHEN $4 = 'failed' THEN 'status' END, 10, 'unavailable', \
                 $5::timestamptz)",
    )
    .bind(id)
    .bind(key)
    .bind(provider)
    .bind(status)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

fn record(key: Uuid, usage: Usage, attempts: Vec<AttemptRecord>) -> RequestRecord {
    RequestRecord {
        request_id: request_id(),
        client_request_id: Some("client-7".to_owned()),
        client_api_key_id: key,
        requested_model: "fast".to_owned(),
        provider: "groq".to_owned(),
        model: "m".to_owned(),
        outcome: Outcome::Succeeded,
        failure: Failure::default(),
        latency_ms: 0,
        usage,
        attempts,
    }
}

fn attempt(provider: &str, outcome: Outcome, decision: RetryDecision) -> AttemptRecord {
    let failed = outcome == Outcome::Failed;
    AttemptRecord {
        provider: provider.to_owned(),
        model: "m".to_owned(),
        outcome,
        failure: if failed {
            Failure {
                kind: Some("status"),
                code: Some("API_KEY_INVALID".to_owned()),
                provider_status: Some(400),
            }
        } else {
            Failure::default()
        },
        class: failed.then_some(FailureClass::TargetProblem),
        decision,
        latency_ms: 12,
    }
}

async fn list(store: &PgHistoryStore, filter: HistoryFilter) -> HistoryPage {
    store.list(&filter).await.unwrap()
}

fn ids(page: &HistoryPage) -> Vec<String> {
    page.requests.iter().map(|r| r.id.clone()).collect()
}

#[sqlx::test]
async fn lists_newest_first_with_the_key_and_attempt_count(pool: PgPool) {
    let (key, prefix) = create_key(&pool, "laptop").await;
    let records = PgRecordStore::new(pool.clone());
    let mut written = Vec::new();
    for attempts in [0, 2, 1] {
        let tries = (0..attempts)
            .map(|_| attempt("groq", Outcome::Succeeded, RetryDecision::NotRetried))
            .collect();
        let record = record(key, Usage::unavailable(), tries);
        records.insert(&record).await.unwrap();
        written.push(record.request_id);
    }
    let store = PgHistoryStore::new(pool);

    let page = list(&store, HistoryFilter::default()).await;

    written.reverse();
    assert_eq!(ids(&page), written);
    assert_eq!(page.next_before, None);
    let counts: Vec<i64> = page.requests.iter().map(|r| r.attempt_count).collect();
    assert_eq!(counts, [1, 2, 0]);
    let first = &page.requests[0];
    assert_eq!(first.key.id, key);
    assert_eq!(first.key.name, "laptop");
    assert_eq!(first.key.prefix, prefix);
    assert_eq!(first.client_request_id.as_deref(), Some("client-7"));
    assert_eq!(first.requested_model, "fast");
    assert_eq!(
        (first.provider.as_str(), first.model.as_str()),
        ("groq", "m")
    );
    assert_eq!(first.usage_source, "unavailable");
    assert_eq!(first.estimated_cost_usd, None);
    let shape = regex_like_utc(&first.created_at);
    assert!(shape, "{}", first.created_at);
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn regex_like_utc(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 24
        && text.ends_with('Z')
        && [4, 7].iter().all(|&i| bytes[i] == b'-')
        && bytes[10] == b'T'
        && [13, 16].iter().all(|&i| bytes[i] == b':')
        && bytes[19] == b'.'
}

#[sqlx::test]
async fn pages_never_repeat_or_skip_even_when_times_are_equal(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    let mut all = Vec::new();
    all.push(insert_at(&pool, key, "groq", "succeeded", "2026-11-12T10:00:00Z").await);
    // Equal times, written out of ID order, so storage order cannot
    // stand in for the ID tiebreak.
    let tied: Vec<String> = (0..5).map(|_| request_id()).collect();
    for index in [2, 0, 4, 1, 3] {
        insert_with_id(
            &pool,
            &tied[index],
            key,
            "groq",
            "succeeded",
            "2026-11-12T11:00:00Z",
        )
        .await;
    }
    all.extend(tied);
    all.push(insert_at(&pool, key, "groq", "succeeded", "2026-11-12T12:00:00Z").await);
    let store = PgHistoryStore::new(pool);

    let mut seen = Vec::new();
    let mut before = None;
    let mut pages = 0;
    loop {
        let page = list(
            &store,
            HistoryFilter {
                before: before.clone(),
                limit: 2,
                ..HistoryFilter::default()
            },
        )
        .await;
        pages += 1;
        assert!(page.requests.len() <= 2);
        seen.extend(ids(&page));
        match page.next_before {
            Some(next) => before = Some(next),
            None => break,
        }
    }

    // Newest first; equal times by ID, which was generated in order.
    all.reverse();
    assert_eq!(seen, all);
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 7);
    assert_eq!(pages, 4);
}

#[sqlx::test]
async fn a_full_last_page_says_there_is_no_next_page(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    for hour in 10..12 {
        insert_at(
            &pool,
            key,
            "groq",
            "succeeded",
            &format!("2026-11-12T{hour}:00:00Z"),
        )
        .await;
    }
    let store = PgHistoryStore::new(pool);

    let page = list(
        &store,
        HistoryFilter {
            limit: 2,
            ..HistoryFilter::default()
        },
    )
    .await;

    assert_eq!(page.requests.len(), 2);
    assert_eq!(page.next_before, None);
}

#[sqlx::test]
async fn an_unknown_before_id_gives_an_empty_page(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    insert_at(&pool, key, "groq", "succeeded", "2026-11-12T10:00:00Z").await;
    let store = PgHistoryStore::new(pool);

    let page = list(
        &store,
        HistoryFilter {
            before: Some(request_id()),
            ..HistoryFilter::default()
        },
    )
    .await;

    assert!(page.requests.is_empty());
}

#[sqlx::test]
async fn filters_by_provider_status_and_time(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    let early = insert_at(&pool, key, "groq", "succeeded", "2026-11-12T09:59:59.999Z").await;
    let start = insert_at(&pool, key, "groq", "failed", "2026-11-12T10:00:00Z").await;
    let gemini = insert_at(&pool, key, "gemini", "timed_out", "2026-11-12T10:30:00Z").await;
    let end = insert_at(&pool, key, "groq", "succeeded", "2026-11-12T11:00:00Z").await;
    let store = PgHistoryStore::new(pool);
    let at = |text: &str| Some(text.parse().unwrap());

    let by_provider = list(
        &store,
        HistoryFilter {
            provider: Some("gemini".to_owned()),
            ..HistoryFilter::default()
        },
    )
    .await;
    assert_eq!(ids(&by_provider), std::slice::from_ref(&gemini));

    let by_status = list(
        &store,
        HistoryFilter {
            status: Some(Outcome::Failed),
            ..HistoryFilter::default()
        },
    )
    .await;
    assert_eq!(ids(&by_status), std::slice::from_ref(&start));
    assert_eq!(by_status.requests[0].error_kind.as_deref(), Some("status"));

    // `from` includes its moment; `to` does not.
    let window = list(
        &store,
        HistoryFilter {
            from: at("2026-11-12T10:00:00Z"),
            to: at("2026-11-12T11:00:00Z"),
            ..HistoryFilter::default()
        },
    )
    .await;
    assert_eq!(ids(&window), [gemini, start.clone()]);

    let combined = list(
        &store,
        HistoryFilter {
            from: at("2026-11-12T10:00:00Z"),
            provider: Some("groq".to_owned()),
            status: Some(Outcome::Succeeded),
            ..HistoryFilter::default()
        },
    )
    .await;
    assert_eq!(ids(&combined), [end]);
    assert!(!ids(&window).contains(&early));
}

fn catalog() -> Catalog {
    Catalog::new([("groq".to_owned(), vec!["m".to_owned()])], [])
}

#[sqlx::test]
async fn costs_and_prices_are_exact(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    let price = SetPrice {
        provider: "groq",
        model: "m",
        input_per_million: "0.10",
        output_per_million: "0.50",
        source: "https://example.com/pricing",
    };
    pricing::set(&pool, &catalog(), price, &mut Vec::new())
        .await
        .unwrap();
    let usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(50),
        total_tokens: Some(150),
    };
    let record = record(key, usage, Vec::new());
    PgRecordStore::new(pool.clone())
        .insert(&record)
        .await
        .unwrap();
    let version: Uuid = sqlx::query_scalar("SELECT id FROM pricing_versions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let store = PgHistoryStore::new(pool);

    let page = list(&store, HistoryFilter::default()).await;
    let detail = store.get(&record.request_id).await.unwrap().unwrap();

    // 100 × 0.10 / 1M + 50 × 0.50 / 1M, with no float rounding.
    let row = &page.requests[0];
    assert_eq!(row.estimated_cost_usd.as_deref(), Some("0.000035"));
    assert_eq!(row.pricing_version, Some(version));
    assert_eq!(
        (row.input_tokens, row.output_tokens, row.total_tokens),
        (Some(100), Some(50), Some(150))
    );
    assert_eq!(row.usage_source, "provider_reported");
    let price = detail.price.unwrap();
    assert_eq!(price.input_usd_per_million, "0.1");
    assert_eq!(price.output_usd_per_million, "0.5");
    assert_eq!(price.source_reference, "https://example.com/pricing");
}

#[sqlx::test]
async fn a_request_shows_its_attempts_in_order(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    let record = record(
        key,
        Usage::unavailable(),
        vec![
            attempt("geminibadkey", Outcome::Failed, RetryDecision::Fallback),
            attempt("groq", Outcome::Succeeded, RetryDecision::NotRetried),
        ],
    );
    PgRecordStore::new(pool.clone())
        .insert(&record)
        .await
        .unwrap();
    let store = PgHistoryStore::new(pool);

    let detail = store.get(&record.request_id).await.unwrap().unwrap();

    assert_eq!(detail.summary.id, record.request_id);
    assert_eq!(detail.summary.attempt_count, 2);
    assert_eq!(detail.price, None);
    let first = &detail.attempts[0];
    assert_eq!(first.number, 1);
    assert_eq!(first.provider, "geminibadkey");
    assert_eq!(first.status, "failed");
    assert_eq!(first.error_kind.as_deref(), Some("status"));
    assert_eq!(first.error_code.as_deref(), Some("API_KEY_INVALID"));
    assert_eq!(first.provider_status, Some(400));
    assert_eq!(first.error_class.as_deref(), Some("target_problem"));
    assert_eq!(first.retry_decision, "fallback");
    assert_eq!(first.latency_ms, 12);
    let second = &detail.attempts[1];
    assert_eq!((second.number, second.provider.as_str()), (2, "groq"));
    assert_eq!(second.error_class, None);
    assert_eq!(second.retry_decision, "not_retried");
}

#[sqlx::test]
async fn an_unknown_request_is_none(pool: PgPool) {
    let store = PgHistoryStore::new(pool);

    assert_eq!(store.get(&request_id()).await.unwrap(), None);
}

#[sqlx::test]
async fn times_are_utc_whatever_the_session_time_zone(pool: PgPool) {
    let (key, _) = create_key(&pool, "k").await;
    let id = insert_at(&pool, key, "groq", "succeeded", "2026-11-12T00:30:00Z").await;
    let new_york = PgPoolOptions::new()
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET TIME ZONE 'America/New_York'")
                    .execute(&mut *connection)
                    .await
                    .map(|_| ())
            })
        })
        .connect_lazy_with((*pool.connect_options()).clone());
    let store = PgHistoryStore::new(new_york);

    let page = list(
        &store,
        HistoryFilter {
            from: Some("2026-11-12T00:00:00Z".parse().unwrap()),
            ..HistoryFilter::default()
        },
    )
    .await;

    assert_eq!(ids(&page), [id]);
    assert_eq!(page.requests[0].created_at, "2026-11-12T00:30:00.000Z");
}
