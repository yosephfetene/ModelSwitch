//! Authentication against a real PostgreSQL (Stage 1 slice 2b spec,
//! case 14, and Stage 3 slice 3c): a key from `keys create` works until
//! it is revoked or its expiry passes, and `ms_live_` keys work the same.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use sqlx::PgPool;
use uuid::Uuid;

use common::{
    ACCOUNTING_TIMEOUT, AUTH_TIMEOUT, DB_CHECK_TIMEOUT, REQUEST_LIMITS, TestResponse, send,
};
use modelswitch::AppState;
use modelswitch::api::auth::AuthenticatedKey;
use modelswitch::app::{require_api_key, with_middleware};
use modelswitch::auth::api_key::KeyEnvironment;
use modelswitch::cli::keys::{self, CreateOptions};
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::request_records::PgRecordStore;

fn app(pool: &PgPool) -> Router {
    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: AUTH_TIMEOUT,
        records: Arc::new(PgRecordStore::new(pool.clone())),
        accounting_timeout: ACCOUNTING_TIMEOUT,
        limits: REQUEST_LIMITS,
        metrics: Default::default(),
        rate_limiter: common::generous_rate_limiter(),
        concurrency: common::generous_concurrency(),
        catalog: Default::default(),
        providers: Default::default(),
    };
    let v1 = Router::new().route(
        "/v1/whoami",
        get(|Extension(key): Extension<AuthenticatedKey>| async move { key.id.to_string() }),
    );
    with_middleware(require_api_key(v1, &state)).with_state(state)
}

async fn whoami(app: &Router, key: &str) -> TestResponse {
    let request = Request::get("/v1/whoami")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

/// Runs `keys create` and returns (plaintext key, ID).
async fn create(pool: &PgPool, options: CreateOptions) -> (String, Uuid) {
    let mut out = Vec::new();
    keys::create(pool, "e2e", options, &mut out).await.unwrap();
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

/// Moves the key's whole life into the past, ending `ago`.
async fn expire(pool: &PgPool, ago: &str) {
    sqlx::query(
        "UPDATE client_api_keys SET created_at = now() - interval '1 year', \
                expires_at = now() - $1::interval",
    )
    .bind(ago)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn end_to_end_with_postgres(pool: PgPool) {
    let app = app(&pool);
    let (key, id) = create(&pool, CreateOptions::default()).await;

    let before = whoami(&app, &key).await;
    keys::revoke(&pool, id, &mut Vec::new()).await.unwrap();
    let after = whoami(&app, &key).await;

    assert_eq!(before.status, StatusCode::OK);
    assert_eq!(before.body, id.to_string());
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn a_live_key_works_like_any_other(pool: PgPool) {
    let app = app(&pool);
    let options = CreateOptions {
        environment: KeyEnvironment::Live,
        ..Default::default()
    };
    let (key, id) = create(&pool, options).await;

    let response = whoami(&app, &key).await;

    assert!(key.starts_with("ms_live_"), "{key:.8}");
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, id.to_string());
}

#[sqlx::test]
async fn a_key_works_until_its_expiry_passes(pool: PgPool) {
    let app = app(&pool);
    let options = CreateOptions {
        expires_in_days: Some(30),
        ..Default::default()
    };
    let (key, _) = create(&pool, options).await;

    let before = whoami(&app, &key).await;
    expire(&pool, "1 second").await;
    let after = whoami(&app, &key).await;

    assert_eq!(before.status, StatusCode::OK);
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn a_key_without_an_expiry_keeps_working(pool: PgPool) {
    let app = app(&pool);
    let (key, _) = create(&pool, CreateOptions::default()).await;

    sqlx::query("UPDATE client_api_keys SET created_at = now() - interval '20 years'")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(whoami(&app, &key).await.status, StatusCode::OK);
}
