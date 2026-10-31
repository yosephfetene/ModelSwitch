//! HTTP plumbing shared by the network adapters (ADR-025).
//!
//! - Redirects are never followed, so an API key cannot be forwarded to
//!   a host other than the configured one.
//! - Response bodies are capped, so a misbehaving endpoint cannot exhaust
//!   memory.
//! - Error text never carries the request URL, and of a provider's error
//!   body only a short identifier is kept, never its message, which can
//!   quote the prompt.

use crate::domain::ProviderError;

/// Largest response body read from a provider.
pub(crate) const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IDENTIFIER_CHARS: usize = 64;

/// An HTTP client for provider calls. The overall deadline is applied by
/// the caller; dropping the future cancels the request.
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    install_crypto_provider();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("modelswitch/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// reqwest is built without a bundled crypto backend; use `ring`, the one
/// sqlx already uses, so the binary carries a single TLS implementation.
/// Installing twice is harmless: later attempts are ignored.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A transport failure. The URL is stripped: some providers accept keys
/// in query strings, and URLs must never reach the logs.
pub(crate) fn request_error(error: reqwest::Error) -> ProviderError {
    ProviderError::Request(error.without_url().to_string())
}

/// Sends the request and returns the status and (capped) body.
pub(crate) async fn send(
    request: reqwest::RequestBuilder,
) -> Result<(reqwest::StatusCode, Vec<u8>), ProviderError> {
    let mut response = request.send().await.map_err(request_error)?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ProviderError::InvalidResponse(
                "response body exceeds 4 MiB".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

/// The provider's short error identifier from an `{"error": {...}}` body:
/// the first of `fields` that holds a string, kept only if it looks like
/// an identifier. Anything else is dropped: free text can quote the
/// prompt and must not reach the logs.
pub(crate) fn error_code(body: &[u8], fields: &[&str]) -> Option<String> {
    let envelope: serde_json::Value = serde_json::from_slice(body).ok()?;
    let error = envelope.get("error")?;
    let code = fields.iter().find_map(|field| error.get(field)?.as_str())?;
    identifier(code)
}

/// `value` if it is a short identifier such as `rate_limit_exceeded` or
/// `RESOURCE_EXHAUSTED`.
pub(crate) fn identifier(value: &str) -> Option<String> {
    let looks_like_identifier = !value.is_empty()
        && value.chars().count() <= MAX_IDENTIFIER_CHARS
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    looks_like_identifier.then(|| value.to_owned())
}

/// A reported token count, or unknown if it does not fit: counts are
/// never clamped into a wrong number (ADR-010).
pub(crate) fn token_count(count: Option<u64>) -> Option<u32> {
    count.and_then(|count| u32::try_from(count).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENAI_FIELDS: &[&str] = &["code", "type"];

    #[test]
    fn error_code_prefers_the_first_field_that_is_a_string() {
        let both = br#"{"error":{"message":"x","type":"invalid_request_error","code":"rate_limit_exceeded"}}"#;
        let type_only = br#"{"error":{"message":"x","type":"invalid_request_error","code":null}}"#;
        let numeric_code = br#"{"error":{"code":429,"message":"x","status":"RESOURCE_EXHAUSTED"}}"#;

        assert_eq!(
            error_code(both, OPENAI_FIELDS).as_deref(),
            Some("rate_limit_exceeded")
        );
        assert_eq!(
            error_code(type_only, OPENAI_FIELDS).as_deref(),
            Some("invalid_request_error")
        );
        assert_eq!(
            error_code(numeric_code, &["code", "status"]).as_deref(),
            Some("RESOURCE_EXHAUSTED")
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
            assert_eq!(
                error_code(body, OPENAI_FIELDS),
                None,
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn oversized_token_counts_stay_unknown() {
        assert_eq!(token_count(Some(u64::from(u32::MAX) + 1)), None);
        assert_eq!(token_count(Some(7)), Some(7));
        assert_eq!(token_count(None), None);
    }
}
