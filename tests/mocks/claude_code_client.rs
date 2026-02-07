//! Mock Claude Code client for testing.
//!
//! Provides helper functions to generate realistic Anthropic API format requests.
//! These mocks follow the Anthropic Messages API specification.

use serde_json::{json, Value};

/// Generate a basic Anthropic messages request.
///
/// # Arguments
/// * `model` - The model identifier (e.g., "claude-3-5-sonnet-20241022")
/// * `messages` - Array of message objects with role and content
///
/// # Returns
/// JSON Value representing a complete Anthropic messages request
///
/// # Example
/// ```rust
/// let request = generate_messages_request(
///     "claude-3-5-sonnet-20241022",
///     json!([{"role": "user", "content": "Hello!"}])
/// );
/// ```
pub fn generate_messages_request(model: &str, messages: Value) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "max_tokens": 4096,
        "temperature": 0.7,
        "stream": false
    })
}

/// Generate a messages request with streaming enabled.
///
/// # Arguments
/// * `model` - The model identifier
/// * `messages` - Array of message objects
///
/// # Returns
/// JSON Value representing a streaming Anthropic messages request
pub fn generate_streaming_request(model: &str, messages: Value) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "max_tokens": 4096,
        "temperature": 0.7,
        "stream": true
    })
}

/// Generate a messages request with thinking enabled.
///
/// Creates a request that enables Claude's extended thinking capability
/// for complex reasoning tasks.
///
/// # Returns
/// JSON Value representing an Anthropic messages request with thinking
///
/// # Example
/// ```rust
/// let request = generate_thinking_request();
/// ```
pub fn generate_thinking_request() -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": "Solve this step by step: What is the derivative of x^3 + 2x^2 - 5x + 1?"
            }
        ],
        "max_tokens": 8192,
        "thinking": {
            "type": "enabled",
            "budget_tokens": 4000
        },
        "stream": false
    })
}

/// Generate a messages request with thinking and custom budget.
///
/// # Arguments
/// * `budget_tokens` - Number of tokens allocated for thinking (max 32000)
///
/// # Returns
/// JSON Value representing a thinking-enabled request
pub fn generate_thinking_request_with_budget(budget_tokens: u32) -> Value {
    json!({
        "model": "claude-3-7-sonnet-20250219",
        "messages": [
            {
                "role": "user",
                "content": "Analyze the trade-offs between different database sharding strategies."
            }
        ],
        "max_tokens": 16384,
        "thinking": {
            "type": "enabled",
            "budget_tokens": budget_tokens
        },
        "stream": false
    })
}

/// Generate a messages request with tool definitions.
///
/// # Arguments
/// * `tools` - Array of tool definition objects
///
/// # Returns
/// JSON Value representing an Anthropic messages request with tools
///
/// # Example
/// ```rust
/// let tools = json!([
///     {
///         "name": "calculator",
///         "description": "Perform mathematical calculations",
///         "input_schema": { "type": "object", "properties": {} }
///     }
/// ]);
/// let request = generate_tool_use_request(tools);
/// ```
pub fn generate_tool_use_request(tools: Value) -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": "What is 1234 * 5678?"
            }
        ],
        "max_tokens": 4096,
        "tools": tools,
        "tool_choice": {
            "type": "auto"
        },
        "stream": false
    })
}

/// Generate a messages request that forces a specific tool.
///
/// # Arguments
/// * `tool_name` - Name of the tool to force
///
/// # Returns
/// JSON Value representing a request with forced tool choice
pub fn generate_forced_tool_request(tool_name: &str) -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": "Calculate 2 + 2"
            }
        ],
        "max_tokens": 4096,
        "tools": [
            {
                "name": tool_name,
                "description": "Perform arithmetic operations",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["add", "subtract", "multiply", "divide"]
                        },
                        "a": {"type": "number"},
                        "b": {"type": "number"}
                    },
                    "required": ["operation", "a", "b"]
                }
            }
        ],
        "tool_choice": {
            "type": "tool",
            "name": tool_name
        },
        "stream": false
    })
}

/// Generate a request with a system prompt.
///
/// # Arguments
/// * `system` - System prompt (string or array of content blocks)
///
/// # Returns
/// JSON Value representing a request with system prompt
pub fn generate_request_with_system(system: Value) -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "system": system,
        "messages": [
            {
                "role": "user",
                "content": "Hello!"
            }
        ],
        "max_tokens": 4096,
        "stream": false
    })
}

/// Generate a request with multimodal content (image).
///
/// # Returns
/// JSON Value representing a request with image content
pub fn generate_multimodal_request() -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What do you see in this image?"
                    },
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
                        }
                    }
                ]
            }
        ],
        "max_tokens": 4096,
        "stream": false
    })
}

