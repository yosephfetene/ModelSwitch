//! Stage 4 exit (ROADMAP): dashboard values reconcile with stored records.
//!
//! Real requests go through `POST /v1/generate` into PostgreSQL: a first-
//! try answer, a retry, a fallback after a rejected key, a pin, and a
//! request where every try fails. Then the dashboard's API is read back
//! and held against three sources that must agree with it: what each
//! client was told, what the providers were asked, and the rows in the
//! database. Costs are also checked against prices worked out by hand.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;

use common::{DB_CHECK_TIMEOUT, FakeProvider, ProviderMode, REQUEST_LIMITS, TestResponse, send};
use modelswitch::api::admin::AdminState;
use modelswitch::cli::{keys, pricing};
use modelswitch::config::AdminConfig;
use modelswitch::domain::{Generation, ProviderError, Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;
use modelswitch::storage::PgDatabaseCheck;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::history::PgHistoryStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::{AppState, build_admin_router, build_router};

const ADMIN_TOKEN: &str = "admin-token-for-tests-0123456789abcdef";
const ADMIN_HOST: &str = "127.0.0.1:9091";
const PROMPT: &str = "PROMPT-CANARY-reconcile";
const OUTPUT: &str = "OUTPUT-CANARY-reconcile";

fn answer(input: u32, output: u32) -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: OUTPUT.to_owned(),
        usage: Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            total_tokens: Some(input + output),
        },
    })
}

fn unreported() -> ProviderMode {
    ProviderMode::Reply(Generation {
        output: OUTPUT.to_owned(),
        usage: Usage::default(),
    })
}

fn status(status: u16, code: &str) -> ProviderMode {
    ProviderMode::Error(ProviderError::Status {
        status,
        code: Some(code.to_owned()),
    })
}

fn catalog() -> Catalog {
    let target = |value: &str| Target::parse(value).unwrap();
    Catalog::new(
        [
            ("primary".to_owned(), vec!["a".to_owned()]),
            ("backup".to_owned(), vec!["b".to_owned()]),
            ("broken".to_owned(), vec!["x".to_owned()]),
        ],
        [
            (
                "fast".to_owned(),
                vec![target("primary/a"), target("backup/b")],
            ),
            ("allfail".to_owned(), vec![target("broken/x")]),
        ],
    )
}

struct Stack {
    gateway: Router,
    admin: Router,
    key: String,
    primary: FakeProvider,
    backup: FakeProvider,
    broken: FakeProvider,
}

async fn stack(pool: &PgPool) -> Stack {
    let mut out = Vec::new();
    keys::create(pool, "reconcile", Default::default(), &mut out)
        .await
        .unwrap();
    let key = String::from_utf8(out)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_owned();

    // primary/a is priced; backup/b is not, so its cost stays unknown.
    pricing::set(
        pool,
        &catalog(),
        pricing::SetPrice {
            provider: "primary",
            model: "a",
            input_per_million: "0.5",
            output_per_million: "1.5",
            source: "https://example.com/pricing",
        },
        &mut Vec::new(),
    )
    .await
    .unwrap();

    // Requests run one after another, so each provider's script is the
    // story of every call it gets, in order.
    let primary = FakeProvider::scripted(
        [
            answer(100, 20),                // 1: answers at once
            status(503, "overloaded"),      // 2: transient, retried...
            answer(10, 5),                  //    ...then answers
            status(401, "invalid_api_key"), // 3: rejected key, falls back
        ],
        status(500, "unexpected"),
    );
    let backup = FakeProvider::new(unreported()); // 3 and 4 answer here
    let broken = FakeProvider::new(status(500, "internal")); // 5: every try fails

    let slot = |provider: &FakeProvider| ProviderSlot {
        provider: Arc::new(provider.clone()),
        timeout: Duration::from_secs(5),
    };
    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: common::REAL_DATABASE_TIMEOUT,
        budgets: common::no_budgets(),
        records: Arc::new(PgRecordStore::new(pool.clone())),
        accounting_timeout: common::REAL_DATABASE_TIMEOUT,
        limits: REQUEST_LIMITS,
        metrics: Default::default(),
        rate_limiter: common::generous_rate_limiter(),
        concurrency: common::generous_concurrency(),
        catalog: Arc::new(catalog()),
        providers: Arc::new(Registry::new([
            ("primary".to_owned(), slot(&primary)),
            ("backup".to_owned(), slot(&backup)),
            ("broken".to_owned(), slot(&broken)),
        ])),
        adaptive: Default::default(),
    };

    let config = AdminConfig {
        bind: ADMIN_HOST.parse().unwrap(),
        token: SecretString::from(ADMIN_TOKEN),
        session_ttl: Duration::from_secs(3600),
        database_timeout: common::REAL_DATABASE_TIMEOUT,
        dashboard_dir: None,
    };
    let admin = build_admin_router(AdminState::new(
        &config,
        Default::default(),
        Arc::new(PgHistoryStore::new(pool.clone())),
    ));

    Stack {
        gateway: build_router(state),
        admin,
        key,
        primary,
        backup,
        broken,
    }
}

