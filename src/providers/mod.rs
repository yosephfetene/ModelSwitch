//! Provider adapters and the registry of configured providers.

pub mod echo;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{AdapterKind, ProviderConfig};
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

impl Registry {
    pub fn new(slots: impl IntoIterator<Item = (String, ProviderSlot)>) -> Self {
        Self {
            slots: slots.into_iter().collect(),
        }
    }

    /// Builds every provider named in the configuration.
    pub fn from_config<'a>(
        providers: impl IntoIterator<Item = (&'a String, &'a ProviderConfig)>,
    ) -> Self {
        Self::new(providers.into_iter().map(|(name, config)| {
            let provider: Arc<dyn Provider> = match config.adapter {
                AdapterKind::Echo => Arc::new(echo::EchoProvider),
            };
            let slot = ProviderSlot {
                provider,
                timeout: config.timeout,
            };
            (name.clone(), slot)
        }))
    }

    pub fn get(&self, name: &str) -> Option<&ProviderSlot> {
        self.slots.get(name)
    }
}
