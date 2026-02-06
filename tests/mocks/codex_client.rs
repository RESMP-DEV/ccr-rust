//! Mock Codex CLI client for testing OpenAI API format requests.
//!
//! This module provides helper functions to generate realistic OpenAI API request
//! payloads for testing the ccr-rust router and transformers.

use serde_json::{json, Value};

/// Generate a standard chat completion request in OpenAI format.
///
/// # Arguments
/// * `model` - The model identifier (e.g., "gpt-4", "codex-mini")
/// * `messages` - Array of message objects with "role" and "content" fields
///
/// # Returns
/// A JSON Value representing a complete OpenAI chat completion request.
///
/// # Example
/// ```
/// let messages = vec![
///     json!({"role": "user", "content": "Hello, world!"}),
/// ];
/// let request = generate_chat_request("gpt-4", &messages);
/// ```
pub fn generate_chat_request(model: &str, messages: &[Value]) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "max_tokens": 1024,
        "temperature": 0.7,
        "top_p": 1.0,
        "frequency_penalty": 0.0,
        "presence_penalty": 0.0,
        "stream": false
    })
}

/// Generate a chat completion request with tool definitions.
///
/// # Arguments
/// * `tools` - Array of tool definition objects following OpenAI function schema
/// * `messages` - Array of message objects with "role" and "content" fields
///
/// # Returns
/// A JSON Value representing a complete OpenAI tool-enabled request.
///
/// # Example
/// ```
/// let tools = vec![
///     json!({
///         "type": "function",
///         "function": {
///             "name": "calculator",
///             "description": "Perform basic arithmetic",
///             "parameters": {
///                 "type": "object",
///                 "properties": {
///                     "a": {"type": "number"},
///                     "b": {"type": "number"}
///                 },
///                 "required": ["a", "b"]
///             }
///         }
///     }),
/// ];
/// let request = generate_tool_request(&tools, &messages);
/// ```
pub fn generate_tool_request(tools: &[Value], messages: &[Value]) -> Value {
    json!({
        "model": "gpt-4-turbo",
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "max_tokens": 1024,
        "temperature": 0.7,
        "stream": false
    })
}

/// Generate a streaming chat completion request.
///
/// Creates a request with `stream: true` for SSE (Server-Sent Events) responses.
/// Optionally includes tools and custom parameters.
///
/// # Arguments
/// * `model` - The model identifier
/// * `messages` - Array of message objects
/// * `tools` - Optional array of tool definitions (pass &[] for no tools)
///
/// # Returns
/// A JSON Value representing a streaming OpenAI chat completion request.
///
/// # Example
/// ```
/// let request = generate_streaming_request("gpt-4", &messages, &[]);
/// ```
pub fn generate_streaming_request(model: &str, messages: &[Value], tools: &[Value]) -> Value {
    let mut request = json!({
        "model": model,
        "messages": messages,
        "max_tokens": 1024,
        "temperature": 0.7,
        "stream": true
    });

    if !tools.is_empty() {
        request["tools"] = json!(tools);
        request["tool_choice"] = json!("auto");
    }

    request
}

/// Generate a complete tool definition for a function.
///
/// Helper function to create properly formatted OpenAI tool definitions.
///
/// # Arguments
/// * `name` - The function name
/// * `description` - Human-readable description of what the function does
/// * `parameters` - JSON Schema for the function parameters
///
/// # Returns
/// A JSON Value representing a complete tool definition.
pub fn create_tool_definition(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters
        }
    })
}

/// Generate a system message.
pub fn system_message(content: &str) -> Value {
    json!({"role": "system", "content": content})
}

/// Generate a user message.
pub fn user_message(content: &str) -> Value {
    json!({"role": "user", "content": content})
}

/// Generate an assistant message.
pub fn assistant_message(content: &str) -> Value {
    json!({"role": "assistant", "content": content})
}

/// Generate a tool message (response to a tool call).
pub fn tool_message(tool_call_id: &str, content: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": content
    })
}

/// Generate an assistant message with tool calls.
pub fn assistant_with_tools(content: &str, tool_calls: &[Value]) -> Value {
    json!({
        "role": "assistant",
        "content": content,
        "tool_calls": tool_calls
    })
}

/// Create a tool call object for assistant messages.
pub fn create_tool_call(id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments
        }
    })
}

/// Generate common tool definitions for testing.
pub mod preset_tools {
    use super::*;