/// What one client was told.
struct Sent {
    id: String,
    status: StatusCode,
    provider: Option<String>,
    attempts: i64,
}

async fn generate(stack: &Stack, model: &str, client_id: &str) -> Sent {
    let response = send(
        &stack.gateway,
        Request::post("/v1/generate")
            .header("authorization", format!("Bearer {}", stack.key))
            .header("content-type", "application/json")
            .header("x-client-request-id", client_id)
            .body(Body::from(
                json!({ "model": model, "input": PROMPT }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    let body = response.json();
    let id = body["id"]
        .as_str()
        .or(body["request_id"].as_str())
        .unwrap()
        .to_owned();
    Sent {
        id,
        status: response.status,
        provider: body["provider"].as_str().map(str::to_owned),
        attempts: response
            .header("x-modelswitch-attempts")
            .unwrap()
            .parse()
            .unwrap(),
    }
}

async fn admin_get(stack: &Stack, cookie: &str, path: &str) -> TestResponse {
    let response = send(
        &stack.admin,
        Request::get(path)
            .header("host", ADMIN_HOST)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{path}: {}", response.body);
    assert!(!response.body.contains(PROMPT), "{path} leaked the prompt");
    assert!(!response.body.contains(OUTPUT), "{path} leaked the output");
    assert!(!response.body.contains(&stack.key), "{path} leaked the key");
    response
}

async fn log_in(stack: &Stack) -> String {
    let response = send(
        &stack.admin,
        Request::post("/admin/api/login")
            .header("host", ADMIN_HOST)
            .header("origin", format!("http://{ADMIN_HOST}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "token": ADMIN_TOKEN }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status, StatusCode::NO_CONTENT);
    response
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[derive(sqlx::FromRow)]
struct RecordRow {
    id: String,
    client_request_id: Option<String>,
    requested_model: String,
    selected_provider: String,
    selected_model: String,
    status: String,
    latency_ms: i64,
    reported_input_tokens: Option<i64>,
    reported_output_tokens: Option<i64>,
    reported_total_tokens: Option<i64>,
    usage_source: String,
    cost: Option<String>,
    attempts: i64,
}

#[derive(sqlx::FromRow, Debug, PartialEq)]
struct AttemptRow {
    attempt_number: i32,
    provider: String,
    model: String,
    status: String,
    error_code: Option<String>,
    provider_status: Option<i32>,
    error_class: Option<String>,
    retry_decision: String,
    latency_ms: i64,
}

fn attempt_from_api(value: &Value) -> AttemptRow {
    let error = &value["error"];
    AttemptRow {
        attempt_number: value["number"].as_i64().unwrap() as i32,
        provider: value["provider"].as_str().unwrap().to_owned(),
        model: value["model"].as_str().unwrap().to_owned(),
        status: value["status"].as_str().unwrap().to_owned(),
        error_code: error["code"].as_str().map(str::to_owned),
        provider_status: error["provider_status"].as_i64().map(|s| s as i32),
        error_class: error["class"].as_str().map(str::to_owned),
        retry_decision: value["retry_decision"].as_str().unwrap().to_owned(),
        latency_ms: value["latency_ms"].as_i64().unwrap(),
    }
}

#[sqlx::test]
async fn dashboard_values_reconcile_with_stored_records(pool: PgPool) {
    let stack = stack(&pool).await;

    // What clients sent, oldest first.
    let sent = [
        generate(&stack, "fast", "c1-first-try").await,
        generate(&stack, "fast", "c2-retry").await,
        generate(&stack, "fast", "c3-fallback").await,
        generate(&stack, "backup/b", "c4-pin").await,
        generate(&stack, "allfail", "c5-all-fail").await,
    ];

    // --- The clients were told what the scripts say. ---
    let told: Vec<_> = sent
        .iter()
        .map(|s| (s.status.as_u16(), s.provider.as_deref(), s.attempts))
        .collect();
    assert_eq!(
        told,
        [
            (200, Some("primary"), 1),
            (200, Some("primary"), 2),
            (200, Some("backup"), 2),
            (200, Some("backup"), 1),
            (502, None, 2),
        ]
    );
    // Every attempt the dashboard will show is a call a provider received.
    let calls =
        stack.primary.calls().len() + stack.backup.calls().len() + stack.broken.calls().len();
    assert_eq!(calls as i64, sent.iter().map(|s| s.attempts).sum::<i64>());

    // --- The dashboard lists them all, newest first, across pages. ---
    let cookie = log_in(&stack).await;
    let mut listed: Vec<Value> = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let path = match &before {
            Some(id) => format!("/admin/api/requests?limit=2&before={id}"),
            None => "/admin/api/requests?limit=2".to_owned(),
        };
        let page = admin_get(&stack, &cookie, &path).await.json();
        listed.extend(page["requests"].as_array().unwrap().iter().cloned());
        match page["next_before"].as_str() {
            Some(id) => before = Some(id.to_owned()),
            None => break,
        }
    }
    let listed_ids: Vec<&str> = listed.iter().map(|r| r["id"].as_str().unwrap()).collect();
    let sent_ids: Vec<&str> = sent.iter().rev().map(|s| s.id.as_str()).collect();
    assert_eq!(listed_ids, sent_ids, "every request once, newest first");

    // --- Each listed value matches its row in the database. ---
    let rows: HashMap<String, RecordRow> = sqlx::query_as::<_, RecordRow>(
        "SELECT r.id, r.client_request_id, r.requested_model, r.selected_provider, \
                r.selected_model, r.status, r.latency_ms, r.reported_input_tokens, \
                r.reported_output_tokens, r.reported_total_tokens, r.usage_source, \
                trim_scale(r.estimated_cost)::text AS cost, \
                (SELECT count(*) FROM request_attempts a WHERE a.request_id = r.id) AS attempts \
         FROM request_records r",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| (row.id.clone(), row))
    .collect();
    assert_eq!(rows.len(), sent.len());

    for (api, sent) in listed.iter().zip(sent.iter().rev()) {
        let row = &rows[&sent.id];
        let id = &sent.id;
        assert_eq!(
            api["client_request_id"].as_str(),
            row.client_request_id.as_deref(),
            "{id}"
        );
        assert_eq!(api["requested_model"], row.requested_model, "{id}");
        assert_eq!(api["provider"], row.selected_provider, "{id}");
        assert_eq!(api["model"], row.selected_model, "{id}");
        assert_eq!(api["status"], row.status, "{id}");
        assert_eq!(api["latency_ms"], row.latency_ms, "{id}");
        assert_eq!(
            api["usage"]["input_tokens"].as_i64(),
            row.reported_input_tokens,
            "{id}"
        );
        assert_eq!(
            api["usage"]["output_tokens"].as_i64(),
            row.reported_output_tokens,
            "{id}"
        );
        assert_eq!(
            api["usage"]["total_tokens"].as_i64(),
            row.reported_total_tokens,
            "{id}"
        );
        assert_eq!(api["usage"]["source"], row.usage_source, "{id}");
        assert_eq!(api["cost"]["usd"].as_str(), row.cost.as_deref(), "{id}");
        assert_eq!(api["attempt_count"], row.attempts, "{id}");
        // ...and with what the client was told.
        assert_eq!(api["attempt_count"], sent.attempts, "{id}");
        if let Some(provider) = &sent.provider {
            assert_eq!(api["provider"], provider.as_str(), "{id}");
        }
    }

    // --- Costs are the hand-worked prices, exact to the last digit. ---
    // 100 in × $0.5/M + 20 out × $1.5/M = $0.00008
    // 10 in × $0.5/M + 5 out × $1.5/M = $0.0000125
    let costs: Vec<Option<&str>> = sent
        .iter()
        .map(|s| listed.iter().find(|r| r["id"] == s.id.as_str()).unwrap()["cost"]["usd"].as_str())
        .collect();
    assert_eq!(
        costs,
        [Some("0.00008"), Some("0.0000125"), None, None, None]
    );

    // --- Each detail's attempt chain is the stored chain, in order. ---
    let mut chains = Vec::new();
    for sent in &sent {
        let detail = admin_get(&stack, &cookie, &format!("/admin/api/requests/{}", sent.id))
            .await
            .json();
        let from_api: Vec<AttemptRow> = detail["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .map(attempt_from_api)
            .collect();
        let stored: Vec<AttemptRow> = sqlx::query_as(
            "SELECT attempt_number, provider, model, status, error_code, provider_status, \
                    error_class, retry_decision, latency_ms \
             FROM request_attempts WHERE request_id = $1 ORDER BY attempt_number",
        )
        .bind(&sent.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(from_api, stored, "{}", sent.id);

        // The price shown is the one the cost used.
        if detail["cost"].is_null() {
            assert!(detail["price"].is_null(), "{}", sent.id);
        } else {
            assert_eq!(
                detail["price"],
                json!({
                    "input_usd_per_million": "0.5",
                    "output_usd_per_million": "1.5",
                    "source": "https://example.com/pricing",
                })
            );
        }

        chains.push(
            from_api
                .iter()
                .map(|a| {
                    format!(
                        "{}:{}:{}:{}",
                        a.provider,
                        a.status,
                        a.error_class.as_deref().unwrap_or("-"),
                        a.retry_decision
                    )
                })
                .collect::<Vec<_>>(),
        );
    }

    // --- The chains tell the routing story the scripts wrote. ---
    assert_eq!(
        chains,
        [
            vec!["primary:succeeded:-:not_retried"],
            vec![
                "primary:failed:transient:retry",
                "primary:succeeded:-:not_retried"
            ],
            vec![
                "primary:failed:target_problem:fallback",
                "backup:succeeded:-:not_retried",
            ],
            vec!["backup:succeeded:-:not_retried"],
            vec![
                "broken:failed:transient:retry",
                "broken:failed:transient:exhausted"
            ],
        ]
    );
}

#[sqlx::test]
async fn filters_agree_with_the_database(pool: PgPool) {
    let stack = stack(&pool).await;
    for (model, client) in [
        ("fast", "f1"),
        ("fast", "f2"),
        ("fast", "f3"),
        ("backup/b", "f4"),
        ("allfail", "f5"),
    ] {
        generate(&stack, model, client).await;
    }
    let cookie = log_in(&stack).await;

    for (query, sql) in [
        (
            "status=failed",
            "SELECT id FROM request_records WHERE status = 'failed'",
        ),
        (
            "status=succeeded",
            "SELECT id FROM request_records WHERE status = 'succeeded'",
        ),
        (
            "provider=backup",
            "SELECT id FROM request_records WHERE selected_provider = 'backup'",
        ),
        (
            "provider=primary",
            "SELECT id FROM request_records WHERE selected_provider = 'primary'",
        ),
        (
            "provider=nobody",
            "SELECT id FROM request_records WHERE false",
        ),
    ] {
        let page = admin_get(&stack, &cookie, &format!("/admin/api/requests?{query}"))
            .await
            .json();
        let mut from_api: Vec<String> = page["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect();
        let mut stored: Vec<String> = sqlx::query_scalar(sql).fetch_all(&pool).await.unwrap();
        from_api.sort();
        stored.sort();
        assert_eq!(from_api, stored, "{query}");
    }
}
