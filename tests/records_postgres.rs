//! Request records against a real PostgreSQL (Stage 1 slice 6): the
//! migration and the insert agree, end to end through the gateway.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use common::{ACCOUNTING_TIMEOUT, AUTH_TIMEOUT, DB_CHECK_TIMEOUT, REQUEST_LIMITS, send};
use modelswitch::accounting::{Failure, Outcome, RecordStore, RequestRecord};
use modelswitch::cli::keys;
use modelswitch::domain::{Target, Usage};
use modelswitch::providers::echo::EchoProvider;
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::{AppState, build_router};

async fn create_key(pool: &PgPool) -> (String, Uuid) {
    let mut out = Vec::new();
    keys::create(pool, "records", &mut out).await.unwrap();
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

#[sqlx::test]
async fn generate_writes_a_row(pool: PgPool) {
    let (key, key_id) = create_key(&pool).await;
    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: AUTH_TIMEOUT,
        records: Arc::new(PgRecordStore::new(pool.clone())),
        accounting_timeout: ACCOUNTING_TIMEOUT,
        limits: REQUEST_LIMITS,
        catalog: Arc::new(Catalog::new(
            [("echo".to_owned(), vec!["echo-1".to_owned()])],
            [(
                "default".to_owned(),
                vec![Target::parse("echo/echo-1").unwrap()],
            )],
        )),
        providers: Arc::new(Registry::new([(
            "echo".to_owned(),
            ProviderSlot {
                provider: Arc::new(EchoProvider),
                timeout: Duration::from_secs(5),
            },
        )])),
    };

    let response = send(
        &build_router(state),
        Request::post("/v1/generate")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .header("x-client-request-id", "order-42")
            .body(Body::from(
                json!({ "model": "default", "input": "PROMPT-CANARY" }).to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.header("x-modelswitch-accounting"), None);
    let row: StoredRow = sqlx::query_as(
        "SELECT id, client_request_id, client_api_key_id, requested_model, \
                selected_provider, selected_model, status, reported_total_tokens, \
                usage_source, created_at <= completed_at AS ordered \
         FROM request_records",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.id, response.json()["id"]);
    assert_eq!(row.client_request_id.as_deref(), Some("order-42"));
    assert_eq!(row.client_api_key_id, key_id);
    assert_eq!(
        (
            row.requested_model.as_str(),
            row.selected_provider.as_str(),
            row.selected_model.as_str()
        ),
        ("default", "echo", "echo-1")
    );
    assert_eq!(row.status, "succeeded");
    assert_eq!(row.reported_total_tokens, None, "echo reports no usage");
    assert_eq!(row.usage_source, "unavailable");
    assert!(row.ordered, "created_at is the start, completed_at the end");
}

#[derive(sqlx::FromRow)]
struct StoredRow {
    id: String,
    client_request_id: Option<String>,
    client_api_key_id: Uuid,
    requested_model: String,
    selected_provider: String,
    selected_model: String,
    status: String,
    reported_total_tokens: Option<i64>,
    usage_source: String,
    ordered: bool,
}

fn record(key_id: Uuid) -> RequestRecord {
    RequestRecord {
        request_id: format!("req_{}", Uuid::now_v7()),
        client_request_id: None,
        client_api_key_id: key_id,
        requested_model: "gemini/gemini-3.5-flash".to_owned(),
        provider: "gemini".to_owned(),
        model: "gemini-3.5-flash".to_owned(),
        outcome: Outcome::Failed,
        failure: Failure {
            kind: Some("blocked"),
            code: Some("SAFETY".to_owned()),
            provider_status: None,
        },
        latency_ms: 812,
        usage: Usage {
            input_tokens: Some(7),
            output_tokens: None,
            total_tokens: Some(u32::MAX),
        },
    }
}

#[sqlx::test]
async fn failed_records_and_large_counts_are_stored(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;

    PgRecordStore::new(pool.clone())
        .insert(&record(key_id))
        .await
        .unwrap();

    let row: (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        String,
    ) = sqlx::query_as(
        "SELECT status, error_kind, error_code, reported_output_tokens, \
                    reported_total_tokens, usage_source FROM request_records",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "failed");
    assert_eq!(row.1.as_deref(), Some("blocked"));
    assert_eq!(row.2.as_deref(), Some("SAFETY"));
    assert_eq!(row.3, None);
    assert_eq!(row.4, Some(i64::from(u32::MAX)));
    assert_eq!(row.5, "provider_reported");
}

#[sqlx::test]
async fn the_schema_rejects_inconsistent_records(pool: PgPool) {
    let (_, key_id) = create_key(&pool).await;
    let store = PgRecordStore::new(pool.clone());

    let mut success_with_error = record(key_id);
    success_with_error.outcome = Outcome::Succeeded;
    let mut failure_without_kind = record(key_id);
    failure_without_kind.failure = Failure::default();
    let unknown_key = record(Uuid::now_v7());

    for (name, bad) in [
        ("success with error details", success_with_error),
        ("failure without a kind", failure_without_kind),
        ("unknown client key", unknown_key),
    ] {
        assert!(store.insert(&bad).await.is_err(), "{name} was accepted");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM request_records")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
