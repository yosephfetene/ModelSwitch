//! Adapter for the Google Gemini API (ADR-008, ADR-026).
//!
//! `POST {base_url}/models/{model}:generateContent` with one user turn.
//!
//! - The key goes only in the `x-goog-api-key` header, never in the URL
//!   (Google also accepts `?key=`, but URLs end up in logs).
//! - The model name is part of the URL path, so it is percent-encoded
//!   here, on top of the configuration allowing only plain identifiers.
//! - Parts marked as `thought` are the model's reasoning, not its answer,
//!   and are left out of the output.
//! - A refusal (safety filter, recitation, ...) becomes
//!   `ProviderError::Blocked` with Google's short reason code.
//!
//! Redirects, response size, and error text are handled by the shared
//! HTTP plumbing (ADR-025).

use reqwest::header::HeaderValue;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::http;
use crate::domain::{BoxFuture, GenerateRequest, Generation, Provider, ProviderError, Usage};

const API_KEY_HEADER: &str = "x-goog-api-key";

pub struct Gemini {
    client: reqwest::Client,
    base_url: url::Url,
    api_key: SecretString,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("HTTP client: {0}")]
    Client(#[from] reqwest::Error),
    #[error("base_url is not a usable URL")]
    BaseUrl,
    // Never includes the key itself.
    #[error("API key contains characters not allowed in an HTTP header")]
    ApiKey,
}

impl Gemini {
    /// `base_url` is the API root, such as
    /// `https://generativelanguage.googleapis.com/v1beta`.
    pub fn new(base_url: &str, api_key: SecretString) -> Result<Self, SetupError> {
        let base_url = url::Url::parse(base_url).map_err(|_| SetupError::BaseUrl)?;
        if base_url.cannot_be_a_base() {
            return Err(SetupError::BaseUrl);
        }
        // Fail at startup rather than on every request.
        api_key_header(&api_key).ok_or(SetupError::ApiKey)?;
        Ok(Self {
            client: http::client()?,
            base_url,
            api_key,
        })
    }

    /// `{base_url}/models/{model}:generateContent`, with the model name
    /// percent-encoded as one path segment: `/`, `?`, and `#` in it
    /// cannot change which endpoint is called.
    fn endpoint(&self, model: &str) -> url::Url {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("checked in new: base_url can be a base")
            .pop_if_empty()
            .push("models")
            .push(&format!("{model}:generateContent"));
        url
    }

    async fn call(
        &self,
        model: &str,
        request: &GenerateRequest,
    ) -> Result<Generation, ProviderError> {
        let body = GenerateContentRequest {
            contents: [RequestContent {
                role: "user",
                parts: [RequestPart {
                    text: &request.input,
                }],
            }],
            generation_config: request
                .max_output_tokens
                .map(|max_output_tokens| GenerationConfig { max_output_tokens }),
        };
        let key = api_key_header(&self.api_key)
            .ok_or_else(|| ProviderError::Request("API key is not a valid header".to_owned()))?;
        let http_request = self
            .client
            .post(self.endpoint(model))
            .header(API_KEY_HEADER, key)
            .json(&body);

        let (status, bytes) = http::send(http_request).await?;
        if !status.is_success() {
            // Newer errors carry a string `code` (`rate_limit_exceeded`);
            // older ones a numeric `code` and a string `status`
            // (`RESOURCE_EXHAUSTED`).
            return Err(ProviderError::Status {
                status: status.as_u16(),
                code: http::error_code(&bytes, &["code", "status"]),
            });
        }

        let parsed: GenerateContentResponse = serde_json::from_slice(&bytes).map_err(|error| {
            ProviderError::InvalidResponse(format!("not a generateContent response: {error}"))
        })?;
        let usage = parsed
            .usage_metadata
            .map(UsageMetadata::into_usage)
            .unwrap_or_default();

        let Some(candidate) = parsed.candidates.into_iter().next() else {
            // No answer at all: the prompt itself was refused, or the
            // response is broken.
            return Err(
                match parsed
                    .prompt_feedback
                    .and_then(|feedback| feedback.block_reason)
                {
                    Some(reason) => ProviderError::Blocked {
                        reason: http::identifier(&reason),
                    },
                    None => ProviderError::InvalidResponse("no candidates".to_owned()),
                },
            );
        };

        let output: String = candidate
            .content
            .into_iter()
            .flat_map(|content| content.parts)
            .filter(|part| !part.thought)
            .filter_map(|part| part.text)
            .collect();
        if !output.is_empty() {
            return Ok(Generation { output, usage });
        }
        match candidate.finish_reason.as_deref() {
            // The token limit ran out before any answer text (often spent
            // on thinking). The tokens were still used, so this is an
            // empty answer with its usage, not a failure.
            Some("MAX_TOKENS") => Ok(Generation { output, usage }),
            Some("STOP") | None => Err(ProviderError::InvalidResponse("no answer text".to_owned())),
            Some(reason) => Err(ProviderError::Blocked {
                reason: http::identifier(reason),
            }),
        }
    }
}

