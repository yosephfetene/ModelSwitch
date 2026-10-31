//! Core types shared by routing, providers, and the API. Nothing here
//! depends on HTTP, the database, or configuration.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// A boxed future, so traits with async methods (`Provider`,
/// `DatabaseCheck`, ...) can be used as trait objects (`Arc<dyn ...>`).
/// Plain `async fn` in traits is not yet usable through `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One concrete provider and model, such as `groq/llama-3.3-70b`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub provider: String,
    pub model: String,
}

impl Target {
    /// Parses `provider/model`. The model may itself contain `/` (some
    /// providers name models like `meta-llama/llama-4`), so only the first
    /// `/` separates the two.
    pub fn parse(value: &str) -> Option<Self> {
        let (provider, model) = value.split_once('/')?;
        (!provider.is_empty() && !model.is_empty()).then(|| Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// What a provider is asked to do, independent of any wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateRequest {
    pub input: String,
    pub max_output_tokens: Option<u32>,
}

/// A provider's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub output: String,
    pub usage: Usage,
}

/// Token counts exactly as the provider reported them. Anything not
/// reported stays `None`; it is never estimated into these fields
/// (ADR-010).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    ProviderReported,
    Unavailable,
}

impl Usage {
    pub fn unavailable() -> Self {
        Self::default()
    }

    pub fn source(&self) -> UsageSource {
        if self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.total_tokens.is_some()
        {
            UsageSource::ProviderReported
        } else {
            UsageSource::Unavailable
        }
    }
}

/// Why a provider call failed. The detail is for logs only: it may
/// contain raw provider text and must never reach a response (FR-09).
/// Retry classification arrives with the reliability executor (Stage 2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The request could not be sent or the response not received
    /// (connection refused, TLS failure, reset).
    #[error("provider request failed: {0}")]
    Request(String),
    /// The provider answered with a non-success HTTP status. `code` is
    /// the provider's short error identifier when it sent one (such as
    /// `rate_limit_exceeded`), never its free-text message, which can
    /// quote the prompt.
    #[error("provider returned HTTP {status}{}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Status { status: u16, code: Option<String> },
    #[error("provider returned an unusable response: {0}")]
    InvalidResponse(String),
    /// The provider declined to answer, for example because of a safety
    /// filter. `reason` is the provider's short identifier (such as
    /// `SAFETY`), never its explanation. Kept apart from failures so
    /// Stage 2 can decide not to retry a prompt that will be refused
    /// again.
    #[error("provider blocked the request{}", reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default())]
    Blocked { reason: Option<String> },
}

/// The contract every provider adapter implements (FR-03). An adapter
/// speaks one wire format; a configured provider is an endpoint,
/// credential, and model list using an adapter (ADR-008).
pub trait Provider: Send + Sync {
    fn generate<'a>(
        &'a self,
        model: &'a str,
        request: &'a GenerateRequest,
    ) -> BoxFuture<'a, Result<Generation, ProviderError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_splits_on_the_first_slash() {
        let target = Target::parse("groq/meta-llama/llama-4").unwrap();

        assert_eq!(target.provider, "groq");
        assert_eq!(target.model, "meta-llama/llama-4");
        assert_eq!(target.to_string(), "groq/meta-llama/llama-4");
    }

    #[test]
    fn target_needs_both_parts() {
        for value in ["", "groq", "groq/", "/llama", "/"] {
            assert_eq!(Target::parse(value), None, "{value:?}");
        }
    }

    #[test]
    fn usage_source_follows_what_was_reported() {
        assert_eq!(Usage::unavailable().source(), UsageSource::Unavailable);

        let partial = Usage {
            output_tokens: Some(5),
            ..Usage::default()
        };
        assert_eq!(partial.source(), UsageSource::ProviderReported);
    }
}