/// Generate a request with stop sequences.
///
/// # Arguments
/// * `stop_sequences` - Array of strings to stop generation
///
/// # Returns
/// JSON Value representing a request with stop sequences
pub fn generate_request_with_stop_sequences(stop_sequences: Vec<&str>) -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": "Count from 1 to 10"
            }
        ],
        "max_tokens": 4096,
        "stop_sequences": stop_sequences,
        "stream": false
    })
}

/// Generate a complete conversation request with multiple turns.
///
/// # Returns
/// JSON Value representing a multi-turn conversation
pub fn generate_conversation_request() -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": "My name is Alice."
            },
            {
                "role": "assistant",
                "content": "Hello Alice! It's nice to meet you. How can I help you today?"
            },
            {
                "role": "user",
                "content": "What's my name?"
            }
        ],
        "max_tokens": 4096,
        "stream": false
    })
}

/// Generate a request with metadata (user_id for tracking).
///
/// # Arguments
/// * `user_id` - User identifier for request tracking
///
/// # Returns
/// JSON Value representing a request with metadata
pub fn generate_request_with_metadata(user_id: &str) -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "messages": [
            {
                "role": "user",
                "content": "Hello!"
            }
        ],
        "max_tokens": 4096,
        "metadata": {
            "user_id": user_id
        },
        "stream": false
    })
}

/// Generate a request with all available parameters.
///
/// # Returns
/// JSON Value representing a request with all optional parameters set
pub fn generate_full_featured_request() -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "system": "You are a helpful coding assistant.",
        "messages": [
            {
                "role": "user",
                "content": "Refactor this function"
            }
        ],
        "max_tokens": 4096,
        "temperature": 0.5,
        "top_p": 0.9,
        "top_k": 50,
        "stop_sequences": ["STOP", "END"],
        "stream": false,
        "tools": [
            {
                "name": "analyze_code",
                "description": "Analyze code for issues",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "code": {"type": "string"}
                    },
                    "required": ["code"]
                }
            }
        ],
        "tool_choice": {
            "type": "auto"
        },
        "metadata": {
            "user_id": "user_12345"
        }
    })
}

/// Generate a batch of common calculator tool definitions.
///
/// # Returns
/// JSON Value representing calculator tools
pub fn generate_calculator_tools() -> Value {
    json!([
        {
            "name": "calculator",
            "description": "Perform basic arithmetic operations",
            "input_schema": {
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
            }
        },
        {
            "name": "sqrt",
            "description": "Calculate square root of a number",
            "input_schema": {
                "type": "object",
                "properties": {
                    "value": {
                        "type": "number",
                        "description": "The number to calculate square root of",
                        "minimum": 0
                    }
                },
                "required": ["value"]
            }
        }
    ])
}

/// Generate weather tool definitions for testing.
///
/// # Returns
/// JSON Value representing weather tools
pub fn generate_weather_tools() -> Value {
    json!([
        {
            "name": "get_weather",
            "description": "Get current weather for a location",
            "input_schema": {
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City name or location"
                    },
                    "units": {
                        "type": "string",
                        "enum": ["celsius", "fahrenheit"],
                        "description": "Temperature units",
                        "default": "celsius"
                    }
                },
                "required": ["location"]
            }
        },
        {
            "name": "get_forecast",
            "description": "Get weather forecast for upcoming days",
            "input_schema": {
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City name or location"
                    },
                    "days": {
                        "type": "integer",
                        "description": "Number of days for forecast",
                        "minimum": 1,
                        "maximum": 14,
                        "default": 7
                    }
                },
                "required": ["location", "days"]
            }
        }
    ])
}

