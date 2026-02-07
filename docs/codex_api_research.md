# Codex CLI API Research

This document documents the JSON request and response schemas for OpenAI Codex CLI and compatible APIs.

## Overview

Current upstream Codex CLI uses the OpenAI Responses API as its wire protocol. In current `openai/codex` source, `wire_api = "chat"` is no longer supported.

## Endpoints

Verified against `openai/codex` `main` on 2026-02-06.

### 1) Endpoint used by Codex CLI

- Current endpoint pattern is `{base_url}/responses`.
- Default OpenAI endpoint is `https://api.openai.com/v1/responses`.
- In ChatGPT auth mode, endpoint base is `https://chatgpt.com/backend-api/codex`, so requests go to `https://chatgpt.com/backend-api/codex/responses`.
- WebSocket mode uses the same path and converts `http/https` to `ws/wss`: `{ws_or_wss_base_url}/responses`.
- `wire_api = "chat"` is rejected in current `openai/codex`; `/v1/chat/completions` is not the active wire path in current releases.

### 2) HTTP method

- `POST` for model generation/streaming requests.

### 3) Headers Codex CLI sends

Common headers on HTTP Responses requests:
- `Authorization: Bearer <token>`
- `Content-Type: application/json` (JSON body; explicit when compression is used)
- `Accept: text/event-stream` (streaming HTTP requests)
- `originator: codex_cli_rs`
- `User-Agent: <codex user agent>`
- `version: <codex_version>` (OpenAI built-in provider default)

Conditional headers (depending on auth/config/request type):
- `OpenAI-Organization`
- `OpenAI-Project`
- `ChatGPT-Account-ID`
- `session_id`
- `x-openai-subagent`
- `x-codex-beta-features`
- `x-codex-turn-state`
- `x-codex-turn-metadata`
- `Content-Encoding: zstd` (when compression enabled)
- `OpenAI-Beta: responses_websockets=2026-02-04` (websocket transport)
- `x-responsesapi-include-timing-metrics: true` (when enabled)

Source evidence:
- Responses endpoint path + `POST` + SSE `Accept`:
  - https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/endpoint/responses.rs
- Request URL construction (`base_url` + path) and websocket URL conversion:
  - https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/provider.rs
- OpenAI base URL defaults + `wire_api = "chat"` rejection:
  - https://github.com/openai/codex/blob/main/codex-rs/core/src/model_provider_info.rs
- Auth headers:
  - https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/auth.rs
- Conversation/session/subagent headers:
  - https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/requests/headers.rs
  - https://github.com/openai/codex/blob/main/codex-rs/core/src/client.rs
- Default client headers (`originator`, `User-Agent`):
  - https://github.com/openai/codex/blob/main/codex-rs/core/src/default_client.rs
- HTTP content-type/compression behavior:
  - https://github.com/openai/codex/blob/main/codex-rs/codex-client/src/transport.rs

---

## Request Schema

### 1. Required Fields (`model`, `messages`)

There are two request shapes relevant to Codex + ccr-rust integration:

1. **Native Codex CLI wire format (Responses API)**
   - Required: `model`, `instructions`, `input`
2. **ccr-rust compatibility format (`/v1/chat/completions`)**
   - Required: `model`, `messages`

Chat-compat minimum body (`model` + `messages`):

```json
{
  "model": "gpt-4o",
  "messages": [
    {
      "role": "user",
      "content": "Hello"
    }
  ]
}
```

Native Codex Responses minimum body:

```json
{
  "model": "gpt-5-codex",
  "instructions": "You are a coding assistant.",
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": [{ "type": "input_text", "text": "Hello" }]
    }
  ]
}
```

| Shape | Required fields |
|-------|-----------------|
| Native Codex (`/v1/responses`) | `model`, `instructions`, `input` |
| Chat-compat (`/v1/chat/completions`) | `model`, `messages` |

### 2. Message Format (`role`, `content`)

Chat-compat message object:

