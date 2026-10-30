//! Authentication against a real PostgreSQL (Stage 1 slice 2b spec,
//! case 14): a key from `keys create` works until `keys revoke`.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use sqlx::PgPool;
use uuid::Uuid;

use common::{AUTH_TIMEOUT, DB_CHECK_TIMEOUT, send};
use modelswitch::AppState;
use modelswitch::api::auth::AuthenticatedKey;
use modelswitch::app::{require_api_key, with_middleware};
use modelswitch::cli::keys;
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;

#[sqlx::test]
async fn end_to_end_with_postgres(pool: PgPool) {
    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: AUTH_TIMEOUT,
        catalog: Default::default(),
        providers: Default::default(),
    };
    let v1 = Router::new().route(
        "/v1/whoami",
        get(|Extension(key): Extension<AuthenticatedKey>| async move { key.id.to_string() }),
    );
    let app = with_middleware(require_api_key(v1, &state)).with_state(state);
    let call = |key: String| {
        let app = app.clone();
        async move {
            let request = Request::get("/v1/whoami")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap();
            send(&app, request).await
        }
    };

    let mut out = Vec::new();
    keys::create(&pool, "e2e", &mut out).await.unwrap();
    let key = String::from_utf8(out)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM client_api_keys")
        .fetch_one(&pool)
        .await
        .unwrap();

    let before = call(key.clone()).await;
    keys::revoke(&pool, id, &mut Vec::new()).await.unwrap();
    let after = call(key).await;

    assert_eq!(before.status, StatusCode::OK);
    assert_eq!(before.body, id.to_string());
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}
