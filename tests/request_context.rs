//! Request IDs, client correlation IDs, and the error envelope
//! (Stage 1 slice 1 spec, cases 1--12).

mod common;

use std::collections::HashSet;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get as get_route;
use tracing::Level;
use uuid::Uuid;

use common::{CapturedLogs, DbMode, FakeDatabase, TestResponse, app, get, send};
use modelswitch::app::with_middleware;

const CORRELATION_ID: &str = "order-42.retry_1:abc";

fn up() -> Router {
    app(FakeDatabase::new(DbMode::Up))
}

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

fn assert_valid_request_id(id: &str) {
    let uuid = id
        .strip_prefix("req_")
        .unwrap_or_else(|| panic!("{id:?} lacks the req_ prefix"));
    let uuid = Uuid::parse_str(uuid).unwrap_or_else(|_| panic!("{id:?} is not a UUID"));
    assert_eq!(uuid.get_version_num(), 7, "{id:?} is not a UUIDv7");
}

/// Asserts the standard error shape, with `request_id` matching the header.
fn assert_error(response: &TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status, status);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = response.json();
    assert_eq!(body["error"]["code"], code, "{body}");
    assert!(body["error"]["message"].is_string(), "{body}");
    assert_eq!(body["request_id"], request_id(response), "{body}");
}

// 1
#[tokio::test]
async fn success_response_has_request_id() {
    let response = get(&up(), "/health/live").await;

    assert_eq!(response.status, StatusCode::OK);
    assert_valid_request_id(request_id(&response));
}

// 2
#[tokio::test]
async fn error_body_request_id_matches_header() {
    let response = get(&up(), "/no/such/route").await;

    assert_valid_request_id(request_id(&response));
    assert_error(&response, StatusCode::NOT_FOUND, "not_found");
}

// 3
#[tokio::test]
async fn request_ids_are_unique() {
    let app = up();
    let mut seen = HashSet::new();

    for _ in 0..100 {
        let response = get(&app, "/health/live").await;
        let id = request_id(&response).to_owned();
        assert!(seen.insert(id.clone()), "duplicate request ID {id}");
    }
}

// 4
#[tokio::test]
async fn client_cannot_choose_request_id() {
    let forged = "req_0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    let request = Request::get("/health/live")
        .header("x-request-id", forged)
        .body(Body::empty())
        .unwrap();

    let response = send(&up(), request).await;

    assert_ne!(request_id(&response), forged);
    assert_valid_request_id(request_id(&response));
}

// 5
#[tokio::test]
async fn valid_correlation_id_is_echoed() {
    let response = send(&up(), get_with_client_id("/health/live", CORRELATION_ID)).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("x-client-request-id"), Some(CORRELATION_ID));
}

// 6
#[tokio::test]
async fn missing_correlation_id_is_fine() {
    let response = get(&up(), "/health/live").await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("x-client-request-id"), None);
}

// 7
#[tokio::test]
async fn invalid_correlation_id_is_rejected() {
    let too_long = "x".repeat(129);
    for value in [too_long.as_str(), "has spaces", "semi;colon", "<script>"] {
        let response = send(&up(), get_with_client_id("/health/live", value)).await;

        assert_error(&response, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(response.header("x-client-request-id"), None, "{value:?}");
        assert!(!response.body.contains(value), "body echoed {value:?}");
    }
}

// 8
#[tokio::test]
async fn unknown_route_returns_json_404() {
    let response = get(&up(), "/v1/does-not-exist").await;

    assert_error(&response, StatusCode::NOT_FOUND, "not_found");
}

// 9
#[tokio::test]
async fn wrong_method_returns_json_405() {
    let request = Request::post("/health/live").body(Body::empty()).unwrap();

    let response = send(&up(), request).await;

    assert_error(
        &response,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
    let allow = response.header("allow").expect("Allow header");
    assert!(allow.contains("GET"), "Allow: {allow}");
}

// 10: the panic message is printed to stderr by Rust's panic hook; that
// noise in the test output is expected.
#[tokio::test]
async fn panic_returns_500_and_server_survives() {
    let app = with_middleware(
        Router::new()
            .route(
                "/boom",
                get_route(|| async {
                    panic!("secret detail: hunter2");
                    #[allow(unreachable_code)]
                    ""
                }),
            )
            .route("/fine", get_route(|| async { "fine" })),
    );

    let boom = get(&app, "/boom").await;
    let after = get(&app, "/fine").await;

    assert_error(&boom, StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    for fragment in ["secret detail", "hunter2", "panicked", ".rs"] {
        assert!(!boom.body.contains(fragment), "body leaked {fragment:?}");
    }
    assert_eq!(after.status, StatusCode::OK);
}

// 11
#[tokio::test]
async fn logs_carry_request_and_correlation_ids() {
    let (logs, _guard) = CapturedLogs::start(Level::INFO);
    let app = app(FakeDatabase::new(DbMode::Down));

    let response = send(&app, get_with_client_id("/health/ready", CORRELATION_ID)).await;

    let logs = logs.text();
    let warning = logs
        .lines()
        .find(|line| line.contains("readiness: database ping failed"))
        .unwrap_or_else(|| panic!("readiness warning not logged:\n{logs}"));
    assert!(warning.contains(request_id(&response)), "{warning}");
    assert!(warning.contains(CORRELATION_ID), "{warning}");
}

// 12
#[tokio::test]
async fn invalid_correlation_id_is_not_logged() {
    let (logs, _guard) = CapturedLogs::start(Level::TRACE);
    let forged_log_line = "x INFO admin login succeeded user=root";

    let response = send(&up(), get_with_client_id("/health/live", forged_log_line)).await;

    let logs = logs.text();
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert!(logs.contains("rejected X-Client-Request-Id"), "{logs}");
    assert!(logs.contains(request_id(&response)), "{logs}");
    assert!(!logs.contains("admin login succeeded"), "{logs}");
}
