//! Factory-based transformer registry.
//!
//! This module provides a registry for creating transformer instances
//! from factory functions, allowing dynamic instantiation based on configuration.

use super::{
    AnthropicToOpenaiTransformer, DeepSeekTransformer, GlmTransformer, KimiTransformer,
    MaxTokenTransformer, MinimaxTransformer, OpenAiToAnthropicTransformer, ThinkTagTransformer,
};
use crate::config::TransformerEntry;
use crate::transformer::{Transformer, TransformerChain};
use std::collections::HashMap;

/// Type alias for a transformer factory function.
///
/// Takes optional JSON configuration and returns a boxed transformer.
pub type TransformerFactory = dyn Fn(Option<&serde_json::Value>) -> Box<dyn Transformer>;

/// Registry for transformer factory functions.
///
/// Uses static string keys to enable lifetime-safe storage of factories.
pub struct TransformerRegistry {
    factories: HashMap<&'static str, Box<TransformerFactory>>,
}

impl TransformerRegistry {
    /// Create a new transformer registry with default provider transformers.
    pub fn new() -> Self {
        let mut registry = Self {
            factories: HashMap::new(),
        };

        // Register provider-specific transformers
        // These transformers adapt provider-specific response formats
        // into a standardized format for the AlphaHENG system.
        // Tier 2: Z.AI GLM-5
        registry.register("zai", |_opts| Box::new(GlmTransformer::default()));
        // Tier 4: Minimax M2.5 (high-performance reasoning, long context)
        registry.register("minimax", |_opts| Box::new(MinimaxTransformer));
        // Tier 3: Moonshot Kimi
        // Keep both provider aliases mapped to the same transformer.
        for provider in ["moonshot", "kimi"] {
            registry.register(provider, |_opts| Box::new(KimiTransformer));
        }
        // Tier 7: DeepSeek Reasoner
        registry.register("deepseek", |_opts| Box::new(DeepSeekTransformer));

        // Format conversion transformers
        registry.register("anthropic", |_opts| Box::new(AnthropicToOpenaiTransformer));
        registry.register("openai-to-anthropic", |_opts| {
            Box::new(OpenAiToAnthropicTransformer)
        });

        // Utility transformers
        registry.register("maxtoken", |opts| {
            if let Some(o) = opts {
                if let Ok(t) = MaxTokenTransformer::from_options(o) {
                    return Box::new(t);
                }
            }
            Box::new(MaxTokenTransformer::new(65536, true))
        });
        registry.register("thinktag", |_opts| Box::new(ThinkTagTransformer));

        registry
    }

    /// Register a transformer factory function with the given name.
    ///
    /// # Arguments
    /// * `name` - Static string identifier for this transformer
    /// * `factory` - Function that creates a transformer instance from optional options
    pub fn register(
        &mut self,
        name: &'static str,
        factory: impl Fn(Option<&serde_json::Value>) -> Box<dyn Transformer> + 'static,
    ) {
        self.factories.insert(name, Box::new(factory));
    }

    /// Build a single transformer from a config entry.
    ///
    /// Returns `None` if the transformer is not registered.
    pub fn build(&self, entry: &TransformerEntry) -> Option<Box<dyn Transformer>> {
        let factory = self.factories.get(entry.name())?;
        let options = entry.options();
        Some(factory(options))
    }

    /// Build a transformer chain from a list of config entries.
    ///
    /// Skips any entries with unregistered transformers.
    pub fn build_chain(&self, entries: &[TransformerEntry]) -> TransformerChain {
        let mut chain = TransformerChain::new();
        for entry in entries {
            if let Some(transformer) = self.build(entry) {
                chain = chain.with_transformer(std::sync::Arc::from(transformer));
            }
        }
        chain
    }

    /// Check if a transformer name is registered.
    pub fn has(&self, name: &str) -> bool {
        self.factories.contains_key(name)
    }

    /// Get the number of registered transformers.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

impl Default for TransformerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_new_registers_provider_transformers() {
        let registry = TransformerRegistry::new();
        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 9);
        assert!(registry.has("zai"));
        assert!(registry.has("minimax"));
        assert!(registry.has("moonshot"));
        assert!(registry.has("kimi"));
        assert!(registry.has("deepseek"));
        assert!(registry.has("anthropic"));
        assert!(registry.has("openai-to-anthropic"));
        assert!(registry.has("maxtoken"));
        assert!(registry.has("thinktag"));
    }

    #[test]
    fn registry_registers_factory() {
        let mut registry = TransformerRegistry::new();
        let baseline_len = registry.len();
        registry.register("test_factory", |_opts| {
            Box::new(crate::transformer::IdentityTransformer)
        });

        assert_eq!(registry.len(), baseline_len + 1);
        assert!(registry.has("test_factory"));
    }

    #[test]
    fn registry_builds_single_transformer() {
        let mut registry = TransformerRegistry::new();
        registry.register("identity", |_opts| {
            Box::new(crate::transformer::IdentityTransformer)
        });

        let entry = TransformerEntry::Name("identity".to_string());
        let transformer = registry.build(&entry);

        assert!(transformer.is_some());
    }

    #[test]
    fn registry_returns_none_for_unregistered() {
        let registry = TransformerRegistry::new();
        let entry = TransformerEntry::Name("unknown".to_string());

        assert!(registry.build(&entry).is_none());
    }

    #[test]
    fn registry_builds_chain() {
        let mut registry = TransformerRegistry::new();
        registry.register("identity", |_opts| {
            Box::new(crate::transformer::IdentityTransformer)
        });
        registry.register("anthropic", |_opts| {
            Box::new(crate::transformer::AnthropicTransformer)
        });

        let entries = vec![
            TransformerEntry::Name("identity".to_string()),
            TransformerEntry::Name("anthropic".to_string()),
        ];

        let chain = registry.build_chain(&entries);
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn registry_chain_skips_unknown() {
        let mut registry = TransformerRegistry::new();
        registry.register("identity", |_opts| {
            Box::new(crate::transformer::IdentityTransformer)
        });

        let entries = vec![
            TransformerEntry::Name("identity".to_string()),
            TransformerEntry::Name("unknown".to_string()),
        ];

        let chain = registry.build_chain(&entries);
        // Only the known transformer should be added
        assert_eq!(chain.len(), 1);
    }
}
