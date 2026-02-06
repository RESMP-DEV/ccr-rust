//! Transform module with factory-based registry.
//!
//! This module provides a factory-based approach to transformer instantiation,
//! separate from the existing static registry in `transformer.rs`.

pub mod anthropic;
pub mod anthropic_to_openai;
pub mod maxtoken;
pub mod openai;
pub mod openai_to_anthropic;
pub mod registry;
pub mod thinktag;

pub use anthropic::AnthropicToOpenaiTransformer;
pub use anthropic_to_openai::AnthropicToOpenAiResponseTransformer;
pub use openai_to_anthropic::OpenAiToAnthropicTransformer;
pub use registry::TransformerRegistry;
