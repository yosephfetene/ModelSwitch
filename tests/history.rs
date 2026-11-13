//! Request history API on the admin listener (Stage 4, ADR-036): login
//! required, filters checked before the database, JSON shape, deadlines.
//! The SQL itself is tested in `tests/history_postgres.rs`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde_json::json;
use uuid::Uuid;

use common::{CapturedLogs, DbMode, FakeHistoryStore, SENSITIVE_ERROR, TestResponse, send};
use modelswitch::accounting::Outcome;
use modelswitch::api::admin::AdminState;
use modelswitch::build_admin_router;
use modelswitch::config::AdminConfig;
use modelswitch::history::{
    AttemptView, HistoryFilter, HistoryPage, KeyLabel, PriceView, RequestDetail, RequestSummary,
};
use modelswitch::metrics::Metrics;

const TOKEN: &str = "admin-token-for-tests-0123456789abcdef";
const HOST: &str = "127.0.0.1:9091";
const ORIGIN: &str = "http://127.0.0.1:9091";

struct Admin {
    app: Router,
    history: FakeHistoryStore,
    cookie: String,
}

async fn admin(mode: DbMode) -> Admin {
    CapturedLogs::global();
    let history = FakeHistoryStore::new(mode);
    let config = AdminConfig {
        bind: HOST.parse().unwrap(),
        token: SecretString::from(TOKEN),
        session_ttl: Duration::from_secs(3600),
        database_timeout: Duration::from_secs(1),
        dashboard_dir: None,
    };
    let app = build_admin_router(AdminState::new(
        &config,
        Arc::new(Metrics::default()),
        Arc::new(history.clone()),
    ));
    let login = Request::post("/admin/api/login")
        .header("host", HOST)
        .header("origin", ORIGIN)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "token": TOKEN }).to_string()))
        .unwrap();
    let response = send(&app, login).await;
    assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.body);
    let cookie = response
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    Admin {
        app,
        history,
        cookie,
    }
}

impl Admin {
    async fn get(&self, path: &str) -> TestResponse {
        let request = Request::get(path)
            .header("host", HOST)
            .header("cookie", &self.cookie)
            .body(Body::empty())
            .unwrap();
        send(&self.app, request).await
    }
}

fn request_id() -> String {
    format!("req_{}", Uuid::now_v7())
}

fn summary(id: &str) -> RequestSummary {
    RequestSummary {
        id: id.to_owned(),
        client_request_id: Some("client-1".to_owned()),
        created_at: "2026-11-12T08:30:00.123Z".to_owned(),
        key: KeyLabel {
            id: Uuid::nil(),
            name: "laptop".to_owned(),
            prefix: "ms_dev_7Hq2".to_owned(),
        },
        requested_model: "fast".to_owned(),
        provider: "gemini".to_owned(),
        model: "gemini-3.5-flash-lite".to_owned(),
        status: "succeeded".to_owned(),
        error_kind: None,
        error_code: None,
        provider_status: None,
        latency_ms: 1544,
        input_tokens: Some(8),
        output_tokens: Some(1),
        total_tokens: Some(9),
        usage_source: "provider_reported".to_owned(),
        estimated_cost_usd: Some("0.0000049".to_owned()),
        pricing_version: Some(Uuid::max()),
        attempt_count: 2,
    }
}

fn assert_error(response: &TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.json()["error"]["code"], code, "{}", response.body);
}

#[tokio::test]
async fn history_needs_a_login() {
    let h = admin(DbMode::Up).await;
    let id = request_id();

    for path in [
        "/admin/api/requests".to_owned(),
        format!("/admin/api/requests/{id}"),
    ] {
        let request = Request::get(&path)
            .header("host", HOST)
            .body(Body::empty())
            .unwrap();
        let response = send(&h.app, request).await;

        assert_error(&response, StatusCode::UNAUTHORIZED, "login_required");
    }
    assert!(h.history.filters().is_empty() && h.history.gets().is_empty());
}

