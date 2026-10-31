//! Contract tests for the Gemini adapter (Stage 1 slice 5), against an
//! in-process mock provider.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use secrecy::SecretString;
use serde_json::{Value, json};

use common::mock_http::{Mock, closed_port};
use common::{DbMode, FakeDatabase, FakeKeyStore, send, state_with_keys};
use modelswitch::build_router;
use modelswitch::config::Config;
use modelswitch::domain::{GenerateRequest, Provider, ProviderError, UsageSource};
use modelswitch::providers::Registry;
use modelswitch::providers::gemini::Gemini;

const KEY: &str = "AIza_test_key";

/// Starts a mock provider; returns it with its API root (`.../v1beta`).
async fn start(status: StatusCode, body: impl Into<String>) -> (Mock, String) {
    let (mock, root) = Mock::start(status, body).await;
    (mock, format!("{root}/v1beta"))
}

fn answer(parts: Value, finish_reason: &str, usage: Option<Value>) -> String {
    let mut body = json!({
        "candidates": [{
            "content": { "role": "model", "parts": parts },
            "finishReason": finish_reason,
            "index": 0
        }],
        "modelVersion": "gemini-3.8-flash"
    });
    if let Some(usage) = usage {
        body["usageMetadata"] = usage;
    }
    body.to_string()
}

fn text(value: &str) -> String {
    answer(json!([{ "text": value }]), "STOP", None)
}

fn request(input: &str, max_output_tokens: Option<u32>) -> GenerateRequest {
    GenerateRequest {
        input: input.to_owned(),
        max_output_tokens,
    }
}

fn adapter(base_url: &str) -> Gemini {
    Gemini::new(base_url, SecretString::from(KEY.to_owned())).unwrap()
}

async fn generate_with(
    status: StatusCode,
    body: impl Into<String>,
) -> Result<String, ProviderError> {
    let (_, base) = start(status, body).await;
    adapter(&base)
        .generate("gemini-3.8-flash", &request("Hello", None))
        .await
        .map(|generation| generation.output)
}

#[tokio::test]
async fn sends_one_user_turn_with_the_key_in_a_header() {
    let (mock, base) = start(StatusCode::OK, text("hi")).await;

    adapter(&base)
        .generate("gemini-3.8-flash", &request("Hello", Some(50)))
        .await
        .unwrap();

    let received = mock.received();
    assert_eq!(received.len(), 1);
    let call = &received[0];
    assert_eq!(call.method, Method::POST);
    assert_eq!(
        call.path(),
        "/v1beta/models/gemini-3.8-flash:generateContent"
    );
    assert_eq!(call.headers["x-goog-api-key"], KEY);
    assert_eq!(call.uri.query(), None, "the key must never be in the URL");
    assert!(call.headers.get("authorization").is_none());
    assert_eq!(
        call.json(),
        json!({
            "contents": [{ "role": "user", "parts": [{ "text": "Hello" }] }],
            "generationConfig": { "maxOutputTokens": 50 }
        })
    );
}

#[tokio::test]
async fn omits_generation_config_when_no_limit_is_given() {
    let (mock, base) = start(StatusCode::OK, text("hi")).await;

    adapter(&format!("{base}/"))
        .generate("gemini-3.8-flash", &request("Hello", None))
        .await
        .unwrap();

    let call = &mock.received()[0];
    assert_eq!(
        call.path(),
        "/v1beta/models/gemini-3.8-flash:generateContent",
        "trailing slash handled"
    );
    assert!(call.json().get("generationConfig").is_none());
}

#[tokio::test]
async fn joins_answer_parts_and_leaves_out_thoughts() {
    let parts = json!([
        { "text": "THOUGHT-CANARY: the user wants a greeting", "thought": true },
        { "text": "Hello " },
        { "text": "there" }
    ]);
    let usage = json!({
        "promptTokenCount": 9,
        "candidatesTokenCount": 2,
        "thoughtsTokenCount": 30,
        "totalTokenCount": 41
    });
    let (_, base) = start(StatusCode::OK, answer(parts, "STOP", Some(usage))).await;

    let generation = adapter(&base)
        .generate("gemini-3.8-flash", &request("Hello", None))
        .await
        .unwrap();

    assert_eq!(generation.output, "Hello there");
    assert_eq!(generation.usage.input_tokens, Some(9));
    assert_eq!(
        generation.usage.output_tokens,
        Some(32),
        "answer + thinking"
    );
    assert_eq!(generation.usage.total_tokens, Some(41));
    assert_eq!(generation.usage.source(), UsageSource::ProviderReported);
}

