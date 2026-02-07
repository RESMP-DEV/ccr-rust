//! Frontend detection and routing module.
//!
//! This module provides the [`Frontend`] trait for implementing API format frontends
//! and the normalized [`InternalRequest`]/[`InternalResponse`] types that bridge
//! OpenAI and Anthropic API formats.

use anyhow::Result;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod claude_code;
pub mod codex;
pub mod detection;

pub use detection::{detect_frontend, FrontendType};

/// A normalized message format that works for both OpenAI and Anthropic.
///
/// This represents a single message in a conversation, with a role and content.
/// Content can be a simple string or an array of content blocks for multimodal support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message sender (e.g., "user", "assistant", "system", "tool")
    pub role: String,
    /// The content of the message - can be a string or array of content blocks
    pub content: Value,
    /// Optional tool call ID for tool result messages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A content block representing a piece of message content.
///
/// This enum covers the various content types supported by different APIs:
/// - Text: Simple text content
/// - Image: Image data (base64 or URL)
/// - ToolUse: A request to use a tool (Anthropic format)
/// - ToolResult: The result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Text content block
    #[serde(rename = "text")]
    Text { text: String },
    /// Image content block
    #[serde(rename = "image")]
    Image { source: ImageSource },
    /// Tool use request (Anthropic style)
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Tool result (response from tool execution)
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: Value },
    /// Thinking/reasoning content (for reasoning models)
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

/// Image source specification for image content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded image data
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    /// Image URL
    #[serde(rename = "url")]
    Url { url: String },
}

/// Tool definition for function calling.
///
/// Normalized format that works across OpenAI and Anthropic:
/// - OpenAI: `{type: "function", function: {name, description, parameters}}`
/// - Anthropic: `{name, description, input_schema}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// The name of the tool
    pub name: String,
    /// Description of what the tool does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON schema for the tool's input parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

/// Usage statistics for a request/response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    /// Number of tokens in the prompt
    pub input_tokens: u64,
    /// Number of tokens in the completion
    pub output_tokens: u64,
    /// Breakdown of input tokens (e.g., cache hits/misses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<Value>,
}

/// Internal normalized request format.
///
/// This struct represents a unified view of chat completion requests that works
/// for both OpenAI's `/v1/chat/completions` and Anthropic's `/v1/messages` APIs.
/// Frontends are responsible for parsing their specific format into this internal
/// representation, and the router works with this normalized format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalRequest {
    /// The model identifier (may include provider prefix like "anthropic,claude-3-opus")
    pub model: String,
    /// The conversation messages
    pub messages: Vec<Message>,
    /// Optional system prompt (extracted from messages for OpenAI format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Value>,
    /// Maximum number of tokens to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0 to 2.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Whether to stream the response
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Tools available for function calling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Tool choice configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    /// Stop sequences to halt generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Provider-specific additional parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_params: Option<Value>,
}

/// Internal normalized response format.
///
/// This struct represents a unified view of chat completion responses that works
/// for both OpenAI and Anthropic APIs. Frontends serialize this into their
/// specific response format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalResponse {
    /// Unique identifier for the response
    pub id: String,
    /// The type of response (usually "message")
    #[serde(rename = "type")]
    pub response_type: String,
    /// The role of the responder (usually "assistant")
    pub role: String,
    /// The model used for generation
    pub model: String,
    /// Content blocks in the response
    pub content: Vec<ContentBlock>,
    /// Reason why generation stopped
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Token usage statistics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Provider-specific additional data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_data: Option<Value>,
}

