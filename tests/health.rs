//! Health endpoint behavior with a fake database (spec cases 1, 2, 4--8).
//! Case 3 needs a real PostgreSQL and lives in `health_postgres.rs`.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;

use common::{DbMode, FakeDatabase, app, get, send};

// 1
#[tokio::test]
async fn live_returns_ok() {
    let app = app(FakeDatabase::new(DbMode::Up));

    let response = get(&app, "/health/live").await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json(), json!({ "status": "ok" }));
}

// 2
#[tokio::test(start_paused = true)]
async fn live_ignores_database_outage() {
    for mode in [DbMode::Down, DbMode::Hang] {
        let app = app(FakeDatabase::new(mode));

        let response = get(&app, "/health/live").await;

        assert_eq!(response.status, StatusCode::OK);
    }
}

// 4
#[tokio::test]
async fn ready_returns_503_when_database_unavailable() {
    let app = app(FakeDatabase::new(DbMode::Down));

    let response = get(&app, "/health/ready").await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json(),
        json!({ "status": "not_ready", "checks": { "database": "unavailable" } })
    );
}

// 5: the clock is paused, so the one-second deadline passes instantly
// in virtual time and the test stays fast. The outer timeout makes a
// handler without a deadline fail this test instead of hanging it.
#[tokio::test(start_paused = true)]
async fn ready_times_out_when_database_hangs() {
    let app = app(FakeDatabase::new(DbMode::Hang));
    let started = tokio::time::Instant::now();

    let response = tokio::time::timeout(Duration::from_secs(2), get(&app, "/health/ready"))
        .await
        .expect("readiness did not answer within 2s while the database hangs");

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() < Duration::from_secs(2));
}

// 6
#[tokio::test]
async fn ready_recovers_after_database_returns() {
    let db = FakeDatabase::new(DbMode::Down);
    let app = app(db.clone());

    let before = get(&app, "/health/ready").await;
    db.set(DbMode::Up);
    let after = get(&app, "/health/ready").await;

    assert_eq!(before.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(after.status, StatusCode::OK);
    assert_eq!(after.json()["checks"]["database"], "ok");
}

// 7
#[tokio::test]
async fn health_endpoints_need_no_api_key() {
    let app = app(FakeDatabase::new(DbMode::Up));

    for path in ["/health/live", "/health/ready"] {
        let no_key = get(&app, path).await;
        let wrong_key = Request::get(path)
            .header(header::AUTHORIZATION, "Bearer ms_dev_not_a_real_key")
            .body(Body::empty())
            .unwrap();
        let with_wrong_key = send(&app, wrong_key).await;

        assert_eq!(no_key.status, StatusCode::OK, "{path} without key");
        assert_eq!(
            with_wrong_key.status,
            StatusCode::OK,
            "{path} with wrong key"
        );
    }
}

// 8
#[tokio::test]
async fn ready_failure_does_not_leak_details() {
    let app = app(FakeDatabase::new(DbMode::Down));

    let response = get(&app, "/health/ready").await;

    for fragment in [
        "postgres://",
        "hunter2",
        "db.internal",
        "5432",
        "connection refused",
    ] {
        assert!(
            !response.body.contains(fragment),
            "body leaked {fragment:?}: {}",
            response.body
        );
    }
}
