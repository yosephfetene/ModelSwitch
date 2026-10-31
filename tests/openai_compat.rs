//! Contract tests for the OpenAI-compatible adapter (Stage 1 slice 4),
//! against an in-process mock provider.

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
use modelswitch::providers::openai_compat::OpenAiCompatible;

/// Starts a mock provider; returns it with its API root (`.../v1`).
async fn start(status: StatusCode, body: impl Into<String>) -> (Mock, String) {
    let (mock, root) = Mock::start(status, body).await;
    (mock, format!("{root}/v1"))
}

fn completion(content: Value, usage: Option<Value>) -> String {
    let mut body = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
    });
    if let Some(usage) = usage {
        body["usage"] = usage;
    }
    body.to_string()
}

fn request(input: &str, max_output_tokens: Option<u32>) -> GenerateRequest {
    GenerateRequest {
        input: input.to_owned(),
        max_output_tokens,
    }
}

fn adapter(base_url: &str, key: Option<&str>) -> OpenAiCompatible {
    OpenAiCompatible::new(base_url, key.map(|key| SecretString::from(key.to_owned()))).unwrap()
}

#[tokio::test]
async fn sends_one_user_message_to_chat_completions_with_bearer_key() {
    let (mock, base) = start(StatusCode::OK, completion(json!("hi"), None)).await;

    adapter(&base, Some("gsk_test_key"))
        .generate("llama-3.1-8b-instant", &request("Hello", Some(50)))
        .await
        .unwrap();

    let received = mock.received();
    assert_eq!(received.len(), 1);
    let call = &received[0];
    assert_eq!(call.method, Method::POST);
    assert_eq!(call.path(), "/v1/chat/completions");
    assert_eq!(call.headers["authorization"], "Bearer gsk_test_key");
    assert!(
        call.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    assert_eq!(
        call.json(),
        json!({
            "model": "llama-3.1-8b-instant",
            "messages": [{ "role": "user", "content": "Hello" }],
            "max_tokens": 50,
            "stream": false
        })
    );
}

#[tokio::test]
async fn omits_authorization_and_max_tokens_when_not_given() {
    let (mock, base) = start(StatusCode::OK, completion(json!("hi"), None)).await;

    adapter(&format!("{base}/"), None)
        .generate("llama3.2", &request("Hello", None))
        .await
        .unwrap();

    let call = &mock.received()[0];
    assert_eq!(
        call.path(),
        "/v1/chat/completions",
        "trailing slash handled"
    );
    assert!(call.headers.get("authorization").is_none());
    assert!(call.json().get("max_tokens").is_none());
}

#[tokio::test]
async fn parses_output_and_reported_usage() {
    let usage = json!({ "prompt_tokens": 9, "completion_tokens": 4, "total_tokens": 13 });
    let (_, base) = start(StatusCode::OK, completion(json!("Hi there"), Some(usage))).await;

    let generation = adapter(&base, None)
        .generate("m", &request("Hello", None))
        .await
        .unwrap();

    assert_eq!(generation.output, "Hi there");
    assert_eq!(generation.usage.input_tokens, Some(9));
    assert_eq!(generation.usage.output_tokens, Some(4));
    assert_eq!(generation.usage.total_tokens, Some(13));
    assert_eq!(generation.usage.source(), UsageSource::ProviderReported);
}

#[tokio::test]
async fn missing_usage_stays_unavailable() {
    let (_, base) = start(StatusCode::OK, completion(json!("Hi"), None)).await;

    let generation = adapter(&base, None)
        .generate("m", &request("Hello", None))
        .await
        .unwrap();

    assert_eq!(generation.usage.source(), UsageSource::Unavailable);
}

#[tokio::test]
async fn error_status_keeps_code_but_never_message() {
    let body = json!({ "error": {
        "message": "Rate limit reached. Your prompt was: PROMPT-CANARY",
        "type": "tokens",
        "code": "rate_limit_exceeded"
    }});
    let (_, base) = start(StatusCode::TOO_MANY_REQUESTS, body.to_string()).await;

    let error = adapter(&base, None)
        .generate("m", &request("PROMPT-CANARY", None))
        .await
        .unwrap_err();

    assert_eq!(
        error,
        ProviderError::Status {
            status: 429,
            code: Some("rate_limit_exceeded".to_owned())
        }
    );
    assert!(!error.to_string().contains("PROMPT-CANARY"), "{error}");
}

#[tokio::test]
async fn unusable_success_bodies_are_invalid_responses() {
    for body in [
        "not json".to_owned(),
        json!({ "choices": [] }).to_string(),
        completion(json!(null), None),
        json!({ "unexpected": true }).to_string(),
    ] {
        let (_, base) = start(StatusCode::OK, body.clone()).await;

        let error = adapter(&base, None)
            .generate("m", &request("Hello", None))
            .await
            .unwrap_err();

        assert!(
            matches!(error, ProviderError::InvalidResponse(_)),
            "{body}: {error:?}"
        );
    }
}

#[tokio::test]
async fn oversized_response_is_rejected() {
    let huge = completion(json!("x".repeat(5 * 1024 * 1024)), None);
    let (_, base) = start(StatusCode::OK, huge).await;

    let error = adapter(&base, None)
        .generate("m", &request("Hello", None))
        .await
        .unwrap_err();

    assert!(
        matches!(&error, ProviderError::InvalidResponse(reason) if reason.contains("4 MiB")),
        "{error:?}"
    );
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let (elsewhere, elsewhere_base) = start(StatusCode::OK, completion(json!("hi"), None)).await;
    let (redirector, base) = start(StatusCode::TEMPORARY_REDIRECT, "").await;
    redirector.with_header("location", &format!("{elsewhere_base}/chat/completions"));

    let error = adapter(&base, Some("gsk_test_key"))
        .generate("m", &request("Hello", None))
        .await
        .unwrap_err();

    assert!(
        matches!(error, ProviderError::Status { status: 307, .. }),
        "{error:?}"
    );
    assert!(elsewhere.received().is_empty(), "the key was forwarded");
}

#[tokio::test]
async fn connection_failure_is_a_request_error() {
    let port = closed_port().await;

    let error = adapter(&format!("http://127.0.0.1:{port}/v1"), None)
        .generate("m", &request("Hello", None))
        .await
        .unwrap_err();

    assert!(matches!(error, ProviderError::Request(_)), "{error:?}");
}

/// The whole path, wired from configuration like `main`: client →
/// gateway → adapter → mock provider.
#[tokio::test]
async fn generate_end_to_end_through_configured_provider() {
    let usage = json!({ "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 });
    let (mock, base) = start(StatusCode::OK, completion(json!("Hello back"), Some(usage))).await;
    let config = Config::from_toml(
        &format!(
            r#"
            [server]
            bind = "127.0.0.1:0"
            [database]
            url_env = "DATABASE_URL"
            [providers.groq]
            adapter = "openai_compatible"
            base_url = "{base}"
            api_key_env = "GROQ_API_KEY"
            models = ["openai/gpt-oss-20b"]
            [aliases.fast]
            candidates = ["groq/openai/gpt-oss-20b"]
            "#
        ),
        |name| match name {
            "GROQ_API_KEY" => Some("gsk_from_env".to_owned()),
            _ => Some("postgres://unused".to_owned()),
        },
    )
    .unwrap();
    let keys = FakeKeyStore::new(DbMode::Up);
    let (key, _) = keys.add_key();
    let mut state = state_with_keys(FakeDatabase::new(DbMode::Up), keys);
    state.catalog = Arc::new(config.catalog());
    state.providers = Arc::new(Registry::from_config(&config.providers).unwrap());
    let app = build_router(state);

    let response = send(
        &app,
        Request::post("/v1/generate")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "model": "fast", "input": "Hello" }).to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let body = response.json();
    assert_eq!(body["output"], "Hello back");
    assert_eq!(body["provider"], "groq");
    assert_eq!(body["resolved_model"], "openai/gpt-oss-20b");
    assert_eq!(body["usage"]["total_tokens"], 7);
    assert_eq!(body["usage"]["source"], "provider_reported");
    // The gateway's client key never reaches the provider; the provider's
    // own key is sent instead.
    let call = &mock.received()[0];
    assert_eq!(call.headers["authorization"], "Bearer gsk_from_env");
    // Only the first `/` of the pin selects the provider; the rest is
    // the provider's own model ID.
    assert_eq!(call.json()["model"], "openai/gpt-oss-20b");
}