```json
{
  "role": "user",
  "content": "Explain this function"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `role` | string | Yes | Author role (`developer`, `system`, `user`, `assistant`, `tool`) |
| `content` | string \| array | Usually | Message content |
| `tool_call_id` | string | Required for `tool` role | Links tool result to assistant tool call |

Native Codex Responses message item uses `input[]` with typed content parts:

```json
{
  "type": "message",
  "role": "user",
  "content": [
    { "type": "input_text", "text": "Explain this function" }
  ]
}
```

### 3. Tool Definition Format

Codex forwards tools as JSON in a `tools` array and sets `tool_choice` to `"auto"`.

```json
{
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "get_weather",
        "description": "Get weather by city name",
        "parameters": {
          "type": "object",
          "properties": {
            "city": { "type": "string" }
          },
          "required": ["city"]
        },
        "strict": true
      }
    }
  ],
  "tool_choice": "auto",
  "parallel_tool_calls": true
}
```

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `tools` | array | No | Tool definitions passed to model |
| `tools[].type` | string | Yes | Typically `function` |
| `tools[].function.name` | string | Yes | Tool/function identifier |
| `tools[].function.parameters` | object | No | JSON Schema for arguments |
| `tool_choice` | string \| object | No | Codex request builder sets `"auto"` |
| `parallel_tool_calls` | boolean | No | Included by Codex Responses builder |

### 4. Streaming Parameter (`stream`)

Native Codex Responses requests set streaming on every turn:

```json
{
  "stream": true
}
```

| Shape | `stream` behavior |
|-------|--------------------|
| Native Codex (`/v1/responses`) | Always `true` in `ResponsesRequestBuilder` |
| Chat-compat (`/v1/chat/completions`) | Optional boolean (typically `false` unless explicitly enabled) |

ccr-rust also accepts/forwards chat-compat `stream: true` and returns SSE output.

Source evidence:
- Codex Responses request struct/fields: `/Users/kearm/codex/codex-rs/codex-api/src/common.rs`
- Required-field checks + `stream: true` + `tool_choice: "auto"`: `/Users/kearm/codex/codex-rs/codex-api/src/requests/responses.rs`
- Responses endpoint path (`responses`) + SSE accept header: `/Users/kearm/codex/codex-rs/codex-api/src/endpoint/responses.rs`
- ccr-rust chat-compat endpoint and OpenAI request struct (`model`, `messages`, `stream`, `tools`): `contrib/ccr-rust/src/main.rs` and `contrib/ccr-rust/src/router.rs`

---

## Streaming Format

Codex streaming (with `"stream": true`) uses HTTP `text/event-stream` with data-only SSE frames. Each event is a `data: ...` line followed by a blank line.

### 1. SSE Event Format (`data: {...}`)

Each streamed event contains a Chat Completions chunk object:

```text
data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
```

Chunk envelope fields:
- `id`: stable completion ID across all chunks for one stream
- `object`: `chat.completion.chunk`
- `created`: Unix timestamp (seconds)
- `model`: model ID
- `choices`: array of deltas for this emission (typically one element)

### 2. Delta Object Structure

Incremental output is carried in `choices[0].delta`:

- `role`: usually first chunk only (for example, `"assistant"`)
- `content`: token/text fragments across chunks
- `tool_calls`: optional incremental tool-call payloads (provider/model dependent)
- `function_call`: deprecated legacy function-calling delta
- `refusal`: optional refusal text deltas
- `reasoning_content`: optional reasoning deltas on models that expose it

In `ccr-rust`, the OpenAI chunk parser currently materializes `role`, `content`, and `reasoning_content` from `delta` (`contrib/ccr-rust/src/router.rs`) and ignores unknown delta keys safely.

If `stream_options.include_usage` is enabled, an extra usage chunk may appear before stream termination, typically with `choices: []` plus aggregate `usage`.

Example usage chunk shape:

```text
data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[],"usage":{"prompt_tokens":36,"completion_tokens":18,"total_tokens":54}}
```

### 3. Done Event Indicator (`[DONE]`)

The stream terminates with a sentinel event:

```text
data: [DONE]
```

`[DONE]` is a literal sentinel string (not a JSON object). In `ccr-rust` parsing (`contrib/ccr-rust/src/router.rs`), both `data:` and `data: ` prefixes are accepted, and `[DONE]` plus empty `data:` payloads are explicitly treated as non-chunk terminal markers.

### 4. Parser Rules Used in ccr-rust

`contrib/ccr-rust/src/router.rs` currently applies these stream parse rules for Codex/OpenAI-compatible chunks:

- Accept lines with `data:` or `data: ` prefix
- Trim the payload after `data:`
- Skip payloads that are empty or exactly `[DONE]`
- Parse remaining payload as JSON chunk (`chat.completion.chunk`) and read `choices[0].delta`

---

## Response Schema

### 1. Top-Level Structure

The standard (non-streaming) response has the following top-level fields:

```json
{
  "id": "chatcmpl-123",
  "object": "chat.completion",
  "created": 1677652288,
  "model": "gpt-4",
  "system_fingerprint": "fp_44709d6fcb",
  "choices": [...],
  "usage": {
    "prompt_tokens": 9,
    "completion_tokens": 12,
    "total_tokens": 21
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique identifier for the chat completion |
| `object` | string | Always `"chat.completion"` for non-streaming responses |
| `created` | integer | Unix timestamp (seconds) when the completion was created |
| `model` | string | The model used for the completion |
| `system_fingerprint` | string | Optional. Backend configuration fingerprint |
| `choices` | array | Array of completion choices (typically 1 element) |
| `usage` | object | Token usage statistics |

### 2. Choice Format

Each element in the `choices` array:

```json
{
  "index": 0,
  "message": {
    "role": "assistant",
    "content": "Hello there, how may I assist you today?",
    "tool_calls": [...]
  },
  "logprobs": null,
  "finish_reason": "stop"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `index` | integer | Position in the choices array (0-based) |
| `message` | object | The generated message object |
| `logprobs` | object \| null | Log probabilities for tokens (if requested) |
| `finish_reason` | string | Why the model stopped generating: `"stop"`, `"length"`, `"tool_calls"`, `"content_filter"` |

**Finish Reason Values:**
- `"stop"` - Model reached a natural stopping point or encountered a stop sequence
- `"length"` - Maximum token limit reached
- `"tool_calls"` - Model decided to call one or more tools
- `"content_filter"` - Content was filtered due to safety settings
- `"error"` - An error occurred during generation

### 3. Message Format

The `message` object within a choice:

```json
{
  "role": "assistant",
  "content": "I'll help you with that.",
  "tool_calls": [
    {
      "id": "call_abc123",
      "type": "function",
      "function": {
        "name": "get_weather",
        "arguments": "{\"location\": \"Boston, MA\"}"
      }
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `role` | string | Always `"assistant"` for responses |
| `content` | string \| null | The generated text content (null if tool_calls present) |
| `tool_calls` | array | Optional. Array of tool calls requested by the model |

**Tool Call Format:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique identifier for this tool call |
| `type` | string | Always `"function"` |
| `function` | object | Function call details |
| `function.name` | string | Name of the function to call |
| `function.arguments` | string | JSON string of function arguments |

### 4. Delta Format for Streaming

When `stream: true` is set in the request, the API returns Server-Sent Events (SSE) with delta updates:

**Event Format:**
```
data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
```

**Streaming Response Structure:**

```json
{
  "id": "chatcmpl-123",
  "object": "chat.completion.chunk",
  "created": 1677652288,
  "model": "gpt-4",
  "choices": [
    {
      "index": 0,
      "delta": {
        "role": "assistant",
        "content": " partial ",
        "tool_calls": [...]
      },
      "finish_reason": null
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Same ID across all chunks for a single completion |
| `object` | string | Always `"chat.completion.chunk"` for streaming |
| `created` | integer | Unix timestamp |
| `model` | string | Model identifier |
| `choices` | array | Array with single delta element |

**Delta Object:**

| Field | Type | Description |
|-------|------|-------------|
| `role` | string | Present only in first chunk: `"assistant"` |
| `content` | string | Incremental text content |
| `tool_calls` | array | Incremental tool call updates |

**Tool Call Delta Format:**

Tool calls in streaming mode may be split across multiple chunks:

```json
{
  "delta": {
    "tool_calls": [
      {
        "index": 0,
        "id": "call_abc123",
        "type": "function",
        "function": {
          "name": "get_weather",
          "arguments": "{\"lo"
        }
      }
    ]
  }
}
```

Subsequent chunks continue building the arguments:
```json
{
  "delta": {
    "tool_calls": [
      {
        "index": 0,
        "function": {
          "arguments": "cation\": \"Bost"
        }
      }
    ]
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `index` | integer | Position of this tool call in the tool_calls array |
| `id` | string | Tool call ID (present only in first delta for this tool call) |
| `type` | string | `"function"` (present only in first delta) |
| `function.name` | string | Function name (present only in first delta) |
| `function.arguments` | string | Incremental JSON string (may be partial) |

### 5. Usage Statistics

```json
{
  "usage": {
    "prompt_tokens": 50,
    "completion_tokens": 30,
    "total_tokens": 80,
    "prompt_tokens_details": {
      "cached_tokens": 20
    },
    "completion_tokens_details": {
      "reasoning_tokens": 10
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `prompt_tokens` | integer | Tokens in the input prompt |
| `completion_tokens` | integer | Tokens in the generated completion |
| `total_tokens` | integer | Total tokens used |
| `prompt_tokens_details.cached_tokens` | integer | Tokens served from cache |
| `completion_tokens_details.reasoning_tokens` | integer | Tokens used for reasoning (if applicable) |

---

## Codex CLI JSONL Output

When using `codex exec --json`, the CLI outputs a JSON Lines (JSONL) stream with event types:

```json
{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}
{"type":"turn.started"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Repo contains docs, sdk, and examples directories."}}
{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122}}
```

**Event Types:**
- `thread.started` - Beginning of a conversation thread
- `turn.started` - Beginning of a model turn
- `turn.completed` - End of a model turn with usage stats
- `turn.failed` - Turn failed with error
- `item.started` - An action item started
- `item.completed` - An action item completed
- `item.*` - Various item types (agent_message, command_execution, file_change, etc.)
- `error` - Error event

---

## References

- [OpenAI Chat Completions API](https://platform.openai.com/docs/guides/chat-completions)
- [OpenAI Chat Completions API Reference (Create)](https://platform.openai.com/docs/api-reference/chat/create)
- [OpenAI Streaming Responses](https://platform.openai.com/docs/guides/streaming-responses)
- [Codex CLI Non-interactive Mode](https://developers.openai.com/codex/noninteractive/)
- [Codex Models](https://developers.openai.com/codex/models/)
- [OpenAI Function Calling](https://platform.openai.com/docs/guides/function-calling)
