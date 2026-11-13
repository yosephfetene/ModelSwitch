//! Dashboard login on the admin listener (Stage 4, ADR-036): sessions,
//! cookies, origins, expiry, and the failed-login limit.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use secrecy::SecretString;
use serde_json::json;

use common::{CapturedLogs, DbMode, FakeDatabase, FakeHistoryStore, TestResponse, send, state};
use modelswitch::api::admin::{AdminState, SESSION_COOKIE};
use modelswitch::auth::admin::{FAILED_LOGIN_WINDOW, MAX_FAILED_LOGINS};
use modelswitch::config::AdminConfig;
use modelswitch::metrics::Metrics;
use modelswitch::{build_admin_router, build_router};

const TOKEN: &str = "admin-token-for-tests-0123456789abcdef";
const HOST: &str = "127.0.0.1:9091";
const ORIGIN: &str = "http://127.0.0.1:9091";

struct Admin {
    app: Router,
    metrics: Arc<Metrics>,
}

fn admin(session_hours: u64) -> Admin {
    CapturedLogs::global();
    let config = AdminConfig {
        bind: HOST.parse().unwrap(),
        token: SecretString::from(TOKEN),
        session_ttl: Duration::from_secs(session_hours * 3600),
        database_timeout: Duration::from_secs(1),
        dashboard_dir: None,
    };
    let metrics = Arc::new(Metrics::default());
    Admin {
        app: build_admin_router(AdminState::new(
            &config,
            Arc::clone(&metrics),
            Arc::new(FakeHistoryStore::new(DbMode::Up)),
        )),
        metrics,
    }
}

fn post(path: &str, origin: Option<&str>, cookie: Option<&str>, body: Body) -> Request<Body> {
    let mut request = Request::post(path)
        .header("host", HOST)
        .header("content-type", "application/json");
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    request.body(body).unwrap()
}

async fn login_from(app: &Router, token: &str, origin: Option<&str>) -> TestResponse {
    let body = Body::from(json!({ "token": token }).to_string());
    send(app, post("/admin/api/login", origin, None, body)).await
}

async fn login(app: &Router, token: &str) -> TestResponse {
    login_from(app, token, Some(ORIGIN)).await
}

async fn logout(app: &Router, cookie: Option<&str>, origin: Option<&str>) -> TestResponse {
    send(
        app,
        post("/admin/api/logout", origin, cookie, Body::empty()),
    )
    .await
}

async fn session(app: &Router, cookie: Option<&str>) -> TestResponse {
    let mut request = Request::get("/admin/api/session").header("host", HOST);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

/// `name=value` from the response's `Set-Cookie`, ready to send back.
fn cookie_pair(response: &TestResponse) -> String {
    let set_cookie = response.header("set-cookie").expect("a session cookie");
    set_cookie.split(';').next().unwrap().to_owned()
}

/// A logged-in session's cookie.
async fn logged_in(app: &Router) -> String {
    let response = login(app, TOKEN).await;
    assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.body);
    cookie_pair(&response)
}

fn assert_error(response: &TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.json()["error"]["code"], code, "{}", response.body);
}