    /// Calculator tool for basic arithmetic operations.
    pub fn calculator() -> Value {
        create_tool_definition(
            "calculator",
            "Perform basic arithmetic operations: add, subtract, multiply, divide",
            json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["add", "subtract", "multiply", "divide"],
                        "description": "The arithmetic operation to perform"
                    },
                    "a": {
                        "type": "number",
                        "description": "First operand"
                    },
                    "b": {
                        "type": "number",
                        "description": "Second operand"
                    }
                },
                "required": ["operation", "a", "b"]
            }),
        )
    }

    /// Weather tool for getting weather information.
    pub fn weather() -> Value {
        create_tool_definition(
            "get_weather",
            "Get current weather information for a location",
            json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City name or coordinates"
                    },
                    "unit": {
                        "type": "string",
                        "enum": ["celsius", "fahrenheit"],
                        "description": "Temperature unit"
                    }
                },
                "required": ["location"]
            }),
        )
    }

    /// File read tool for reading file contents.
    pub fn file_read() -> Value {
        create_tool_definition(
            "read_file",
            "Read contents of a file at the given path",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative path to the file"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line offset to start reading from (0-indexed)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read"
                    }
                },
                "required": ["path"]
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_chat_request() {
        let messages = vec![user_message("Hello")];
        let request = generate_chat_request("gpt-4", &messages);

        assert_eq!(request["model"], "gpt-4");
        assert_eq!(request["messages"].as_array().unwrap().len(), 1);
        assert_eq!(request["stream"], false);
        assert_eq!(request["max_tokens"], 1024);
    }

    #[test]
    fn test_generate_tool_request() {
        let tools = vec![preset_tools::calculator()];
        let messages = vec![
            system_message("You are a helpful assistant."),
            user_message("What is 2 + 2?"),
        ];
        let request = generate_tool_request(&tools, &messages);

        assert_eq!(request["tools"].as_array().unwrap().len(), 1);
        assert_eq!(request["tool_choice"], "auto");
        assert_eq!(request["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_generate_streaming_request() {
        let messages = vec![user_message("Tell me a story")];
        let request = generate_streaming_request("gpt-4", &messages, &[]);

        assert_eq!(request["stream"], true);
        assert_eq!(request["model"], "gpt-4");
        assert!(request.get("tools").is_none());
    }

    #[test]
    fn test_generate_streaming_request_with_tools() {
        let tools = vec![preset_tools::weather()];
        let messages = vec![user_message("What's the weather in Tokyo?")];
        let request = generate_streaming_request("gpt-4-turbo", &messages, &tools);

        assert_eq!(request["stream"], true);
        assert!(request.get("tools").is_some());
        assert_eq!(request["tool_choice"], "auto");
    }

    #[test]
    fn test_calculator_tool_definition() {
        let tool = preset_tools::calculator();

        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "calculator");
        assert!(tool["function"]["description"]
            .as_str()
            .unwrap()
            .contains("arithmetic"));
    }

    #[test]
    fn test_message_helpers() {
        let sys = system_message("System prompt");
        assert_eq!(sys["role"], "system");
        assert_eq!(sys["content"], "System prompt");

        let user = user_message("User input");
        assert_eq!(user["role"], "user");

        let assistant = assistant_message("Assistant response");
        assert_eq!(assistant["role"], "assistant");

        let tool_msg = tool_message("call_123", "Tool result");
        assert_eq!(tool_msg["role"], "tool");
        assert_eq!(tool_msg["tool_call_id"], "call_123");
    }

    #[test]
    fn test_assistant_with_tools() {
        let tool_call = create_tool_call("call_abc", "calculator", r#"{"a": 1, "b": 2}"#);
        let msg = assistant_with_tools("I'll calculate that.", &[tool_call]);

        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "I'll calculate that.");
        assert_eq!(msg["tool_calls"].as_array().unwrap().len(), 1);
        assert_eq!(msg["tool_calls"][0]["id"], "call_abc");
    }

    #[test]
    fn test_full_conversation_with_tools() {
        // Simulate a complete tool-use conversation flow
        let tools = vec![preset_tools::calculator()];

        let conversation = vec![
            system_message("You are a helpful math assistant."),
            user_message("Calculate 10 divided by 2"),
            assistant_with_tools(
                "I'll calculate that for you.",
                &[create_tool_call(
                    "call_1",
                    "calculator",
                    r#"{"operation": "divide", "a": 10, "b": 2}"#,
                )],
            ),
            tool_message("call_1", "5"),
            assistant_message("10 divided by 2 equals 5."),
        ];

        let request = generate_tool_request(&tools, &conversation);
        assert_eq!(request["messages"].as_array().unwrap().len(), 5);
    }
}
