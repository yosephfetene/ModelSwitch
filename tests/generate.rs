//! `POST /v1/generate` with fake providers (Stage 1 slice 3 spec). Every
//! test installs the global log capture first (see `CapturedLogs::global`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use common::{
    CapturedLogs, DbMode, FakeDatabase, FakeKeyStore, FakeProvider, ProviderMode, TestResponse,
    send, state_with_keys,
};
use modelswitch::build_router;
use modelswitch::config::Config;
use modelswitch::domain::{Target, Usage};
use modelswitch::providers::{ProviderSlot, Registry};
use modelswitch::routing::Catalog;

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

/// The application with two fake providers: `primary` serves `model-a`
/// and `model-b`, `secondary` serves `model-c`. Alias `default` lists
/// `primary/model-a` then `secondary/model-c`.
struct Harness {
    app: Router,
    key: String,
    primary: FakeProvider,
    secondary: FakeProvider,
}

fn reported(input: u32, output: u32) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
    }
}

fn harness_with(primary: FakeProvider) -> Harness {
    CapturedLogs::global();
    let secondary = FakeProvider::replying("from secondary", Usage::unavailable());
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let mut state = state_with_keys(FakeDatabase::new(DbMode::Up), keys);
    let target = |value: &str| Target::parse(value).unwrap();
    state.catalog = Arc::new(Catalog::new(
        [
            (
                "primary".to_owned(),
                vec!["model-a".to_owned(), "model-b".to_owned()],
            ),
            ("secondary".to_owned(), vec!["model-c".to_owned()]),
        ],
        [(
            "default".to_owned(),
            vec![target("primary/model-a"), target("secondary/model-c")],
        )],
    ));
    let slot = |provider: &FakeProvider| ProviderSlot {
        provider: Arc::new(provider.clone()),
        timeout: PROVIDER_TIMEOUT,
    };
    state.providers = Arc::new(Registry::new([
        ("primary".to_owned(), slot(&primary)),
        ("secondary".to_owned(), slot(&secondary)),
    ]));
    Harness {
        app: build_router(state),
        key,
        primary,
        secondary,
    }
}

fn harness() -> Harness {
    harness_with(FakeProvider::replying("generated text", reported(12, 3)))
}

impl Harness {
    async fn post_raw(&self, content_type: Option<&str>, body: impl Into<Body>) -> TestResponse {
        let mut request =
            Request::post("/v1/generate").header("authorization", format!("Bearer {}", self.key));
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        send(&self.app, request.body(body.into()).unwrap()).await
    }

    async fn post(&self, body: Value) -> TestResponse {
        self.post_raw(Some("application/json"), body.to_string())
            .await
    }

    fn total_calls(&self) -> usize {
        self.primary.calls().len() + self.secondary.calls().len()
    }
}

fn assert_error(response: &TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.json()["error"]["code"], code, "{}", response.body);
}

// 1
#[tokio::test]
async fn alias_request_succeeds_with_documented_response() {
    let h = harness();

    let response = h
        .post(json!({ "model": "default", "input": "Hello" }))
        .await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let body = response.json();
    assert_eq!(body["id"], response.header("x-request-id").unwrap());
    assert_eq!(body["model"], "default");
    assert_eq!(body["provider"], "primary");
    assert_eq!(body["resolved_model"], "model-a");
    assert_eq!(body["output"], "generated text");
    assert_eq!(
        body["usage"],
        json!({ "input_tokens": 12, "output_tokens": 3, "total_tokens": 15, "source": "provider_reported" })
    );
    assert!(body["latency_ms"].is_u64(), "{body}");
    let calls = h.primary.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "model-a");
    assert_eq!(calls[0].1.input, "Hello");
}

// 2
#[tokio::test]
async fn pin_calls_exactly_that_provider_and_model() {
    let h = harness();

    let response = h
        .post(json!({ "model": "secondary/model-c", "input": "Hi" }))
        .await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.json()["provider"], "secondary");
    assert_eq!(response.json()["resolved_model"], "model-c");
    assert_eq!(h.secondary.calls().len(), 1);
    assert!(h.primary.calls().is_empty());
}

// 3, 4
#[tokio::test]
async fn unknown_models_are_422_without_provider_call() {
    let h = harness();

    for model in ["no-such-alias", "primary/model-z", "nowhere/model-a"] {
        let response = h.post(json!({ "model": model, "input": "Hi" })).await;

        assert_error(&response, StatusCode::UNPROCESSABLE_ENTITY, "unknown_model");
        assert!(!response.body.contains(model), "echoed {model:?}");
    }
    assert_eq!(h.total_calls(), 0);
}

