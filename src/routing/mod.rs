//! Resolving a request's `model` to concrete targets (ADR-007).
//!
//! - An alias (`fast-cheap`) resolves to its ordered candidate list; with
//!   `routing.fallback = false`, only the first candidate.
//! - A pin (`groq/llama-3.3-70b`) resolves to exactly that target, and
//!   only if that provider and model are configured. Pins never fall
//!   back to anything else.
//!
//! Trying the next candidate after a failure is the reliability
//! executor's job (Stage 2); this module only decides what is eligible.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::Target;

/// The configured providers' models and the aliases over them.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    models: BTreeMap<String, BTreeSet<String>>,
    aliases: BTreeMap<String, Vec<Target>>,
}

/// The requested model is neither a known alias nor a configured pin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown model {0:?}")]
pub struct UnknownModel(pub String);

impl Catalog {
    /// `models` maps each provider to its model names. Aliases must only
    /// reference configured targets; configuration validation ensures it.
    pub fn new(
        models: impl IntoIterator<Item = (String, Vec<String>)>,
        aliases: impl IntoIterator<Item = (String, Vec<Target>)>,
    ) -> Self {
        Self {
            models: models
                .into_iter()
                .map(|(provider, models)| (provider, models.into_iter().collect()))
                .collect(),
            aliases: aliases.into_iter().collect(),
        }
    }

    pub fn is_configured(&self, target: &Target) -> bool {
        self.models
            .get(&target.provider)
            .is_some_and(|models| models.contains(&target.model))
    }

    /// The ordered, eligible targets for `model`. Never empty on success.
    pub fn resolve(&self, model: &str, fallback: bool) -> Result<Vec<Target>, UnknownModel> {
        let unknown = || UnknownModel(model.to_owned());
        if let Some(candidates) = self.aliases.get(model) {
            let take = if fallback { candidates.len() } else { 1 };
            return Ok(candidates.iter().take(take).cloned().collect());
        }
        let pin = Target::parse(model).ok_or_else(unknown)?;
        if self.is_configured(&pin) {
            Ok(vec![pin])
        } else {
            Err(unknown())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(value: &str) -> Target {
        Target::parse(value).unwrap()
    }

    fn catalog() -> Catalog {
        Catalog::new(
            [
                ("groq".to_owned(), vec!["llama-3.3-70b".to_owned()]),
                ("echo".to_owned(), vec!["echo-1".to_owned()]),
            ],
            [(
                "fast-cheap".to_owned(),
                vec![target("groq/llama-3.3-70b"), target("echo/echo-1")],
            )],
        )
    }

    #[test]
    fn alias_resolves_to_all_candidates_in_order() {
        assert_eq!(
            catalog().resolve("fast-cheap", true),
            Ok(vec![target("groq/llama-3.3-70b"), target("echo/echo-1")])
        );
    }

    #[test]
    fn alias_without_fallback_resolves_to_first_candidate_only() {
        assert_eq!(
            catalog().resolve("fast-cheap", false),
            Ok(vec![target("groq/llama-3.3-70b")])
        );
    }

    #[test]
    fn pin_resolves_to_exactly_that_target() {
        for fallback in [true, false] {
            assert_eq!(
                catalog().resolve("echo/echo-1", fallback),
                Ok(vec![target("echo/echo-1")])
            );
        }
    }

    #[test]
    fn unknown_aliases_and_unconfigured_pins_are_rejected() {
        for model in [
            "slow-expensive",
            "openai/gpt-5",
            "groq/not-a-model",
            "groq/",
            "",
        ] {
            assert_eq!(
                catalog().resolve(model, true),
                Err(UnknownModel(model.to_owned())),
                "{model:?}"
            );
        }
    }
}
