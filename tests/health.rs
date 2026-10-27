//! Health endpoint behavior with a fake database (spec cases 1, 2, 4--8).
//! Case 3 needs a real PostgreSQL and lives in `health_postgres.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use modelswitch::storage::{BoxFuture, DatabaseCheck, PingError};
use modelswitch::{AppState, build_router};

/// Deliberately sensitive, so the leak test has something to catch.
const SENSITIVE_ERROR: &str =
    "connection refused: postgres://modelswitch:hunter2@db.internal:5432/modelswitch_dev";

const DB_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
enum DbMode {
    Up,
    Down,
    Hang,
}

/// A database whose behavior the test controls, including switching it
/// between requests.
#[derive(Clone)]
struct FakeDatabase {
    mode: Arc<Mutex<DbMode>>,
}

impl FakeDatabase {
    fn new(mode: DbMode) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
        }
    }

    fn set(&self, mode: DbMode) {
        *self.mode.lock().unwrap() = mode;
    }
}

impl DatabaseCheck for FakeDatabase {
    fn ping(&self) -> BoxFuture<'_, Result<(), PingError>> {
        let mode = *self.mode.lock().unwrap();
        Box::pin(async move {
            match mode {
                DbMode::Up => Ok(()),
                DbMode::Down => Err(PingError(SENSITIVE_ERROR.to_owned())),
                DbMode::Hang => std::future::pending().await,
            }
        })
    }
}

fn app(db: FakeDatabase) -> Router {
    build_router(AppState {
        db: Arc::new(db),
        db_check_timeout: DB_CHECK_TIMEOUT,
    })
}

async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    get_with(app, Request::get(path).body(Body::empty()).unwrap()).await
}

async fn get_with(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn json_of(body: &str) -> Value {
    serde_json::from_str(body).unwrap()
}

// 1
#[tokio::test]
async fn live_returns_ok() {
    let app = app(FakeDatabase::new(DbMode::Up));

    let (status, body) = get(&app, "/health/live").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "status": "ok" }));
}

// 2
#[tokio::test(start_paused = true)]
async fn live_ignores_database_outage() {
    for mode in [DbMode::Down, DbMode::Hang] {
        let app = app(FakeDatabase::new(mode));

        let (status, _) = get(&app, "/health/live").await;

        assert_eq!(status, StatusCode::OK);
    }
}

// 4
#[tokio::test]
async fn ready_returns_503_when_database_unavailable() {
    let app = app(FakeDatabase::new(DbMode::Down));

    let (status, body) = get(&app, "/health/ready").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_of(&body),
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

    let (status, _) = tokio::time::timeout(Duration::from_secs(2), get(&app, "/health/ready"))
        .await
        .expect("readiness did not answer within 2s while the database hangs");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() < Duration::from_secs(2));
}

// 6
#[tokio::test]
async fn ready_recovers_after_database_returns() {
    let db = FakeDatabase::new(DbMode::Down);
    let app = app(db.clone());

    let (before, _) = get(&app, "/health/ready").await;
    db.set(DbMode::Up);
    let (after, body) = get(&app, "/health/ready").await;

    assert_eq!(before, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(after, StatusCode::OK);
    assert_eq!(json_of(&body)["checks"]["database"], "ok");
}

// 7
#[tokio::test]
async fn health_endpoints_need_no_api_key() {
    let app = app(FakeDatabase::new(DbMode::Up));

    for path in ["/health/live", "/health/ready"] {
        let (no_key, _) = get(&app, path).await;
        let wrong_key = Request::get(path)
            .header(header::AUTHORIZATION, "Bearer ms_dev_not_a_real_key")
            .body(Body::empty())
            .unwrap();
        let (with_wrong_key, _) = get_with(&app, wrong_key).await;

        assert_eq!(no_key, StatusCode::OK, "{path} without key");
        assert_eq!(with_wrong_key, StatusCode::OK, "{path} with wrong key");
    }
}

// 8
#[tokio::test]
async fn ready_failure_does_not_leak_details() {
    let app = app(FakeDatabase::new(DbMode::Down));

    let (_, body) = get(&app, "/health/ready").await;

    for fragment in [
        "postgres://",
        "hunter2",
        "db.internal",
        "5432",
        "connection refused",
    ] {
        assert!(!body.contains(fragment), "body leaked {fragment:?}: {body}");
    }
}
