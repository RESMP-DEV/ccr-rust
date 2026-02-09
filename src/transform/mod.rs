//! Transform module with factory-based registry.
//!
//! This module provides a factory-based approach to transformer instantiation,
//! separate from the existing static registry in `transformer.rs`.
//!
//! Supported transformers:
//! - DeepSeek
//! - GLM
//! - Kimi
//! - Minimax

// Core model transformers
pub mod deepseek;
pub mod glm;
pub mod kimi;
pub mod minimax;

pub use deepseek::DeepSeekTransformer;
pub use glm::GlmTransformer;
pub use kimi::KimiTransformer;
pub use minimax::MinimaxTransformer;

// Other transformers
pub mod anthropic;
pub use anthropic::AnthropicToOpenaiTransformer;
pub mod anthropic_to_openai;
pub use anthropic_to_openai::AnthropicToOpenAiResponseTransformer;
pub mod maxtoken;
pub use maxtoken::MaxTokenTransformer;
pub mod openai;
pub mod openai_to_anthropic;
pub use openai_to_anthropic::OpenAiToAnthropicTransformer;
pub mod registry;
pub use registry::TransformerRegistry;
pub mod thinktag;
pub use thinktag::ThinkTagTransformer;