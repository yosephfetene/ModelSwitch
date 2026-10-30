//! A built-in provider that returns the input unchanged. It needs no
//! account or network, so the whole gateway can be tried, demoed, and
//! smoke-tested without provider credentials. It reports no usage, like
//! any provider that does not count tokens (ADR-010).

use crate::domain::{BoxFuture, GenerateRequest, Generation, Provider, ProviderError, Usage};

#[derive(Debug, Clone, Copy, Default)]
pub struct EchoProvider;

impl Provider for EchoProvider {
    fn generate<'a>(
        &'a self,
        _model: &'a str,
        request: &'a GenerateRequest,
    ) -> BoxFuture<'a, Result<Generation, ProviderError>> {
        Box::pin(async move {
            Ok(Generation {
                output: request.input.clone(),
                usage: Usage::unavailable(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::UsageSource;

    use super::*;

    #[tokio::test]
    async fn echoes_input_and_reports_no_usage() {
        let request = GenerateRequest {
            input: "hello, gateway".to_owned(),
            max_output_tokens: Some(10),
        };

        let generation = EchoProvider.generate("echo-1", &request).await.unwrap();

        assert_eq!(generation.output, "hello, gateway");
        assert_eq!(generation.usage.source(), UsageSource::Unavailable);
    }
}
