//! API key authentication (Stage 1 slice 2b spec, cases 1--13). Case 14
//! needs a real PostgreSQL and lives in `auth_postgres.rs`.
//!
//! `/v1/generate` does not exist yet, so these tests protect a small
//! route that echoes the authenticated key's ID. Every test installs the
//! global log capture first (see `CapturedLogs::global`).

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};

use common::{
    CapturedLogs, DbMode, FakeDatabase, FakeKeyStore, TestResponse, send, state_with_keys,
};
use modelswitch::api::auth::AuthenticatedKey;
use modelswitch::api::health;
use modelswitch::app::{require_api_key, with_middleware};

const PROTECTED: &str = "/v1/whoami";
const WELL_FORMED_UNKNOWN: &str = "ms_dev_0000000000000000000000000000000000000000000";

/// Health plus one protected route, wired like the real application.
fn app(keys: &FakeKeyStore) -> Router {
    CapturedLogs::global();
    let state = state_with_keys(FakeDatabase::new(DbMode::Up), keys.clone());
    let v1 = Router::new().route(
        PROTECTED,
        get(|Extension(key): Extension<AuthenticatedKey>| async move { key.id.to_string() }),
    );
    with_middleware(health::router().merge(require_api_key(v1, &state))).with_state(state)
}

async fn call(app: &Router, path: &str, authorization: Option<&str>) -> TestResponse {
    let mut request = Request::get(path);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

async fn call_with_key(app: &Router, key: &str) -> TestResponse {
    call(app, PROTECTED, Some(&format!("Bearer {key}"))).await
}

fn assert_unauthorized(response: &TestResponse) {
    assert_eq!(
        response.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        response.body
    );
    assert_eq!(response.json()["error"]["code"], "unauthorized");
    assert_eq!(response.header("www-authenticate"), Some("Bearer"));
}

fn request_id(response: &TestResponse) -> &str {
    response.header("x-request-id").expect("X-Request-Id")
}

/// The captured log lines belonging to this response's request.
fn our_logs(response: &TestResponse) -> Vec<String> {
    let id = request_id(response).to_owned();
    CapturedLogs::global()
        .text()
        .lines()
        .filter(|line| line.contains(&id))
        .map(str::to_owned)
        .collect()
}

// 1
#[tokio::test]
async fn health_stays_open_beside_protected_routes() {
    let app = app(&FakeKeyStore::new(DbMode::Up));

    for path in ["/health/live", "/health/ready"] {
        assert_eq!(
            call(&app, path, None).await.status,
            StatusCode::OK,
            "{path}"
        );
    }
}

// 2
#[tokio::test]
async fn valid_key_is_accepted() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, id) = keys.add_key();

    let response = call_with_key(&app(&keys), &key).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, id.to_string());
}

// 3
#[tokio::test]
async fn missing_key_is_rejected() {
    let response = call(&app(&FakeKeyStore::new(DbMode::Up)), PROTECTED, None).await;

    assert_unauthorized(&response);
}

// 4
#[tokio::test]
async fn malformed_key_is_rejected_without_database() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let app = app(&keys);

    for value in [
        "hello",
        "Bearer hello",
        &format!("Basic {key}"),
        &format!("Bearer {}", &key[..key.len() - 1]),
    ] {
        assert_unauthorized(&call(&app, PROTECTED, Some(value)).await);
    }
    assert_eq!(keys.lookups(), 0, "malformed keys reached the database");
}

// 5
#[tokio::test]
async fn unknown_key_is_rejected() {
    let response = call_with_key(&app(&FakeKeyStore::new(DbMode::Up)), WELL_FORMED_UNKNOWN).await;

    assert_unauthorized(&response);
}

// 6
#[tokio::test]
async fn revoked_key_is_rejected() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    keys.revoke(&key);

    assert_unauthorized(&call_with_key(&app(&keys), &key).await);
}