#[tokio::test]
async fn login_sets_a_session_cookie_scripts_cannot_read() {
    let h = admin(12);

    let response = login(&h.app, TOKEN).await;

    assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.body);
    let set_cookie = response.header("set-cookie").unwrap();
    let attributes: Vec<&str> = set_cookie.split("; ").collect();
    let (name, value) = attributes[0].split_once('=').unwrap();
    assert_eq!(name, SESSION_COOKIE);
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|b| b.is_ascii_hexdigit()), "{value}");
    for attribute in [
        "HttpOnly",
        "SameSite=Strict",
        "Path=/admin/api",
        "Max-Age=43200",
    ] {
        assert!(attributes.contains(&attribute), "{set_cookie}");
    }
    assert_eq!(response.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn the_session_cookie_opens_admin_routes() {
    let h = admin(12);
    let cookie = logged_in(&h.app).await;

    let response = session(&h.app, Some(&cookie)).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let left = response.json()["expires_in_secs"].as_u64().unwrap();
    assert!((43_190..=43_200).contains(&left), "{left}");
    assert_eq!(response.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn the_session_cookie_is_found_among_other_cookies() {
    let h = admin(12);
    let cookie = logged_in(&h.app).await;

    let response = session(&h.app, Some(&format!("theme=dark; {cookie}; lang=en"))).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
}

#[tokio::test]
async fn admin_routes_need_a_live_session() {
    let h = admin(12);
    logged_in(&h.app).await;
    let made_up = format!("{SESSION_COOKIE}={}", "a".repeat(64));
    let empty = format!("{SESSION_COOKIE}=");
    let other_name = "session=abc".to_owned();

    for cookie in [None, Some(made_up), Some(empty), Some(other_name)] {
        let response = session(&h.app, cookie.as_deref()).await;

        assert_error(&response, StatusCode::UNAUTHORIZED, "login_required");
        assert_eq!(response.header("www-authenticate"), None, "not an API key");
    }
}

#[tokio::test]
async fn a_wrong_token_is_refused_counted_and_starts_no_session() {
    let h = admin(12);

    for token in ["", "wrong", &TOKEN[1..], &format!("{TOKEN} ")] {
        let response = login(&h.app, token).await;

        assert_error(&response, StatusCode::UNAUTHORIZED, "login_failed");
        assert_eq!(response.header("set-cookie"), None);
    }
    assert!(
        h.metrics
            .render()
            .contains("admin_login_failures_total 4\n"),
        "{}",
        h.metrics.render()
    );
}

#[tokio::test]
async fn login_needs_the_pages_own_origin() {
    let h = admin(12);

    for origin in [
        None,
        Some("http://evil.example"),
        Some("http://127.0.0.1:9092"),
        Some("http://localhost:9091"),
        Some("null"),
        Some("ftp://127.0.0.1:9091"),
    ] {
        let response = login_from(&h.app, TOKEN, origin).await;

        assert_error(&response, StatusCode::FORBIDDEN, "forbidden");
        assert_eq!(response.header("set-cookie"), None, "{origin:?}");
    }
    assert!(
        h.metrics
            .render()
            .contains("admin_login_failures_total 0\n")
    );
}

#[tokio::test]
async fn a_proxy_origin_matching_its_host_is_accepted() {
    let h = admin(12);
    let body = Body::from(json!({ "token": TOKEN }).to_string());
    let request = Request::post("/admin/api/login")
        .header("host", "admin.example.com")
        .header("origin", "https://admin.example.com")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = send(&h.app, request).await;

    assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.body);
}

#[tokio::test]
async fn logout_ends_the_session_and_clears_the_cookie() {
    let h = admin(12);
    let cookie = logged_in(&h.app).await;
    let other = logged_in(&h.app).await;

    let response = logout(&h.app, Some(&cookie), Some(ORIGIN)).await;

    assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.body);
    let set_cookie = response.header("set-cookie").unwrap();
    assert!(
        set_cookie.starts_with(&format!("{SESSION_COOKIE}=;")),
        "{set_cookie}"
    );
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
    assert_error(
        &session(&h.app, Some(&cookie)).await,
        StatusCode::UNAUTHORIZED,
        "login_required",
    );
    assert_eq!(
        session(&h.app, Some(&other)).await.status,
        StatusCode::OK,
        "other sessions stay"
    );
}

#[tokio::test]
async fn logout_without_a_session_still_succeeds() {
    let h = admin(12);

    let response = logout(&h.app, None, Some(ORIGIN)).await;

    assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.body);
}

