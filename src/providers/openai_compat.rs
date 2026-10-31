//! Adapter for the OpenAI-compatible chat completions API, used for Groq
//! and local Ollama (ADR-008).
//!
//! `POST {base_url}/chat/completions` with one user message. The key is
//! sent as a bearer token. Redirects, response size, and error text are
//! handled by the shared HTTP plumbing (ADR-025).

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::http;
use crate::domain::{BoxFuture, GenerateRequest, Generation, Provider, ProviderError, Usage};

pub struct OpenAiCompatible {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<SecretString>,
}

impl OpenAiCompatible {
    /// `base_url` is the API root, such as `https://api.groq.com/openai/v1`.
    pub fn new(base_url: &str, api_key: Option<SecretString>) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: http::client()?,
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
        let mut http_request = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            // `bearer_auth` marks the header sensitive, so it is redacted
            // from reqwest's own debug output.
            http_request = http_request.bearer_auth(key.expose_secret());
        }

        let (status, bytes) = http::send(http_request).await?;
        if !status.is_success() {
            return Err(ProviderError::Status {
                status: status.as_u16(),
                code: http::error_code(&bytes, &["code", "type"]),
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
    fn into_usage(self) -> Usage {
        Usage {
            input_tokens: http::token_count(self.prompt_tokens),
            output_tokens: http::token_count(self.completion_tokens),
            total_tokens: http::token_count(self.total_tokens),
        }
    }
}
