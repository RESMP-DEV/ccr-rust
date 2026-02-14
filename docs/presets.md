# Presets

Named routing configurations with parameter overrides.

## Configuration

Define presets in your `config.json`:

```json
{
    "Presets": {
        "fast": {
            "route": "groq,llama-3",
            "max_tokens": 2048
        },
        "smart": {
            "route": "anthropic,claude-3-opus",
            "temperature": 0.2
        },
        "code": {
            "route": "deepseek,deepseek-coder",
            "max_tokens": 4096,
            "temperature": 0.0
        }
    }
}
```

Each preset can override:
- `route` - Provider and model (`provider,model`)
- `max_tokens` - Maximum output tokens
- `temperature` - Sampling temperature
- Any other model parameter

## Usage

Route a request through a preset:

```bash
curl http://localhost:3456/preset/fast/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "user", "content": "Hello"}]}'
```

The preset's parameters are merged with the request body. Request parameters take precedence.

## Listing Presets

```bash
curl http://localhost:3456/v1/presets
```

Returns:

```json
{
    "presets": ["fast", "smart", "code"]
}
```

## Web Search Integration

Automatically route requests containing `[search]` or `[web]` tags to a search-capable provider:

```json
{
    "Router": {
        "web_search": {
            "enabled": true,
            "search_provider": "perplexity,sonar-pro"
        }
    }
}
```

When enabled:
1. CCR-Rust scans incoming messages for `[search]` or `[web]` tags
2. Tagged requests route to the configured `search_provider`
3. Tags are stripped before forwarding to the provider

Example request:

```bash
curl http://localhost:3456/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "user", "content": "[search] What is the latest Rust version?"}]}'
```

The `[search]` tag is removed, and the request routes to `perplexity,sonar-pro`.

## Built-in Presets

CCR-Rust includes recommended presets for common workloads:

| Preset | Route | Use Case |
|--------|-------|----------|
| `coding` | Fast code model (e.g., GLM-5) | P0/P1 code generation tasks |
| `reasoning` | Reasoning model (e.g., DeepSeek Reasoner) | Complex analysis, refactors |
| `documentation` | Context compression (e.g., Gemini Flash) | Summarization, handoffs, coalescence |

### Documentation Preset

The `documentation` preset is designed for context compression:

```json
{
    "Presets": {
        "documentation": {
            "route": "gemini,gemini-3-flash-preview",
            "temperature": 0.3
        }
    }
}
```

Usage:
```bash
curl http://localhost:3456/preset/documentation/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "user", "content": "Summarize this session..."}]}'
```

See [Gemini Integration](gemini-integration.md) for cost savings and configuration details.