// 7
#[tokio::test]
async fn all_rejections_look_the_same() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (revoked, _) = keys.add_key();
    keys.revoke(&revoked);
    let app = app(&keys);

    let responses = [
        call(&app, PROTECTED, None).await,
        call(&app, PROTECTED, Some("Bearer hello")).await,
        call_with_key(&app, WELL_FORMED_UNKNOWN).await,
        call_with_key(&app, &revoked).await,
    ];

    // Everything a client sees, minus the per-request ID.
    let visible = |response: &TestResponse| {
        let mut body = response.json();
        body.as_object_mut().unwrap().remove("request_id");
        let mut headers: Vec<(String, String)> = response
            .headers
            .iter()
            .filter(|(name, _)| *name != "x-request-id")
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect();
        headers.sort();
        (response.status, headers, body)
    };
    let first = visible(&responses[0]);
    for response in &responses[1..] {
        assert_eq!(visible(response), first);
    }
}

// 8
#[tokio::test]
async fn revocation_takes_effect_immediately() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let app = app(&keys);

    let before = call_with_key(&app, &key).await;
    keys.revoke(&key);
    let after = call_with_key(&app, &key).await;

    assert_eq!(before.status, StatusCode::OK);
    assert_unauthorized(&after);
}

// 9
#[tokio::test]
async fn database_down_returns_503_and_fails_closed() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    keys.set(DbMode::Down);

    let response = call_with_key(&app(&keys), &key).await;

    // The handler would have answered 200 with the key ID.
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.json()["error"]["code"], "service_unavailable");
    for fragment in ["postgres://", "hunter2", "connection refused"] {
        assert!(!response.body.contains(fragment), "leaked {fragment:?}");
    }
}

// 10
#[tokio::test(start_paused = true)]
async fn database_hang_returns_503_within_deadline() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    keys.set(DbMode::Hang);
    let app = app(&keys);

    let response = tokio::time::timeout(Duration::from_secs(2), call_with_key(&app, &key))
        .await
        .expect("auth did not answer within 2s while the database hangs");

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
}

// 11
#[tokio::test]
async fn malformed_key_is_401_even_when_database_down() {
    let keys = FakeKeyStore::new(DbMode::Down);

    let response = call(&app(&keys), PROTECTED, Some("Bearer hello")).await;

    assert_unauthorized(&response);
    assert_eq!(keys.lookups(), 0);
}

// 12
#[tokio::test]
async fn rejections_log_reason_never_key() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (revoked, revoked_id) = keys.add_key();
    keys.revoke(&revoked);
    let app = app(&keys);

    let cases = [
        (call(&app, PROTECTED, None).await, "reason=missing"),
        (
            call(&app, PROTECTED, Some("Bearer hello")).await,
            "reason=malformed",
        ),
        (
            call_with_key(&app, WELL_FORMED_UNKNOWN).await,
            "reason=unknown",
        ),
        (call_with_key(&app, &revoked).await, "reason=revoked"),
    ];

    for (response, reason) in &cases {
        let logs = our_logs(response);
        assert!(
            logs.iter()
                .any(|line| line.contains("auth: rejected API key") && line.contains(reason)),
            "no {reason} line: {logs:#?}"
        );
    }
    let revoked_logs = our_logs(&cases[3].0);
    assert!(
        revoked_logs
            .iter()
            .any(|line| line.contains(&revoked_id.to_string())),
        "{revoked_logs:#?}"
    );
    let all = CapturedLogs::global().text();
    let body = revoked.strip_prefix("ms_dev_").unwrap();
    assert!(!all.contains(body), "key leaked into logs");
}

// 13
#[tokio::test]
async fn accepted_key_id_is_in_logs() {
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, id) = keys.add_key();

    let response = call_with_key(&app(&keys), &key).await;

    assert_eq!(response.status, StatusCode::OK);
    let logs = our_logs(&response);
    assert!(
        logs.iter()
            .any(|line| line.contains(&format!("key_id={id}"))),
        "{logs:#?}"
    );
    let all = CapturedLogs::global().text();
    assert!(!all.contains(key.strip_prefix("ms_dev_").unwrap()));
}

// Route layer: auth applies only to routes that exist.
#[tokio::test]
async fn unknown_v1_path_is_404_not_401() {
    let response = call(&app(&FakeKeyStore::new(DbMode::Up)), "/v1/nope", None).await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.json()["error"]["code"], "not_found");
}
