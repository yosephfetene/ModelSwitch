//! What request handling writes to the logs (Stage 1 slice 1 spec,
//! cases 11--12).
//!
//! Every test here calls `CapturedLogs::global()` first and shares one
//! buffer, so assertions only look at lines carrying the test's own
//! request ID. Negative checks ("never logged") are paired with a
//! positive one proving the capture saw the request at all.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};

use common::{CapturedLogs, DbMode, FakeDatabase, TestResponse, app, send};

const CORRELATION_ID: &str = "order-42.retry_1:abc";

fn get_with_client_id(path: &str, client_request_id: &str) -> Request<Body> {
    Request::get(path)
        .header("x-client-request-id", client_request_id)
        .body(Body::empty())
        .unwrap()
}

fn request_id(response: &TestResponse) -> &str {
    response
        .header("x-request-id")
        .expect("X-Request-Id header")
}

/// The captured lines that belong to this response's request.
fn lines_of(logs: &str, response: &TestResponse) -> Vec<String> {
    let id = request_id(response);
    logs.lines()
        .filter(|line| line.contains(id))
        .map(str::to_owned)
        .collect()
}

// 11
#[tokio::test]
async fn logs_carry_request_and_correlation_ids() {
    let logs = CapturedLogs::global();
    let app = app(FakeDatabase::new(DbMode::Down));

    let response = send(&app, get_with_client_id("/health/ready", CORRELATION_ID)).await;

    let all = logs.text();
    let ours = lines_of(&all, &response);
    let warning = ours
        .iter()
        .find(|line| line.contains("readiness: database ping failed"))
        .unwrap_or_else(|| panic!("no readiness warning with our request ID:\n{all}"));
    assert!(warning.contains(CORRELATION_ID), "{warning}");
}

// 12
#[tokio::test]
async fn invalid_correlation_id_is_not_logged() {
    let logs = CapturedLogs::global();
    let forged_log_line = "x INFO admin login succeeded user=root";
    let app = app(FakeDatabase::new(DbMode::Up));

    let response = send(&app, get_with_client_id("/health/live", forged_log_line)).await;

    let all = logs.text();
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    let ours = lines_of(&all, &response);
    assert!(
        ours.iter()
            .any(|line| line.contains("rejected X-Client-Request-Id")),
        "rejection not logged with our request ID:\n{all}"
    );
    assert!(!all.contains("admin login succeeded"), "{all}");
}