#[tokio::test]
async fn missing_usage_stays_unavailable() {
    let (_, base) = start(StatusCode::OK, text("Hi")).await;

    let generation = adapter(&base)
        .generate("gemini-3.8-flash", &request("Hello", None))
        .await
        .unwrap();

    assert_eq!(generation.usage.source(), UsageSource::Unavailable);
}

#[tokio::test]
async fn error_status_keeps_code_but_never_message() {
    let older = json!({ "error": {
        "code": 429,
        "message": "Quota exceeded. Your prompt was: PROMPT-CANARY",
        "status": "RESOURCE_EXHAUSTED"
    }});
    let newer = json!({ "error": {
        "code": "rate_limit_exceeded",
        "message": "Your prompt was: PROMPT-CANARY"
    }});

    for (body, code) in [
        (older, "RESOURCE_EXHAUSTED"),
        (newer, "rate_limit_exceeded"),
    ] {
        let error = generate_with(StatusCode::TOO_MANY_REQUESTS, body.to_string())
            .await
            .unwrap_err();

        assert_eq!(
            error,
            ProviderError::Status {
                status: 429,
                code: Some(code.to_owned())
            }
        );
        assert!(!error.to_string().contains("PROMPT-CANARY"), "{error}");
    }
}

#[tokio::test]
async fn refused_prompt_is_blocked_with_its_reason() {
    let body = json!({
        "promptFeedback": { "blockReason": "PROHIBITED_CONTENT" },
        "usageMetadata": { "promptTokenCount": 7, "totalTokenCount": 7 }
    });

    let error = generate_with(StatusCode::OK, body.to_string())
        .await
        .unwrap_err();

    assert_eq!(
        error,
        ProviderError::Blocked {
            reason: Some("PROHIBITED_CONTENT".to_owned())
        }
    );
}

#[tokio::test]
async fn refused_answer_is_blocked_with_its_reason() {
    let refused = json!({ "candidates": [{ "finishReason": "SAFETY", "index": 0 }] });
    let odd_reason = json!({ "candidates": [{ "finishReason": "said no: PROMPT-CANARY" }] });

    assert_eq!(
        generate_with(StatusCode::OK, refused.to_string()).await,
        Err(ProviderError::Blocked {
            reason: Some("SAFETY".to_owned())
        })
    );
    assert_eq!(
        generate_with(StatusCode::OK, odd_reason.to_string()).await,
        Err(ProviderError::Blocked { reason: None }),
        "free text is never kept as a reason"
    );
}

#[tokio::test]
async fn token_limit_before_any_answer_is_an_empty_answer_with_usage() {
    let parts = json!([{ "text": "thinking...", "thought": true }]);
    let usage = json!({ "promptTokenCount": 5, "thoughtsTokenCount": 20, "totalTokenCount": 25 });
    let (_, base) = start(StatusCode::OK, answer(parts, "MAX_TOKENS", Some(usage))).await;

    let generation = adapter(&base)
        .generate("gemini-3.8-flash", &request("Hello", Some(20)))
        .await
        .unwrap();

    assert_eq!(generation.output, "");
    assert_eq!(generation.usage.output_tokens, Some(20));
}

