//! Provider registry — manages provider instances by ID and model name.

use std::collections::HashMap;
use std::sync::Arc;

use mc_config::{ProviderConfig, ProviderKind};
use mc_core::McError;

use crate::anthropic::AnthropicProvider;
use crate::openai::OpenAiProvider;
use crate::retry::{RetryConfig, RetryProvider};
use crate::trait_def::LlmProvider;

/// Registry of LLM providers, keyed by provider ID.
///
/// Built from application configuration via [`ProviderRegistry::from_config`].
/// Supports lookup by provider ID or by model name.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
}

impl ProviderRegistry {
    /// Build a registry from a list of [`ProviderConfig`] entries.
    ///
    /// Each config entry is instantiated as the appropriate provider type
    /// based on its `kind` field.
    pub fn from_config(configs: &[ProviderConfig]) -> Result<Self, McError> {
        let mut providers: HashMap<String, Arc<dyn LlmProvider>> = HashMap::new();

        for config in configs {
            let provider: Arc<dyn LlmProvider> = match config.kind {
                ProviderKind::OpenAI => Arc::new(OpenAiProvider::new(
                    &config.id,
                    &config.base_url,
                    &config.api_key,
                    config.models.clone(),
                )),
                ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(
                    &config.id,
                    &config.base_url,
                    &config.api_key,
                    config.models.clone(),
                )),
            };

            if providers.contains_key(&config.id) {
                return Err(McError::Config(format!(
                    "Duplicate provider ID: {}",
                    config.id
                )));
            }
            providers.insert(config.id.clone(), provider);
        }

        Ok(Self { providers })
    }

    /// Get a provider by its unique ID.
    pub fn get(&self, id: &str) -> Result<Arc<dyn LlmProvider>, McError> {
        self.providers
            .get(id)
            .cloned()
            .ok_or_else(|| McError::Config(format!("Provider not found: {id}")))
    }

    /// Find a provider that lists the given model in its `models` list.
    ///
    /// Scans all registered providers. Returns an error if no provider
    /// claims to support the given model.
    pub fn find_by_model(&self, model: &str) -> Result<Arc<dyn LlmProvider>, McError> {
        for provider in self.providers.values() {
            if provider.models().iter().any(|m| m == model) {
                return Ok(provider.clone());
            }
        }
        Err(McError::Config(format!(
            "No provider found for model: {model}"
        )))
    }

    /// Wrap a provider with retry logic.
    ///
    /// Replaces the provider identified by `id` with a [`RetryProvider`]
    /// that automatically retries transient failures with exponential backoff.
    pub fn wrap_with_retry(&mut self, id: &str, config: RetryConfig) -> Result<(), McError> {
        let inner = self
            .providers
            .get(id)
            .cloned()
            .ok_or_else(|| McError::Config(format!("Provider not found: {id}")))?;
        let wrapped = Arc::new(RetryProvider::new(inner, config));
        self.providers.insert(id.to_string(), wrapped);
        Ok(())
    }

    /// Wrap ALL providers with retry logic using default configuration.
    ///
    /// Convenience method for applying retry to every registered provider.
    pub fn wrap_all_with_retry(&mut self, config: RetryConfig) {
        let old = std::mem::take(&mut self.providers);
        for (id, provider) in old {
            let wrapped = Arc::new(RetryProvider::new(provider, config.clone()));
            self.providers.insert(id, wrapped);
        }
    }
}