#[tokio::test]
async fn logout_from_another_origin_is_refused_and_keeps_the_session() {
    let h = admin(12);
    let cookie = logged_in(&h.app).await;

    for origin in [None, Some("http://evil.example")] {
        let response = logout(&h.app, Some(&cookie), origin).await;

        assert_error(&response, StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(session(&h.app, Some(&cookie)).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_session_expires_after_its_hours() {
    let h = admin(1);
    let cookie = logged_in(&h.app).await;

    tokio::time::advance(Duration::from_secs(3599)).await;
    let response = session(&h.app, Some(&cookie)).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["expires_in_secs"], 1);

    tokio::time::advance(Duration::from_secs(1)).await;
    assert_error(
        &session(&h.app, Some(&cookie)).await,
        StatusCode::UNAUTHORIZED,
        "login_required",
    );
}

#[tokio::test(start_paused = true)]
async fn too_many_failed_logins_pause_every_login() {
    let h = admin(12);
    for _ in 0..MAX_FAILED_LOGINS {
        login(&h.app, "wrong").await;
    }

    let response = login(&h.app, TOKEN).await;

    assert_error(&response, StatusCode::TOO_MANY_REQUESTS, "login_throttled");
    assert_eq!(
        response.header("retry-after"),
        Some(FAILED_LOGIN_WINDOW.as_secs().to_string().as_str())
    );
    assert_eq!(response.header("set-cookie"), None);

    tokio::time::advance(FAILED_LOGIN_WINDOW).await;
    assert_eq!(login(&h.app, TOKEN).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test(start_paused = true)]
async fn a_correct_login_is_not_a_failure() {
    let h = admin(12);
    for _ in 0..MAX_FAILED_LOGINS * 2 {
        logged_in(&h.app).await;
    }

    assert_eq!(login(&h.app, TOKEN).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn malformed_login_bodies_are_rejected() {
    let h = admin(12);
    let request = |content_type: &str, body: String| {
        Request::post("/admin/api/login")
            .header("host", HOST)
            .header("origin", ORIGIN)
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap()
    };
    let token = json!({ "token": TOKEN }).to_string();

    for (content_type, body) in [
        ("text/plain", token.clone()),
        ("application/json", "not json".to_owned()),
        ("application/json", json!({ "token": 42 }).to_string()),
        (
            "application/json",
            json!({ "token": TOKEN, "extra": 1 }).to_string(),
        ),
    ] {
        let response = send(&h.app, request(content_type, body)).await;

        assert_error(&response, StatusCode::BAD_REQUEST, "invalid_request");
    }

    let huge = json!({ "token": "x".repeat(8 * 1024) }).to_string();
    let response = send(&h.app, request("application/json", huge)).await;
    assert_error(
        &response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );
}

#[tokio::test]
async fn neither_the_token_nor_a_session_id_is_logged() {
    let h = admin(12);
    let wrong = "wrong-token-LOG-CANARY-0123456789abcdef";
    login(&h.app, wrong).await;
    let cookie = logged_in(&h.app).await;
    let session_id = cookie.split_once('=').unwrap().1.to_owned();
    session(&h.app, Some(&cookie)).await;
    logout(&h.app, Some(&cookie), Some(ORIGIN)).await;

    let logs = CapturedLogs::global().text();

    assert!(logs.contains("admin: login failed"), "logs are captured");
    for secret in [TOKEN, wrong, &session_id] {
        assert!(!logs.contains(secret), "{secret} was logged");
    }
}

#[tokio::test]
async fn unknown_admin_paths_are_not_found_not_login_required() {
    let h = admin(12);

    let response = session_path(&h.app, "/admin/api/nope").await;

    assert_error(&response, StatusCode::NOT_FOUND, "not_found");
}

async fn session_path(app: &Router, path: &str) -> TestResponse {
    let request = Request::get(path).header("host", HOST);
    send(app, request.body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn the_admin_listener_serves_neither_the_gateway_nor_metrics() {
    let h = admin(12);

    for path in ["/v1/generate", "/metrics", "/health/live"] {
        let response = session_path(&h.app, path).await;
        assert_error(&response, StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test]
async fn the_public_port_has_no_admin_routes() {
    let app = build_router(state(FakeDatabase::new(DbMode::Up)));

    let response = login(&app, TOKEN).await;

    assert_error(&response, StatusCode::NOT_FOUND, "not_found");
}