#[tokio::test]
async fn no_filters_means_the_newest_page_of_everything() {
    let h = admin(DbMode::Up).await;

    let response = h.get("/admin/api/requests").await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(h.history.filters(), vec![HistoryFilter::default()]);
    assert_eq!(h.history.filters()[0].limit, 50);
    assert_eq!(
        response.json(),
        json!({ "requests": [], "next_before": null })
    );
    assert_eq!(response.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn every_filter_is_passed_on() {
    let h = admin(DbMode::Up).await;
    let before = request_id();

    let response = h
        .get(&format!(
            "/admin/api/requests?from=2026-11-12T03:00:00%2B03:00&to=2026-11-13T00:00:00Z\
             &provider=groq-2&status=timed_out&before={before}&limit=200"
        ))
        .await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let utc = |text: &str| text.parse::<DateTime<Utc>>().unwrap();
    assert_eq!(
        h.history.filters(),
        vec![HistoryFilter {
            from: Some(utc("2026-11-12T00:00:00Z")),
            to: Some(utc("2026-11-13T00:00:00Z")),
            provider: Some("groq-2".to_owned()),
            status: Some(Outcome::TimedOut),
            before: Some(before),
            limit: 200,
        }]
    );
}

#[tokio::test]
async fn each_status_is_understood() {
    let h = admin(DbMode::Up).await;

    for (text, status) in [
        ("succeeded", Outcome::Succeeded),
        ("failed", Outcome::Failed),
        ("timed_out", Outcome::TimedOut),
    ] {
        let response = h.get(&format!("/admin/api/requests?status={text}")).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(h.history.filters().last().unwrap().status, Some(status));
    }
}

#[tokio::test]
async fn bad_filters_are_refused_before_the_database() {
    let h = admin(DbMode::Up).await;
    let v4 = "req_6f9619ff-8b86-4011-b42d-00c04fc964ff".to_owned();
    let long = "a".repeat(65);

    for query in [
        "from=yesterday",
        "from=2026-11-12",
        "to=2026-11-12T00:00:00",
        "from=2026-11-12T00:00:00Z&to=2026-11-12T00:00:00Z",
        "from=2026-11-13T00:00:00Z&to=2026-11-12T00:00:00Z",
        "provider=Groq",
        "provider=a%20b",
        "provider=",
        &format!("provider={long}"),
        "status=ok",
        "status=",
        "before=req_123",
        &format!("before={v4}"),
        "limit=0",
        "limit=201",
        "limit=-1",
        "limit=ten",
        "key=abc",
        "Status=failed",
    ] {
        let response = h.get(&format!("/admin/api/requests?{query}")).await;

        assert_error(&response, StatusCode::BAD_REQUEST, "invalid_request");
    }
    assert!(h.history.filters().is_empty(), "the database was asked");
}

#[tokio::test]
async fn a_page_has_the_documented_shape() {
    let h = admin(DbMode::Up).await;
    let id = request_id();
    let next = request_id();
    h.history.set_page(HistoryPage {
        requests: vec![summary(&id)],
        next_before: Some(next.clone()),
    });

    let response = h.get("/admin/api/requests").await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(
        response.json(),
        json!({
            "requests": [{
                "id": id,
                "client_request_id": "client-1",
                "created_at": "2026-11-12T08:30:00.123Z",
                "key": {
                    "id": "00000000-0000-0000-0000-000000000000",
                    "name": "laptop",
                    "prefix": "ms_dev_7Hq2"
                },
                "requested_model": "fast",
                "provider": "gemini",
                "model": "gemini-3.5-flash-lite",
                "status": "succeeded",
                "error": null,
                "latency_ms": 1544,
                "usage": {
                    "input_tokens": 8,
                    "output_tokens": 1,
                    "total_tokens": 9,
                    "source": "provider_reported"
                },
                "cost": {
                    "usd": "0.0000049",
                    "pricing_version": "ffffffff-ffff-ffff-ffff-ffffffffffff"
                },
                "attempt_count": 2
            }],
            "next_before": next
        })
    );
}

#[tokio::test]
async fn unknown_values_are_null_never_zero() {
    let h = admin(DbMode::Up).await;
    let id = request_id();
    let failed = RequestSummary {
        status: "failed".to_owned(),
        error_kind: Some("status".to_owned()),
        error_code: Some("API_KEY_INVALID".to_owned()),
        provider_status: Some(400),
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        usage_source: "unavailable".to_owned(),
        estimated_cost_usd: None,
        pricing_version: None,
        client_request_id: None,
        ..summary(&id)
    };
    h.history.set_page(HistoryPage {
        requests: vec![failed],
        next_before: None,
    });

    let body = h.get("/admin/api/requests").await.json();

    let row = &body["requests"][0];
    assert_eq!(
        row["error"],
        json!({ "kind": "status", "code": "API_KEY_INVALID", "provider_status": 400 })
    );
    assert_eq!(
        row["usage"],
        json!({
            "input_tokens": null,
            "output_tokens": null,
            "total_tokens": null,
            "source": "unavailable"
        })
    );
    assert_eq!(row["cost"], json!(null));
    assert_eq!(row["client_request_id"], json!(null));
}

#[tokio::test]
async fn a_request_shows_its_attempts_and_price() {
    let h = admin(DbMode::Up).await;
    let id = request_id();
    h.history.add_detail(RequestDetail {
        summary: summary(&id),
        attempts: vec![
            AttemptView {
                number: 1,
                provider: "geminibadkey".to_owned(),
                model: "gemini-3.5-flash-lite".to_owned(),
                status: "failed".to_owned(),
                error_kind: Some("status".to_owned()),
                error_code: Some("API_KEY_INVALID".to_owned()),
                provider_status: Some(400),
                error_class: Some("target_problem".to_owned()),
                retry_decision: "fallback".to_owned(),
                latency_ms: 456,
            },
            AttemptView {
                number: 2,
                provider: "gemini".to_owned(),
                model: "gemini-3.5-flash-lite".to_owned(),
                status: "succeeded".to_owned(),
                error_kind: None,
                error_code: None,
                provider_status: None,
                error_class: None,
                retry_decision: "not_retried".to_owned(),
                latency_ms: 1088,
            },
        ],
        price: Some(PriceView {
            input_usd_per_million: "0.3".to_owned(),
            output_usd_per_million: "2.5".to_owned(),
            source_reference: "https://example.com/pricing".to_owned(),
        }),
    });

    let response = h.get(&format!("/admin/api/requests/{id}")).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let body = response.json();
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["attempt_count"], 2);
    assert_eq!(
        body["attempts"],
        json!([
            {
                "number": 1,
                "provider": "geminibadkey",
                "model": "gemini-3.5-flash-lite",
                "status": "failed",
                "error": {
                    "kind": "status",
                    "code": "API_KEY_INVALID",
                    "provider_status": 400,
                    "class": "target_problem"
                },
                "retry_decision": "fallback",
                "latency_ms": 456
            },
            {
                "number": 2,
                "provider": "gemini",
                "model": "gemini-3.5-flash-lite",
                "status": "succeeded",
                "error": null,
                "retry_decision": "not_retried",
                "latency_ms": 1088
            }
        ])
    );
    assert_eq!(
        body["price"],
        json!({
            "input_usd_per_million": "0.3",
            "output_usd_per_million": "2.5",
            "source": "https://example.com/pricing"
        })
    );
}

#[tokio::test]
async fn an_unknown_request_is_not_found() {
    let h = admin(DbMode::Up).await;
    let id = request_id();

    let response = h.get(&format!("/admin/api/requests/{id}")).await;

    assert_error(&response, StatusCode::NOT_FOUND, "request_not_found");
    assert_eq!(h.history.gets(), vec![id]);
}

#[tokio::test]
async fn an_id_that_cannot_exist_is_not_looked_up() {
    let h = admin(DbMode::Up).await;

    for id in [
        "nope",
        "req_123",
        "req_6f9619ff-8b86-4011-b42d-00c04fc964ff",
    ] {
        let response = h.get(&format!("/admin/api/requests/{id}")).await;

        assert_error(&response, StatusCode::NOT_FOUND, "request_not_found");
    }
    assert!(h.history.gets().is_empty());
}

#[tokio::test]
async fn a_failing_database_is_503_without_details() {
    let h = admin(DbMode::Down).await;

    for path in [
        "/admin/api/requests".to_owned(),
        format!("/admin/api/requests/{}", request_id()),
    ] {
        let response = h.get(&path).await;

        assert_error(
            &response,
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        );
        assert!(!response.body.contains("hunter2"), "{}", response.body);
        assert!(!response.body.contains(SENSITIVE_ERROR));
    }
}

#[tokio::test(start_paused = true)]
async fn a_hanging_database_is_503_at_the_deadline() {
    let h = admin(DbMode::Hang).await;

    for path in [
        "/admin/api/requests".to_owned(),
        format!("/admin/api/requests/{}", request_id()),
    ] {
        let started = tokio::time::Instant::now();
        let response = h.get(&path).await;

        assert_error(
            &response,
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        );
        assert_eq!(started.elapsed(), Duration::from_secs(1));
    }
}