#[tokio::test]
async fn unusable_success_bodies_are_invalid_responses() {
    for body in [
        "not json".to_owned(),
        json!({}).to_string(),
        json!({ "candidates": [] }).to_string(),
        json!({ "candidates": "nope" }).to_string(),
        answer(json!([]), "STOP", None),
        json!({ "candidates": [{ "content": { "parts": [] } }] }).to_string(),
    ] {
        let error = generate_with(StatusCode::OK, body.clone())
            .await
            .unwrap_err();

        assert!(
            matches!(error, ProviderError::InvalidResponse(_)),
            "{body}: {error:?}"
        );
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let (elsewhere, elsewhere_base) = start(StatusCode::OK, text("hi")).await;
    let (redirector, base) = start(StatusCode::TEMPORARY_REDIRECT, "").await;
    redirector.with_header(
        "location",
        &format!("{elsewhere_base}/models/gemini-3.8-flash:generateContent"),
    );

    let error = adapter(&base)
        .generate("gemini-3.8-flash", &request("Hello", None))
        .await
        .unwrap_err();

    assert!(
        matches!(error, ProviderError::Status { status: 307, .. }),
        "{error:?}"
    );
    assert!(elsewhere.received().is_empty(), "the key was forwarded");
}

#[tokio::test]
async fn connection_failure_never_mentions_the_key_or_url() {
    let port = closed_port().await;

    let error = adapter(&format!("http://127.0.0.1:{port}/v1beta"))
        .generate("gemini-3.8-flash", &request("Hello", None))
        .await
        .unwrap_err();

    assert!(matches!(error, ProviderError::Request(_)), "{error:?}");
    let message = error.to_string();
    assert!(!message.contains(KEY), "{message}");
    assert!(!message.contains("generateContent"), "{message}");
}

/// The gateway wired from configuration like `main`, with a valid client
/// key.
fn gateway(base: &str) -> (axum::Router, String) {
    let config = Config::from_toml(
        &format!(
            r#"
            [server]
            bind = "127.0.0.1:0"
            [database]
            url_env = "DATABASE_URL"
            [providers.gemini]
            adapter = "gemini"
            base_url = "{base}"
            api_key_env = "GEMINI_API_KEY"
            models = ["gemini-3.8-flash"]
            "#
        ),
        |name| match name {
            "GEMINI_API_KEY" => Some("AIza_from_env".to_owned()),
            _ => Some("postgres://unused".to_owned()),
        },
    )
    .unwrap();
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let mut state = state_with_keys(FakeDatabase::new(DbMode::Up), keys);
    state.catalog = Arc::new(config.catalog());
    state.providers = Arc::new(Registry::from_config(&config.providers).unwrap());
    (build_router(state), key)
}

async fn generate_through(base: &str, input: &str) -> common::TestResponse {
    let (app, key) = gateway(base);
    send(
        &app,
        Request::post("/v1/generate")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "model": "gemini/gemini-3.8-flash", "input": input }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

/// Client → gateway → Gemini adapter → mock provider.
#[tokio::test]
async fn generate_end_to_end_through_configured_gemini() {
    let usage = json!({ "promptTokenCount": 5, "candidatesTokenCount": 2, "totalTokenCount": 7 });
    let (mock, base) = start(
        StatusCode::OK,
        answer(json!([{ "text": "Hello back" }]), "STOP", Some(usage)),
    )
    .await;

    let response = generate_through(&base, "Hello").await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let body = response.json();
    assert_eq!(body["output"], "Hello back");
    assert_eq!(body["provider"], "gemini");
    assert_eq!(body["resolved_model"], "gemini-3.8-flash");
    assert_eq!(body["usage"]["output_tokens"], 2);
    assert_eq!(body["usage"]["source"], "provider_reported");
    let call = &mock.received()[0];
    assert_eq!(call.headers["x-goog-api-key"], "AIza_from_env");
    assert!(
        call.headers.get("authorization").is_none(),
        "client key leaked"
    );
}

/// A refusal reaches the client as the generic provider error: Google's
/// reason is for the logs, not the response (ADR-019).
#[tokio::test]
async fn blocked_request_is_a_generic_provider_error() {
    let body = json!({ "promptFeedback": { "blockReason": "SAFETY" } });
    let (_, base) = start(StatusCode::OK, body.to_string()).await;

    let response = generate_through(&base, "Hello").await;

    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    assert_eq!(response.json()["error"]["code"], "provider_error");
    assert!(!response.body.contains("SAFETY"), "{}", response.body);
}
