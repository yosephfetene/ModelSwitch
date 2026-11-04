//! Readiness against a real PostgreSQL (spec case 3). `#[sqlx::test]`
//! creates a throwaway database per test from `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::PgPool;
use tower::ServiceExt;

use modelswitch::reliability::Limits;
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::{AppState, build_router};

// 3
#[sqlx::test]
async fn ready_returns_ok_when_database_reachable(pool: PgPool) {
    let app = build_router(AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: Duration::from_secs(1),
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: Duration::from_secs(1),
        records: Arc::new(PgRecordStore::new(pool)),
        accounting_timeout: Duration::from_secs(1),
        limits: Limits {
            max_attempts: 3,
            request_timeout: Duration::from_secs(60),
        },
        metrics: Default::default(),
        catalog: Default::default(),
        providers: Default::default(),
    });

    let response = app
        .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
