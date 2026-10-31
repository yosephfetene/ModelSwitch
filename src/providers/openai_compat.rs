//! Adapter for the OpenAI-compatible chat completions API, used for Groq
//! and local Ollama (ADR-008).
//!
//! `POST {base_url}/chat/completions` with one user message. Defensive
//! choices (ADR-025):
//!
//! - Redirects are never followed, so the API key cannot be forwarded to
//!   a host other than the configured one.
//! - Response bodies are capped, so a misbehaving endpoint cannot exhaust
//!   memory.
//! - On an error status only the provider's short error code is kept,
//!   never its message, which can quote the prompt.
//!
//! The overall deadline is applied by the caller; dropping the future
//! cancels the HTTP request.

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::domain::{BoxFuture, GenerateRequest, Generation, Provider, ProviderError, Usage};

/// Largest response body read from a provider.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_CODE_CHARS: usize = 64;

pub struct OpenAiCompatible {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<SecretString>,
}

impl OpenAiCompatible {
    /// `base_url` is the API root, such as `https://api.groq.com/openai/v1`.
    pub fn new(base_url: &str, api_key: Option<SecretString>) -> Result<Self, reqwest::Error> {
        install_crypto_provider();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("modelswitch/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            endpoint: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            api_key,
        })
    }

    async fn call(
        &self,
        model: &str,
        request: &GenerateRequest,
    ) -> Result<Generation, ProviderError> {
        let body = ChatRequest {
            model,
            messages: [Message {
                role: "user",
                content: &request.input,
            }],
            max_tokens: request.max_output_tokens,
            stream: false,
        };
        let mut http = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            // `bearer_auth` marks the header sensitive, so it is redacted
            // from reqwest's own debug output.
            http = http.bearer_auth(key.expose_secret());
        }
        let response = http
            .send()
            .await
            .map_err(|error| ProviderError::Request(error.to_string()))?;

        let status = response.status();
        let bytes = read_limited(response).await?;
        if !status.is_success() {
            return Err(ProviderError::Status {
                status: status.as_u16(),
                code: error_code(&bytes),
            });
        }

        let parsed: ChatResponse = serde_json::from_slice(&bytes).map_err(|error| {
            ProviderError::InvalidResponse(format!("not a chat completion: {error}"))
        })?;
        let output = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| ProviderError::InvalidResponse("no message content".to_owned()))?;
        let usage = parsed.usage.map(WireUsage::into_usage).unwrap_or_default();
        Ok(Generation { output, usage })
    }
}

impl Provider for OpenAiCompatible {
    fn generate<'a>(
        &'a self,
        model: &'a str,
        request: &'a GenerateRequest,
    ) -> BoxFuture<'a, Result<Generation, ProviderError>> {
        Box::pin(self.call(model, request))
    }
}

/// reqwest is built without a bundled crypto backend; use `ring`, the one
/// sqlx already uses, so the binary carries a single TLS implementation.
/// Installing twice is harmless: later attempts are ignored.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ProviderError::Request(error.to_string()))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ProviderError::InvalidResponse(
                "response body exceeds 4 MiB".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The provider's short error identifier (`error.code`, else
/// `error.type`), if it looks like one. Anything else is dropped: free
/// text can quote the prompt and must not reach the logs.
fn error_code(body: &[u8]) -> Option<String> {
    let envelope: ErrorEnvelope = serde_json::from_slice(body).ok()?;
    let error = envelope.error?;
    let code = match error.code {
        Some(serde_json::Value::String(code)) => Some(code),
        _ => error.kind,
    }?;
    let looks_like_identifier = !code.is_empty()
        && code.chars().count() <= MAX_ERROR_CODE_CHARS
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    looks_like_identifier.then_some(code)
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [Message<'a>; 1],
    /// `max_tokens` rather than the newer `max_completion_tokens`: both
    /// Groq and Ollama accept it.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl WireUsage {
    /// Counts that do not fit stay unknown rather than being clamped
    /// into a wrong number (ADR-010).
    fn into_usage(self) -> Usage {
        let fit = |count: Option<u64>| count.and_then(|count| u32::try_from(count).ok());
        Usage {
            input_tokens: fit(self.prompt_tokens),
            output_tokens: fit(self.completion_tokens),
            total_tokens: fit(self.total_tokens),
        }
    }
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<serde_json::Value>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_prefers_code_then_type() {
        let both = br#"{"error":{"message":"x","type":"invalid_request_error","code":"rate_limit_exceeded"}}"#;
        let type_only = br#"{"error":{"message":"x","type":"invalid_request_error","code":null}}"#;

        assert_eq!(error_code(both).as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(
            error_code(type_only).as_deref(),
            Some("invalid_request_error")
        );
    }

    #[test]
    fn error_code_drops_free_text_and_garbage() {
        for body in [
            &br#"{"error":{"code":"prompt was: tell me a secret"}}"#[..],
            br#"{"error":{"code":42}}"#,
            br#"{"error":"plain string"}"#,
            br#"not json"#,
            b"",
        ] {
            assert_eq!(error_code(body), None, "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn oversized_token_counts_stay_unknown() {
        let usage = WireUsage {
            prompt_tokens: Some(u64::from(u32::MAX) + 1),
            completion_tokens: Some(7),
            total_tokens: None,
        }
        .into_usage();

        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, Some(7));
    }
}