// 5
#[tokio::test]
async fn bad_keys_are_401_without_provider_call() {
    let h = harness();
    let body = json!({ "model": "default", "input": "Hi" }).to_string();

    for authorization in [None, Some("Bearer hello")] {
        let mut request = Request::post("/v1/generate").header("content-type", "application/json");
        if let Some(value) = authorization {
            request = request.header("authorization", value);
        }
        let response = send(&h.app, request.body(Body::from(body.clone())).unwrap()).await;

        assert_error(&response, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    assert_eq!(h.total_calls(), 0);
}

// 6
#[tokio::test]
async fn invalid_bodies_are_400_without_provider_call() {
    let h = harness();

    for body in [
        "not json".to_owned(),
        "{}".to_owned(),
        "[]".to_owned(),
        json!({ "model": "default" }).to_string(),
        json!({ "model": "default", "input": "   " }).to_string(),
        json!({ "model": "", "input": "Hi" }).to_string(),
        json!({ "model": "x".repeat(257), "input": "Hi" }).to_string(),
        json!({ "model": "default", "input": 42 }).to_string(),
        json!({ "model": "default", "input": "Hi", "metadata": {} }).to_string(),
        json!({ "model": "default", "input": "Hi", "routing": { "policy": "x" } }).to_string(),
    ] {
        let response = h.post_raw(Some("application/json"), body.clone()).await;

        assert_error(&response, StatusCode::BAD_REQUEST, "invalid_request");
    }
    assert_eq!(h.total_calls(), 0);
}

// 7
#[tokio::test]
async fn non_json_content_type_is_400() {
    let h = harness();
    let body = json!({ "model": "default", "input": "Hi" }).to_string();

    for content_type in [Some("text/plain"), None] {
        let response = h.post_raw(content_type, body.clone()).await;

        assert_error(&response, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let with_charset = h
        .post_raw(Some("application/json; charset=utf-8"), body)
        .await;
    assert_eq!(with_charset.status, StatusCode::OK);
}

// 8
#[tokio::test]
async fn oversized_body_is_json_413() {
    let h = harness();
    let input = "x".repeat(modelswitch::api::generate::MAX_BODY_BYTES);

    let response = h.post(json!({ "model": "default", "input": input })).await;

    assert_error(
        &response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );
    assert_eq!(h.total_calls(), 0);
}

// 9
#[tokio::test]
async fn max_output_tokens_is_validated_and_passed_on() {
    let h = harness();

    for tokens in [0, 100_001] {
        let response = h
            .post(json!({ "model": "default", "input": "Hi", "max_output_tokens": tokens }))
            .await;
        assert_error(&response, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let ok = h
        .post(json!({ "model": "default", "input": "Hi", "max_output_tokens": 500 }))
        .await;

    assert_eq!(ok.status, StatusCode::OK);
    let calls = h.primary.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.max_output_tokens, Some(500));
}

// 10 (resolution itself is unit-tested in `routing`)
#[tokio::test]
async fn fallback_flag_is_accepted() {
    let h = harness();

    let response = h
        .post(json!({ "model": "default", "input": "Hi", "routing": { "fallback": false } }))
        .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["provider"], "primary");
}

// 11
#[tokio::test]
async fn provider_failure_is_502_without_provider_text() {
    let h = harness_with(FakeProvider::new(ProviderMode::Fail(
        "upstream said: {\"error\":\"internal secret sauce\"}".to_owned(),
    )));

    let response = h.post(json!({ "model": "default", "input": "Hi" })).await;

    assert_error(&response, StatusCode::BAD_GATEWAY, "provider_error");
    assert!(!response.body.contains("secret sauce"), "{}", response.body);
}

// 12
#[tokio::test(start_paused = true)]
async fn provider_hang_is_504_within_deadline() {
    let h = harness_with(FakeProvider::new(ProviderMode::Hang));

    let response = tokio::time::timeout(
        PROVIDER_TIMEOUT * 2,
        h.post(json!({ "model": "default", "input": "Hi" })),
    )
    .await
    .expect("generate did not answer within twice the provider deadline");

    assert_error(&response, StatusCode::GATEWAY_TIMEOUT, "upstream_timeout");
}

// 13
#[tokio::test]
async fn missing_usage_stays_null() {
    let h = harness_with(FakeProvider::replying("text", Usage::unavailable()));

    let response = h.post(json!({ "model": "default", "input": "Hi" })).await;

    assert_eq!(
        response.json()["usage"],
        json!({ "input_tokens": null, "output_tokens": null, "total_tokens": null, "source": "unavailable" })
    );
}

// 15, wired from configuration exactly like `main`
#[tokio::test]
async fn echo_provider_from_config_answers_end_to_end() {
    CapturedLogs::global();
    let config = Config::from_toml(
        r#"
        [server]
        bind = "127.0.0.1:0"
        [database]
        url_env = "DATABASE_URL"
        [providers.echo]
        adapter = "echo"
        models = ["echo-1"]
        [aliases.default]
        candidates = ["echo/echo-1"]
        "#,
        |_| Some("postgres://unused".to_owned()),
    )
    .unwrap();
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let mut state = state_with_keys(FakeDatabase::new(DbMode::Up), keys);
    state.catalog = Arc::new(config.catalog());
    state.providers = Arc::new(Registry::from_config(&config.providers).unwrap());
    let app = build_router(state);

    let request = Request::post("/v1/generate")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "model": "default", "input": "ping" }).to_string(),
        ))
        .unwrap();
    let response = send(&app, request).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.json()["output"], "ping");
    assert_eq!(response.json()["provider"], "echo");
    assert_eq!(response.json()["usage"]["source"], "unavailable");
}

// ADR-020: a known key, prompt, and output never appear in the logs.
#[tokio::test]
async fn logs_never_contain_key_prompt_or_output() {
    let h = harness_with(FakeProvider::replying(
        "OUTPUT-CANARY-7f3a",
        Usage::unavailable(),
    ));

    let response = h
        .post(json!({ "model": "default", "input": "PROMPT-CANARY-9c1e" }))
        .await;

    assert_eq!(response.status, StatusCode::OK);
    let all = CapturedLogs::global().text();
    let id = response.header("x-request-id").unwrap();
    assert!(
        all.lines()
            .any(|line| line.contains(id) && line.contains("generate: succeeded")),
        "success not logged for {id}:\n{all}"
    );
    for secret in [
        "PROMPT-CANARY-9c1e",
        "OUTPUT-CANARY-7f3a",
        h.key.strip_prefix("ms_dev_").unwrap(),
    ] {
        assert!(!all.contains(secret), "{secret:?} leaked into logs");
    }
}