impl Provider for Gemini {
    fn generate<'a>(
        &'a self,
        model: &'a str,
        request: &'a GenerateRequest,
    ) -> BoxFuture<'a, Result<Generation, ProviderError>> {
        Box::pin(self.call(model, request))
    }
}

/// The key as a header value marked sensitive, so reqwest redacts it from
/// its own debug output.
fn api_key_header(key: &SecretString) -> Option<HeaderValue> {
    let mut value = HeaderValue::from_str(key.expose_secret()).ok()?;
    value.set_sensitive(true);
    Some(value)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest<'a> {
    contents: [RequestContent<'a>; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Serialize)]
struct RequestContent<'a> {
    role: &'static str,
    parts: [RequestPart<'a>; 1],
}

#[derive(Serialize)]
struct RequestPart<'a> {
    text: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    max_output_tokens: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    prompt_feedback: Option<PromptFeedback>,
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: Option<ResponseContent>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
struct ResponsePart {
    text: Option<String>,
    #[serde(default)]
    thought: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    block_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
    thoughts_token_count: Option<u64>,
    total_token_count: Option<u64>,
}

impl UsageMetadata {
    /// Output is answer plus thinking tokens, both as reported: that is
    /// what Google bills as output, and what OpenAI-compatible providers
    /// already report as `completion_tokens` (ADR-026). Google omits
    /// zero counts, so one missing half counts as zero; if both are
    /// missing, output stays unknown.
    fn into_usage(self) -> Usage {
        let output = match (self.candidates_token_count, self.thoughts_token_count) {
            (None, None) => None,
            (answer, thinking) => answer.unwrap_or(0).checked_add(thinking.unwrap_or(0)),
        };
        Usage {
            input_tokens: http::token_count(self.prompt_token_count),
            output_tokens: http::token_count(output),
            total_tokens: http::token_count(self.total_token_count),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(answer: Option<u64>, thinking: Option<u64>) -> Usage {
        UsageMetadata {
            prompt_token_count: Some(10),
            candidates_token_count: answer,
            thoughts_token_count: thinking,
            total_token_count: None,
        }
        .into_usage()
    }

    #[test]
    fn output_tokens_include_thinking() {
        assert_eq!(usage(Some(4), Some(30)).output_tokens, Some(34));
        assert_eq!(usage(Some(4), None).output_tokens, Some(4));
        assert_eq!(usage(None, Some(30)).output_tokens, Some(30));
        assert_eq!(usage(None, None).output_tokens, None);
    }

    #[test]
    fn output_tokens_that_do_not_fit_stay_unknown() {
        assert_eq!(
            usage(Some(u64::from(u32::MAX)), Some(1)).output_tokens,
            None
        );
    }

    #[test]
    fn model_name_cannot_change_the_endpoint() {
        let gemini = Gemini::new(
            "https://generativelanguage.googleapis.com/v1beta/",
            SecretString::from("k".to_owned()),
        )
        .unwrap();

        assert_eq!(
            gemini.endpoint("gemini-3.8-flash").as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:generateContent"
        );
        let hostile = gemini.endpoint("../x?key=1#y");
        assert_eq!(
            hostile.path(),
            "/v1beta/models/..%2Fx%3Fkey=1%23y:generateContent"
        );
        assert_eq!(hostile.query(), None);
        assert_eq!(hostile.fragment(), None);
    }

    #[test]
    fn key_that_cannot_be_a_header_is_refused_at_startup() {
        let result = Gemini::new(
            "https://example.com/v1beta",
            SecretString::from("bad\nkey".to_owned()),
        );

        assert!(matches!(result, Err(SetupError::ApiKey)));
    }
}