/// A trait for frontend implementations.
///
/// Frontends handle the parsing and serialization of API-specific request/response
/// formats into/from the internal normalized format. This allows the router to work
/// with a single internal representation while supporting multiple API formats.
///
/// # Type Parameters
///
/// Implementations must be `Send + Sync` to allow use across async boundaries.
///
/// # Example
///
/// ```rust,ignore
/// use ccr_rust::frontend::{Frontend, InternalRequest, InternalResponse};
/// use anyhow::Result;
/// use axum::http::HeaderMap;
/// use serde_json::Value;
///
/// struct MyFrontend;
///
/// impl Frontend for MyFrontend {
///     fn name(&self) -> &str {
///         "my_frontend"
///     }
///
///     fn detect(&self, headers: &HeaderMap, body: &Value) -> bool {
///         // Detection logic
///         true
///     }
///
///     fn parse_request(&self, body: Value) -> Result<InternalRequest> {
///         // Parse into internal format
///         todo!()
///     }
///
///     fn serialize_response(&self, response: InternalResponse) -> Result<Vec<u8>> {
///         // Serialize from internal format
///         todo!()
///     }
/// }
/// ```
pub trait Frontend: Send + Sync {
    /// Returns the name of the frontend.
    ///
    /// This is used for logging, metrics, and identification purposes.
    fn name(&self) -> &str;

    /// Detects if the request belongs to this frontend.
    ///
    /// Examines HTTP headers and the request body to determine if this frontend
    /// should handle the request. Detection should be fast and non-allocating
    /// where possible.
    ///
    /// # Arguments
    ///
    /// * `headers` - HTTP request headers
    /// * `body` - The parsed JSON request body
    ///
    /// # Returns
    ///
    /// `true` if this frontend should handle the request, `false` otherwise.
    fn detect(&self, headers: &HeaderMap, body: &Value) -> bool;

    /// Parses an incoming request body into the internal normalized format.
    ///
    /// This method converts the frontend-specific request format (OpenAI or Anthropic)
    /// into the internal `InternalRequest` representation that the router uses.
    ///
    /// # Arguments
    ///
    /// * `body` - The parsed JSON request body
    ///
    /// # Returns
    ///
    /// A `Result` containing the parsed `InternalRequest` or an error if parsing fails.
    fn parse_request(&self, body: Value) -> Result<InternalRequest>;

    /// Serializes an internal response into the frontend's response format.
    ///
    /// This method converts the internal `InternalResponse` representation into
    /// the frontend-specific response format (OpenAI or Anthropic) for transmission
    /// to the client.
    ///
    /// # Arguments
    ///
    /// * `response` - The internal response to serialize
    ///
    /// # Returns
    ///
    /// A `Result` containing the serialized response as a byte vector (usually JSON).
    fn serialize_response(&self, response: InternalResponse) -> Result<Vec<u8>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_creation() {
        let msg = Message {
            role: "user".to_string(),
            content: Value::String("Hello".to_string()),
            tool_call_id: None,
        };
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "Hello");
    }

    #[test]
    fn test_content_block_text() {
        let block = ContentBlock::Text {
            text: "Hello world".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"Hello world\""));
    }

    #[test]
    fn test_content_block_tool_use() {
        let block = ContentBlock::ToolUse {
            id: "tool_123".to_string(),
            name: "calculator".to_string(),
            input: serde_json::json!({"a": 1, "b": 2}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"id\":\"tool_123\""));
    }

    #[test]
    fn test_internal_request_defaults() {
        let req = InternalRequest {
            model: "gpt-4".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String("Hi".to_string()),
                tool_call_id: None,
            }],
            system: None,
            max_tokens: None,
            temperature: None,
            stream: None,
            tools: None,
            tool_choice: None,
            stop_sequences: None,
            extra_params: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"model\":\"gpt-4\""));
        // Optional fields should be skipped when None
        assert!(!json.contains("system"));
        assert!(!json.contains("max_tokens"));
    }

    #[test]
    fn test_internal_response_serialization() {
        let resp = InternalResponse {
            id: "msg_123".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-3-opus".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                input_tokens_details: None,
            }),
            extra_data: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"id\":\"msg_123\""));
        assert!(json.contains("\"type\":\"message\""));
        assert!(json.contains("\"input_tokens\":10"));
    }

    #[test]
    fn test_usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_image_source_base64() {
        let source = ImageSource::Base64 {
            media_type: "image/jpeg".to_string(),
            data: "base64data".to_string(),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("\"type\":\"base64\""));
        assert!(json.contains("\"media_type\":\"image/jpeg\""));
    }

    #[test]
    fn test_image_source_url() {
        let source = ImageSource::Url {
            url: "https://example.com/image.jpg".to_string(),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("\"type\":\"url\""));
        assert!(json.contains("\"url\":\"https://example.com/image.jpg\""));
    }
}
