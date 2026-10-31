//! Provider adapters and the registry of configured providers.

pub mod echo;
pub mod gemini;
mod http;
pub mod openai_compat;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{AdapterConfig, ProviderConfig};
use crate::domain::Provider;

/// A configured provider, ready to call.
#[derive(Clone)]
pub struct ProviderSlot {
    pub provider: Arc<dyn Provider>,
    /// Deadline for one call to this provider.
    pub timeout: Duration,
}

/// Configured providers by name.
#[derive(Clone, Default)]
pub struct Registry {
    slots: HashMap<String, ProviderSlot>,
}

/// A provider could not be set up (for example, its HTTP client).
#[derive(Debug, thiserror::Error)]
#[error("provider {name}: {reason}")]
pub struct BuildError {
    pub name: String,
    pub reason: String,
}

impl Registry {
    pub fn new(slots: impl IntoIterator<Item = (String, ProviderSlot)>) -> Self {
        Self {
            slots: slots.into_iter().collect(),
        }
    }

    /// Builds every provider named in the configuration.
    pub fn from_config<'a>(
        providers: impl IntoIterator<Item = (&'a String, &'a ProviderConfig)>,
    ) -> Result<Self, BuildError> {
        let mut slots = HashMap::new();
        for (name, config) in providers {
            let failed = |reason: String| BuildError {
                name: name.clone(),
                reason,
            };
            let provider: Arc<dyn Provider> = match &config.adapter {
                AdapterConfig::Echo => Arc::new(echo::EchoProvider),
                AdapterConfig::OpenaiCompatible { base_url, api_key } => Arc::new(
                    openai_compat::OpenAiCompatible::new(base_url, api_key.clone())
                        .map_err(|error| failed(error.to_string()))?,
                ),
                AdapterConfig::Gemini { base_url, api_key } => Arc::new(
                    gemini::Gemini::new(base_url, api_key.clone())
                        .map_err(|error| failed(error.to_string()))?,
                ),
            };
            let slot = ProviderSlot {
                provider,
                timeout: config.timeout,
            };
            slots.insert(name.clone(), slot);
        }
        Ok(Self { slots })
    }

    pub fn get(&self, name: &str) -> Option<&ProviderSlot> {
        self.slots.get(name)
    }
}
