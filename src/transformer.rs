//! Request/response transformer module for CCR-Rust.
//!
//! Transformers modify requests before sending to providers and responses
//! before returning to clients. This mirrors the Node.js transformer system.

use crate::config::TransformerEntry;
use crate::transform::glm::GlmTransformer;
use crate::transform::minimax::MinimaxTransformer;
use crate::transform::openai_to_anthropic::OpenAiToAnthropicTransformer;
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, trace};

// ============================================================================
// Transformer Trait
// ============================================================================

/// Core trait for request/response transformers.
///
/// Transformers can:
/// - Modify requests before sending to upstream providers
/// - Modify responses before returning to clients
/// - Handle streaming transformations
pub trait Transformer: Send + Sync {
    /// Apply transformation to an incoming Anthropic request.
    ///
    /// Returns a modified request or an error if transformation fails.
    fn transform_request(&self, request: Value) -> Result<Value> {
        Ok(request)
    }

    /// Apply transformation to an outgoing Anthropic response.
    ///
    /// Returns a modified response or an error if transformation fails.
    fn transform_response(&self, response: Value) -> Result<Value> {
        Ok(response)
    }

    /// Check if this transformer should be applied as a passthrough (no-op).
    ///
    /// Some transformers are identity passthroughs when specific conditions
    /// are met (e.g., Anthropic transformer with Anthropic API).
    #[allow(dead_code)]
    fn is_passthrough(&self, request: &Value) -> bool {
        let _ = request;
        false
    }

    /// Get the transformer's name for logging and debugging.
    fn name(&self) -> &str {
        "unknown"
    }
}

// ============================================================================
// Concrete Transformer Implementations
// ============================================================================

/// Identity passthrough transformer - makes no modifications.
///
/// Used when no transformation is needed or as a fallback.
#[derive(Debug, Clone)]
pub struct IdentityTransformer;

impl Transformer for IdentityTransformer {
    fn name(&self) -> &str {
        "identity"
    }

    fn is_passthrough(&self, _request: &Value) -> bool {
        true
    }
}

/// Anthropic API transformer.
///
/// Handles transformations specific to the Anthropic API format.
/// In most cases, this is a passthrough since we already use
/// Anthropic format internally.
#[derive(Debug, Clone)]
pub struct AnthropicTransformer;

impl Transformer for AnthropicTransformer {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn is_passthrough(&self, request: &Value) -> bool {
        // Anthropic format requests passthrough by default
        request.get("model").is_some()
    }
}

/// DeepSeek API transformer.
///
/// Handles DeepSeek-specific transformations including:
/// - Tool use format conversions
/// - Response parsing for DeepSeek models
#[derive(Debug, Clone)]
pub struct DeepSeekTransformer;

impl Transformer for DeepSeekTransformer {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // DeepSeek may return tool calls in a different format
        // Ensure tool calls are properly formatted for Anthropic format
        if let Some(content_array) = response.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in content_array {
                if let Some(block_obj) = block.as_object_mut() {
                    // Normalize tool use blocks
                    if block_obj.get("type") == Some(&Value::String("tool_use".to_string()))
                        && !block_obj.contains_key("id")
                    {
                        // Generate a deterministic ID if missing
                        block_obj.insert(
                            "id".to_string(),
                            Value::String(format!("toolu_{}", uuid_nopanic::timestamp_ms())),
                        );
                    }
                }
            }
        }
        Ok(response)
    }
}

/// OpenRouter API transformer.
///
/// Handles OpenRouter-specific transformations.
#[derive(Debug, Clone)]
pub struct OpenRouterTransformer;

impl Transformer for OpenRouterTransformer {
    fn name(&self) -> &str {
        "openrouter"
    }
}