/// Generate file operation tool definitions.
///
/// # Returns
/// JSON Value representing file operation tools
pub fn generate_file_tools() -> Value {
    json!([
        {
            "name": "read_file",
            "description": "Read contents of a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line offset to start reading from",
                        "minimum": 0,
                        "default": 0
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read",
                        "minimum": 1,
                        "default": 100
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "write_file",
            "description": "Write content to a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Whether to append instead of overwrite",
                        "default": false
                    }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "list_directory",
            "description": "List contents of a directory",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory to list"
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "Whether to list recursively",
                        "default": false
                    }
                },
                "required": ["path"]
            }
        }
    ])
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_messages_request() {
        let request = generate_messages_request(
            "claude-3-5-sonnet-20241022",
            json!([{"role": "user", "content": "Hello!"}]),
        );

        assert_eq!(request["model"], "claude-3-5-sonnet-20241022");
        assert_eq!(request["messages"][0]["role"], "user");
        assert_eq!(request["messages"][0]["content"], "Hello!");
        assert_eq!(request["max_tokens"], 4096);
        assert_eq!(request["temperature"], 0.7);
        assert_eq!(request["stream"], false);
    }

    #[test]
    fn test_generate_thinking_request() {
        let request = generate_thinking_request();

        assert_eq!(request["model"], "claude-3-5-sonnet-20241022");
        assert!(request.get("thinking").is_some());
        assert_eq!(request["thinking"]["type"], "enabled");
        assert_eq!(request["thinking"]["budget_tokens"], 4000);
        assert_eq!(request["max_tokens"], 8192);
    }

    #[test]
    fn test_generate_thinking_request_with_budget() {
        let request = generate_thinking_request_with_budget(8000);

        assert_eq!(request["thinking"]["budget_tokens"], 8000);
        assert_eq!(request["model"], "claude-3-7-sonnet-20250219");
    }

    #[test]
    fn test_generate_tool_use_request() {
        let tools = generate_calculator_tools();
        let request = generate_tool_use_request(tools);

        assert!(request.get("tools").is_some());
        assert_eq!(request["tools"].as_array().unwrap().len(), 2);
        assert_eq!(request["tools"][0]["name"], "calculator");
        assert_eq!(request["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(request["tool_choice"]["type"], "auto");
    }

    #[test]
    fn test_generate_forced_tool_request() {
        let request = generate_forced_tool_request("calculator");

        assert_eq!(request["tool_choice"]["type"], "tool");
        assert_eq!(request["tool_choice"]["name"], "calculator");
    }

    #[test]
    fn test_generate_request_with_system() {
        let request = generate_request_with_system(json!("You are a helpful assistant."));

        assert_eq!(request["system"], "You are a helpful assistant.");
        assert!(request.get("messages").is_some());
    }

    #[test]
    fn test_generate_request_with_system_array() {
        let system = json!([
            {"type": "text", "text": "You are a coding assistant."},
            {"type": "text", "text": "Be concise."}
        ]);
        let request = generate_request_with_system(system);

        assert!(request["system"].is_array());
        assert_eq!(request["system"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_generate_multimodal_request() {
        let request = generate_multimodal_request();

        let content = request["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
    }

    #[test]
    fn test_generate_request_with_stop_sequences() {
        let stops = vec!["STOP", "END", "HALT"];
        let request = generate_request_with_stop_sequences(stops);

        let stop_seqs = request["stop_sequences"].as_array().unwrap();
        assert_eq!(stop_seqs.len(), 3);
        assert_eq!(stop_seqs[0], "STOP");
        assert_eq!(stop_seqs[1], "END");
        assert_eq!(stop_seqs[2], "HALT");
    }

    #[test]
    fn test_generate_conversation_request() {
        let request = generate_conversation_request();

        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[2]["role"], "user");
    }

    #[test]
    fn test_generate_request_with_metadata() {
        let request = generate_request_with_metadata("user_abc123");

        assert_eq!(request["metadata"]["user_id"], "user_abc123");
    }

    #[test]
    fn test_generate_streaming_request() {
        let request = generate_streaming_request(
            "claude-3-opus-20240229",
            json!([{"role": "user", "content": "Hi"}]),
        );

        assert_eq!(request["stream"], true);
        assert_eq!(request["model"], "claude-3-opus-20240229");
    }

    #[test]
    fn test_generate_full_featured_request() {
        let request = generate_full_featured_request();

        assert!(request.get("system").is_some());
        assert!(request.get("temperature").is_some());
        assert!(request.get("top_p").is_some());
        assert!(request.get("top_k").is_some());
        assert!(request.get("stop_sequences").is_some());
        assert!(request.get("tools").is_some());
        assert!(request.get("tool_choice").is_some());
        assert!(request.get("metadata").is_some());
    }

    #[test]
    fn test_generate_calculator_tools() {
        let tools = generate_calculator_tools();
        let tools_arr = tools.as_array().unwrap();

        assert_eq!(tools_arr.len(), 2);
        assert_eq!(tools_arr[0]["name"], "calculator");
        assert_eq!(tools_arr[1]["name"], "sqrt");
        
        // Verify input_schema structure
        let schema = &tools_arr[0]["input_schema"];
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["operation"]["enum"].is_array());
    }

    #[test]
    fn test_generate_weather_tools() {
        let tools = generate_weather_tools();
        let tools_arr = tools.as_array().unwrap();

        assert_eq!(tools_arr.len(), 2);
        assert_eq!(tools_arr[0]["name"], "get_weather");
        assert_eq!(tools_arr[1]["name"], "get_forecast");
        
        // Check default values in schema
        assert_eq!(tools_arr[0]["input_schema"]["properties"]["units"]["default"], "celsius");
    }

    #[test]
    fn test_generate_file_tools() {
        let tools = generate_file_tools();
        let tools_arr = tools.as_array().unwrap();

        assert_eq!(tools_arr.len(), 3);
        assert_eq!(tools_arr[0]["name"], "read_file");
        assert_eq!(tools_arr[1]["name"], "write_file");
        assert_eq!(tools_arr[2]["name"], "list_directory");
    }
}