/// Anthropic to OpenAI transformer.
///
/// Converts Anthropic API format to OpenAI API format.
/// Handles:
/// - Message content blocks (array → string or keep array for multimodal)
/// - Tool definitions (input_schema → parameters)
/// - Tool choice (type conversion)
/// - Remove Anthropic-specific fields (metadata, stop_sequences)
#[derive(Debug, Clone)]
pub struct AnthropicToOpenaiTransformer;

impl Transformer for AnthropicToOpenaiTransformer {
    fn name(&self) -> &str {
        "anthropic-to-openai"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Extract and handle the system field (Anthropic-specific)
        // Anthropic: system field at top level
        // OpenAI: system message prepended to messages array
        if let Some(request_obj) = request.as_object_mut() {
            if let Some(system) = request_obj.remove("system") {
                // Convert system prompt to string (Anthropic allows string or array of blocks)
                let system_content: String = if let Some(s) = system.as_str() {
                    s.to_string()
                } else if let Some(blocks) = system.as_array() {
                    // Handle array of content blocks
                    let text_blocks: Vec<&str> = blocks
                        .iter()
                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                        .collect();
                    text_blocks.join("\n\n")
                } else {
                    // Unknown format, convert to string
                    system.to_string()
                };

                // Prepend system message to messages array
                if let Some(messages) = request_obj.get_mut("messages") {
                    if let Some(messages_array) = messages.as_array_mut() {
                        let system_message = serde_json::json!({
                            "role": "system",
                            "content": system_content
                        });
                        messages_array.insert(0, system_message);
                    }
                } else {
                    // No messages array yet, create one with just the system message
                    let messages = serde_json::json!([{
                        "role": "system",
                        "content": system_content
                    }]);
                    request_obj.insert("messages".to_string(), messages);
                }
            }
        }

        // Transform messages from Anthropic content blocks to OpenAI format
        if let Some(messages) = request.get_mut("messages") {
            if let Some(messages_array) = messages.as_array_mut() {
                for message in messages_array {
                    if let Some(message_obj) = message.as_object_mut() {
                        if let Some(content) = message_obj.get_mut("content") {
                            // Anthropic: content can be a string or an array of content blocks
                            // OpenAI: content can be a string or an array (for multimodal)

                            // If content is a string, leave it as is (both formats support strings)
                            if content.is_string() {
                                continue;
                            }

                            // If content is an array, check if we need to convert it
                            if let Some(content_array) = content.as_array_mut() {
                                // Check if all blocks are text blocks with no special handling
                                let all_simple_text = content_array.iter().all(|block| {
                                    matches!(
                                        block.get("type").and_then(|t| t.as_str()),
                                        Some("text")
                                    )
                                });

                                if all_simple_text {
                                    // Convert single or multiple text blocks to a simple string
                                    let combined_text: String = content_array
                                        .iter()
                                        .filter_map(|block| {
                                            block.get("text").and_then(|t| t.as_str())
                                        })
                                        .collect::<Vec<&str>>()
                                        .join("\n\n");
                                    *content = Value::String(combined_text);
                                } else {
                                    // Mixed content (text + image, etc.) - convert to OpenAI format
                                    // Anthropic: {"type": "image", "source": {"type": "base64", ...}}
                                    // OpenAI: {"type": "image_url", "image_url": {"url": "..."}}
                                    for block in content_array.iter_mut() {
                                        if let Some(block_obj) = block.as_object_mut() {
                                            if block_obj.get("type")
                                                == Some(&Value::String("image".to_string()))
                                            {
                                                if let Some(source) = block_obj.get("source") {
                                                    let image_url =
                                                        if let Some(data) = source.get("data") {
                                                            // Base64 encoded data
                                                            let media_type = source
                                                                .get("media_type")
                                                                .and_then(|m| m.as_str())
                                                                .unwrap_or("image/jpeg");
                                                            format!(
                                                                "data:{};base64,{}",
                                                                media_type,
                                                                data.as_str().unwrap_or("")
                                                            )
                                                        } else {
                                                            // URL-based source
                                                            source
                                                                .get("url")
                                                                .and_then(|u| u.as_str())
                                                                .unwrap_or("")
                                                                .to_string()
                                                        };

                                                    *block = serde_json::json!({
                                                        "type": "image_url",
                                                        "image_url": {"url": image_url}
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Transform tools from Anthropic to OpenAI format
        if let Some(tools) = request.get_mut("tools") {
            if let Some(tools_array) = tools.as_array_mut() {
                for tool in tools_array {
                    if let Some(tool_obj) = tool.as_object_mut() {
                        // Anthropic: {"name": "...", "description": "...", "input_schema": {...}}
                        // OpenAI: {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}

                        let name = tool_obj.get("name").cloned();
                        let description = tool_obj.get("description").cloned();
                        let input_schema = tool_obj.get("input_schema").cloned();

                        if let (Some(n), Some(d), Some(is)) = (name, description, input_schema) {
                            *tool = serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": n,
                                    "description": d,
                                    "parameters": is
                                }
                            });
                        }
                    }
                }
            }
        }

        // Transform tool_choice from Anthropic to OpenAI format
        if let Some(tool_choice) = request.get_mut("tool_choice") {
            let new_tool_choice = match tool_choice {
                // Anthropic: {"type": "tool", "name": "..."}
                // OpenAI: {"type": "function", "function": {"name": "..."}}
                Value::Object(map)
                    if map.get("type") == Some(&Value::String("tool".to_string())) =>
                {
                    map.get("name").cloned().map(|name| {
                        serde_json::json!({
                            "type": "function",
                            "function": {"name": name}
                        })
                    })
                }
                // Anthropic: "any" → OpenAI: "required"
                Value::String(s) if s == "any" => Some(Value::String("required".to_string())),
                // "auto" stays the same
                Value::String(s) if s == "auto" => None,
                // Already in OpenAI format, leave as is
                Value::Object(map)
                    if map.get("type") == Some(&Value::String("function".to_string())) =>
                {
                    None
                }
                _ => None,
            };

            if let Some(tc) = new_tool_choice {
                *tool_choice = tc;
            }
        }

        // max_tokens is required in Anthropic but optional in OpenAI - keep it as-is

        // Remove Anthropic-specific fields
        if let Some(request_obj) = request.as_object_mut() {
            // Remove metadata field (Anthropic-specific)
            request_obj.remove("metadata");

            // Convert stop_sequences to stop (if present)
            if let Some(stop_sequences) = request_obj.remove("stop_sequences") {
                // Anthropic: stop_sequences: ["string", ...]
                // OpenAI: stop: "string" or stop: ["string", ...]
                request_obj.insert("stop".to_string(), stop_sequences);
            }
        }

        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Convert OpenAI tool calls back to Anthropic format
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                for block in content_array {
                    if let Some(block_obj) = block.as_object_mut() {
                        // Ensure tool_use blocks have IDs
                        if block_obj.get("type") == Some(&Value::String("tool_use".to_string()))
                            && !block_obj.contains_key("id")
                        {
                            block_obj.insert(
                                "id".to_string(),
                                Value::String(format!("toolu_{}", uuid_nopanic::timestamp_ms())),
                            );
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Tool use enhancement transformer.
///
/// Ensures tool calls are properly formatted and adds any missing metadata.
#[derive(Debug, Clone)]
pub struct ToolUseTransformer;

impl Transformer for ToolUseTransformer {
    fn name(&self) -> &str {
        "tooluse"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Ensure tools are properly formatted for providers that need it
        if let Some(tools_array) = request.get_mut("tools").and_then(|t| t.as_array_mut()) {
            for tool in tools_array {
                if let Some(tool_obj) = tool.as_object_mut() {
                    // Ensure tool has required fields
                    if !tool_obj.contains_key("input_schema") {
                        tool_obj.insert(
                            "input_schema".to_string(),
                            Value::Object(serde_json::Map::new()),
                        );
                    }
                }
            }
        }
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Ensure tool_use blocks have IDs
        if let Some(content_array) = response.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in content_array {
                if let Some(block_obj) = block.as_object_mut() {
                    if block_obj.get("type") == Some(&Value::String("tool_use".to_string()))
                        && !block_obj.contains_key("id")
                    {
                        block_obj.insert(
                            "id".to_string(),
                            Value::String(format!("toolu_{}", uuid_nopanic::timestamp_ms())),
                        );
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Max tokens transformer.
///
/// Ensures requests respect max_tokens limits.
#[derive(Debug, Clone)]
pub struct MaxTokenTransformer {
    max_tokens: u32,
}

impl MaxTokenTransformer {
    pub fn new(max_tokens: u32) -> Self {
        Self { max_tokens }
    }
}

impl Transformer for MaxTokenTransformer {
    fn name(&self) -> &str {
        "maxtoken"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Cap max_tokens if present
        if let Some(max_tokens) = request.get_mut("max_tokens") {
            if let Some(current) = max_tokens.as_u64() {
                if current > self.max_tokens as u64 {
                    *max_tokens = Value::Number(serde_json::Number::from(self.max_tokens));
                }
            }
        } else {
            // Add max_tokens if not present
            request
                .as_object_mut()
                .map(|obj| obj.insert("max_tokens".to_string(), self.max_tokens.into()));
        }
        Ok(request)
    }
}

/// Reasoning transformer for DeepSeek-R1 and other reasoning models.
///
/// Handles the special reasoning_content field in responses.
#[derive(Debug, Clone)]
pub struct ReasoningTransformer;

impl Transformer for ReasoningTransformer {
    fn name(&self) -> &str {
        "reasoning"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Extract thinking field before mutable borrow of content
        let thinking_text = response
            .get("thinking")
            .and_then(|t| t.as_str())
            .map(String::from);

        // Ensure reasoning content is properly formatted in content blocks
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                // Check if there's a thinking block
                let has_thinking = content_array
                    .iter()
                    .any(|block| block.get("type") == Some(&Value::String("thinking".to_string())));

                // Some providers return reasoning in a different format
                if !has_thinking {
                    if let Some(text) = thinking_text {
                        // Convert to proper thinking content block
                        let thinking_block = serde_json::json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": ""
                        });
                        content_array.insert(0, thinking_block);
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Enhance tool transformer.
///
/// Adds additional metadata to tool calls for better handling.
#[derive(Debug, Clone)]
pub struct EnhanceToolTransformer;

impl Transformer for EnhanceToolTransformer {
    fn name(&self) -> &str {
        "enhancetool"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Add cache_control metadata to tool_use blocks
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                for block in content_array {
                    if let Some(block_obj) = block.as_object_mut() {
                        if block_obj.get("type") == Some(&Value::String("tool_use".to_string())) {
                            // Add cache control if missing
                            if !block_obj.contains_key("cache_control") {
                                let mut cache_control = serde_json::Map::new();
                                cache_control.insert(
                                    "type".to_string(),
                                    Value::String("ephemeral".to_string()),
                                );
                                block_obj.insert(
                                    "cache_control".to_string(),
                                    Value::Object(cache_control),
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Think tag transformer.
///
/// Strips thinking/reasoning tags from response text content.
/// Removes <think>, <thinking>, and <reasoning> blocks and their content.
#[derive(Debug, Clone)]
pub struct ThinkTagTransformer;

impl Transformer for ThinkTagTransformer {
    fn name(&self) -> &str {
        "thinktag"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        lazy_static::lazy_static! {
            // Regex crate doesn't support backreferences, so use alternation
            static ref THINK_TAG_RE: Regex = Regex::new(
                r"(?s)<think>.*?</think>|<thinking>.*?</thinking>|<reasoning>.*?</reasoning>"
            ).unwrap();
        }

        if let Some(content) = response.get_mut("content") {
            if let Some(arr) = content.as_array_mut() {
                for block in arr {
                    if let Some(text) = block.get_mut("text") {
                        if let Some(s) = text.as_str() {
                            let stripped = THINK_TAG_RE.replace_all(s, "");
                            *text = Value::String(stripped.trim().to_string());
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}

// ============================================================================
// Transformer Chain
// ============================================================================

/// A chain of transformers applied in sequence.
#[derive(Clone)]
pub struct TransformerChain {
    transformers: Vec<Arc<dyn Transformer>>,
}

impl TransformerChain {
    /// Create a new empty transformer chain.
    pub fn new() -> Self {
        Self {
            transformers: Vec::new(),
        }
    }

    /// Add a transformer to the chain.
    pub fn with_transformer(mut self, transformer: Arc<dyn Transformer>) -> Self {
        self.transformers.push(transformer);
        self
    }

    /// Apply all transformers in the chain to a request.
    pub fn apply_request(&self, mut request: Value) -> Result<Value> {
        for transformer in &self.transformers {
            debug!(name = %transformer.name(), "applying request transformer");
            request = transformer.transform_request(request)?;
            trace!(after = %serde_json::to_string(&request).unwrap_or_default(),
                   "request after transformation");
        }
        Ok(request)
    }

    /// Apply all transformers in the chain to a response.
    pub fn apply_response(&self, mut response: Value) -> Result<Value> {
        // Apply transformers in reverse order for responses
        for transformer in self.transformers.iter().rev() {
            debug!(name = %transformer.name(), "applying response transformer");
            response = transformer.transform_response(response)?;
            trace!(after = %serde_json::to_string(&response).unwrap_or_default(),
                   "response after transformation");
        }
        Ok(response)
    }

    /// Check if the entire chain is a passthrough (all transformers are identity).
    #[allow(dead_code)]
    pub fn is_passthrough(&self, request: &Value) -> bool {
        self.transformers.iter().all(|t| t.is_passthrough(request))
    }

    /// Get the number of transformers in the chain.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.transformers.len()
    }

    /// Check if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.transformers.is_empty()
    }
}

impl Default for TransformerChain {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Transformer Registry
// ============================================================================

/// Registry for looking up and creating transformer instances by name.
pub struct TransformerRegistry {
    transformers: HashMap<String, Arc<dyn Transformer>>,
}

impl TransformerRegistry {
    /// Create a new transformer registry with default transformers registered.
    pub fn new() -> Self {
        let mut registry = Self {
            transformers: HashMap::new(),
        };

        // Register default transformers
        registry.register("anthropic", Arc::new(AnthropicTransformer));
        registry.register(
            "anthropic-to-openai",
            Arc::new(AnthropicToOpenaiTransformer),
        );
        registry.register("deepseek", Arc::new(DeepSeekTransformer));
        registry.register(
            "openai-to-anthropic",
            Arc::new(OpenAiToAnthropicTransformer),
        );
        registry.register("minimax", Arc::new(MinimaxTransformer));
        registry.register("openrouter", Arc::new(OpenRouterTransformer));
        registry.register("tooluse", Arc::new(ToolUseTransformer));
        registry.register("identity", Arc::new(IdentityTransformer));
        registry.register("reasoning", Arc::new(ReasoningTransformer));
        registry.register("enhancetool", Arc::new(EnhanceToolTransformer));
        registry.register("thinktag", Arc::new(ThinkTagTransformer));
        registry.register("glm", Arc::new(GlmTransformer::default()));

        registry
    }

    /// Register a transformer with a given name.
    pub fn register(&mut self, name: &str, transformer: Arc<dyn Transformer>) {
        self.transformers.insert(name.to_string(), transformer);
    }

    /// Look up a transformer by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Transformer>> {
        self.transformers.get(name).cloned()
    }

    /// Create a transformer with options.
    ///
    /// Some transformers accept configuration via a JSON options object.
    pub fn create_with_options(&self, name: &str, options: &Value) -> Option<Arc<dyn Transformer>> {
        match name {
            "maxtoken" => {
                if let Some(max_tokens) = options.get("max_tokens").and_then(|v| v.as_u64()) {
                    return Some(Arc::new(MaxTokenTransformer::new(max_tokens as u32)));
                }
                // Default to 65536 if not specified
                Some(Arc::new(MaxTokenTransformer::new(65536)))
            }
            _ => self.get(name),
        }
    }

    /// Build a transformer chain from a list of transformer entries.
    ///
    /// Returns an empty chain if no entries are provided.
    pub fn build_chain(&self, entries: &[TransformerEntry]) -> TransformerChain {
        let mut chain = TransformerChain::new();
        for entry in entries {
            if let Some(transformer) = entry
                .options()
                .and_then(|opts| self.create_with_options(entry.name(), opts))
                .or_else(|| self.get(entry.name()))
            {
                chain = chain.with_transformer(transformer);
            } else {
                tracing::warn!(name = entry.name(), "transformer not found, skipping");
            }
        }
        chain
    }

    /// Check if all entries in the list are valid transformers.
    #[allow(dead_code)]
    pub fn validate_entries(&self, entries: &[TransformerEntry]) -> Vec<String> {
        let mut errors = Vec::new();
        for entry in entries {
            if self.get(entry.name()).is_none() && !matches!(entry.name(), "maxtoken") {
                errors.push(format!("Unknown transformer: {}", entry.name()));
            }
        }
        errors
    }
}

impl Default for TransformerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Simple timestamp-based UUID generation for tool IDs.
/// Uses a simple counter-based approach to avoid external dependencies.
pub(crate) mod uuid_nopanic {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn timestamp_ms() -> u64 {
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> Value {
        serde_json::json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        })
    }

    #[test]
    fn identity_passthrough() {
        let transformer = IdentityTransformer;
        let request = test_request();
        assert!(transformer.is_passthrough(&request));
        assert_eq!(transformer.name(), "identity");
    }

    #[test]
    fn anthropic_passthrough() {
        let transformer = AnthropicTransformer;
        let request = test_request();
        assert!(transformer.is_passthrough(&request));
        assert_eq!(transformer.name(), "anthropic");
    }

    #[test]
    fn maxtoken_caps_request() {
        let transformer = MaxTokenTransformer::new(5000);
        let mut request = test_request();
        request["max_tokens"] = Value::Number(10000.into());

        let result = transformer.transform_request(request).unwrap();
        assert_eq!(result["max_tokens"], 5000);
    }

    #[test]
    fn maxtoken_adds_if_missing() {
        let transformer = MaxTokenTransformer::new(5000);
        let mut request = test_request();
        request.as_object_mut().unwrap().remove("max_tokens");

        let result = transformer.transform_request(request).unwrap();
        assert_eq!(result["max_tokens"], 5000);
    }

    #[test]
    fn tooluse_adds_id_to_tool_use() {
        let transformer = ToolUseTransformer;
        let response = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "calculator", "input": {}}
            ]
        });

        let result = transformer.transform_response(response).unwrap();
        let tool_use = result["content"][0].as_object().unwrap();
        assert!(tool_use.contains_key("id"));
        assert!(tool_use["id"].as_str().unwrap().starts_with("toolu_"));
    }

    #[test]
    fn transformer_chain_applies_in_order() {
        let chain = TransformerChain::new()
            .with_transformer(Arc::new(MaxTokenTransformer::new(100)))
            .with_transformer(Arc::new(IdentityTransformer));

        let mut request = test_request();
        request["max_tokens"] = Value::Number(1000.into());

        let result = chain.apply_request(request).unwrap();
        assert_eq!(result["max_tokens"], 100);
    }

    #[test]
    fn transformer_chain_reverses_for_response() {
        let chain = TransformerChain::new()
            .with_transformer(Arc::new(ToolUseTransformer))
            .with_transformer(Arc::new(DeepSeekTransformer));

        let response = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "test", "input": {}}
            ]
        });

        let result = chain.apply_response(response).unwrap();
        // ToolUseTransformer should add the ID
        assert!(result["content"][0].as_object().unwrap().contains_key("id"));
    }

    #[test]
    fn registry_gets_transformer() {
        let registry = TransformerRegistry::new();
        assert!(registry.get("anthropic").is_some());
        assert!(registry.get("deepseek").is_some());
        assert!(registry.get("minimax").is_some());
        assert!(registry.get("unknown").is_none());
    }

    #[test]
    fn registry_builds_chain() {
        let registry = TransformerRegistry::new();
        let entries = vec![
            TransformerEntry::Name("anthropic".to_string()),
            TransformerEntry::Name("tooluse".to_string()),
        ];

        let chain = registry.build_chain(&entries);
        assert_eq!(chain.len(), 2);
        assert!(!chain.is_empty());
    }

    #[test]
    fn registry_creates_maxtoken_with_options() {
        let registry = TransformerRegistry::new();
        let options = serde_json::json!({"max_tokens": 12345});
        let transformer = registry.create_with_options("maxtoken", &options);

        assert!(transformer.is_some());
        assert_eq!(transformer.unwrap().name(), "maxtoken");
    }

    #[test]
    fn registry_validates_entries() {
        let registry = TransformerRegistry::new();
        let entries = vec![
            TransformerEntry::Name("anthropic".to_string()),
            TransformerEntry::Name("unknown_transformer".to_string()),
        ];

        let errors = registry.validate_entries(&entries);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("unknown_transformer"));
    }

    #[test]
    fn chain_empty_is_passthrough() {
        let chain = TransformerChain::new();
        assert!(chain.is_passthrough(&test_request()));
        assert!(chain.is_empty());
    }

    #[test]
    fn thinktag_strips_blocks() {
        let t = ThinkTagTransformer;
        let think_text = format!(
            "{}think{}hidden content{}/think{}Before After",
            '<', '>', '<', '>'
        );
        let resp = serde_json::json!({
            "content": [{"type": "text", "text": think_text}]
        });
        let result = t.transform_response(resp).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("<think>"), "Should strip think tags");
        assert!(
            !text.contains("hidden content"),
            "Should strip think content"
        );
        assert!(text.contains("Before") && text.contains("After"));
    }

    #[test]
    fn anthropic_to_openai_converts_string_system() {
        let transformer = AnthropicToOpenaiTransformer;
        let request = serde_json::json!({
            "model": "claude-3",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(request).unwrap();

        // System field should be removed
        assert!(result.get("system").is_none());

        // System message should be prepended to messages
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a helpful assistant.");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn anthropic_to_openai_converts_array_system() {
        let transformer = AnthropicToOpenaiTransformer;
        let request = serde_json::json!({
            "model": "claude-3",
            "system": [
                {"type": "text", "text": "First instruction"},
                {"type": "text", "text": "Second instruction"}
            ],
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(request).unwrap();

        // System field should be removed
        assert!(result.get("system").is_none());

        // System message should be prepended to messages with combined text
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[0]["content"],
            "First instruction\n\nSecond instruction"
        );
    }

    #[test]
    fn anthropic_to_openai_system_creates_messages_if_missing() {
        let transformer = AnthropicToOpenaiTransformer;
        let request = serde_json::json!({
            "model": "claude-3",
            "system": "You are helpful",
            "max_tokens": 1000
        });

        let result = transformer.transform_request(request).unwrap();

        // System message should create messages array
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");
    }

    #[test]
    fn anthropic_to_openai_no_system_field_passthrough() {
        let transformer = AnthropicToOpenaiTransformer;
        let request = test_request();

        let result = transformer.transform_request(request).unwrap();

        // Should have original messages without system prefix
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }
}
