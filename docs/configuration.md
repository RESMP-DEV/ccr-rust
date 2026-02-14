# CCR-Rust Configuration

## Config File Location

CCR-Rust reads its configuration from a JSON file. The location is determined by:

| Method | Value |
|--------|-------|
| CLI flag | `--config path/to/config.json` |
| Environment | `CCR_CONFIG=path/to/config.json` |
| Default | `~/.claude-code-router/config.json` |

## Environment Variables

CCR-Rust supports environment variable expansion in config files using `${VAR_NAME}` syntax:

```json
{
    "Providers": [
        {
            "name": "gemini",
            "api_key": "${GEMINI_API_KEY}",
            ...
        }
    ]
}
```

### .env File Support

CCR-Rust automatically loads `.env` files from:
1. Current working directory
2. `~/.claude-code-router/.env`

```bash
# .env
GEMINI_API_KEY=your-key-here
DEEPSEEK_API_KEY=sk-xxx
MINIMAX_API_KEY=mk-xxx
```

### Security Best Practice

| Method | Security | Use Case |
|--------|----------|----------|
| `.env` file | Medium | Development (add to `.gitignore`) |
| Environment | High | Production, CI/CD |
| Hardcoded | Low | Never use in shared code |

See [Gemini Integration](gemini-integration.md) for detailed security guidance.

## Full Schema

```json
{
  "Providers": [
    {
      "name": "provider-name",
      "api_base_url": "https://api.example.com/v1/chat/completions",
      "api_key": "sk-...",
      "models": ["model-a", "model-b"],
      "transformer": {
        "use": ["transformer-name"],
        "model-specific": { "use": ["override-transformer"] }
      }
    }
  ],
  "Router": {
    "default": "provider,model",
    "background": "provider,model",
    "think": "provider,model",
    "webSearch": "provider,model",
    "tierRetries": {
      "tier-0": {
        "max_retries": 3,
        "base_backoff_ms": 100,
        "backoff_multiplier": 2.0,
        "max_backoff_ms": 10000
      }
    }
  },
  "PORT": 3456,
  "HOST": "127.0.0.1",
  "API_TIMEOUT_MS": 600000,
  "PROXY_URL": "http://proxy.example.com:8080",
  "POOL_MAX_IDLE_PER_HOST": 64,
  "POOL_IDLE_TIMEOUT_MS": 90000,
  "SSE_BUFFER_SIZE": 32
}
```

## Providers

Each provider entry configures an upstream API endpoint.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | - | Unique identifier for this provider. Used in router routes. |
| `api_base_url` | string | Yes | - | Full URL to the API endpoint. |
| `api_key` | string | Yes | - | API key for authentication. |
| `models` | array | Yes | - | List of model names available from this provider. |
| `transformer` | object | No | - | Request/response transformation configuration. |

### Provider Transformer Configuration

The `transformer` object defines how requests and responses are modified when routing through this provider.

```json
{
  "transformer": {
    "use": ["deepseek", "tooluse"],
    "model-name": { "use": ["alternative-transformer"] }
  }
}
```

| Subfield | Type | Description |
|----------|------|-------------|
| `use` | array | Provider-level transformer chain applied to all models. |
| `<model-name>` | object | Model-specific transformer override (key is model name). |

#### Transformer Entry Formats

Each entry in the `use` array can be:

1. **Bare string**: Simple transformer name without options
   ```json
   "use": ["deepseek", "openrouter"]
   ```

2. **Tuple with options**: Transformer name and configuration object
   ```json
   "use": [["maxtoken", {"max_tokens": 65536}]]
   ```

#### Model Override Pattern

Model-specific overrides replace the provider-level transformers for that model:

```json
{
  "transformer": {
    "use": ["deepseek"],
    "deepseek-chat": { "use": ["tooluse"] }
  }
}
```

In this example:
- All models except `deepseek-chat` use the `deepseek` transformer
- `deepseek-chat` uses the `tooluse` transformer instead

## Router

The `Router` section configures how incoming requests are routed to providers.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `default` | string | Yes | - | Default route format: `"provider,model"`. |
| `background` | string | No | - | Route for background tasks. |
| `think` | string | No | - | Route for reasoning/thinking models. |
| `webSearch` | string | No | - | Route for web search requests. |
| `tierRetries` | object | No | - | Per-tier retry configuration. |
| `forceNonStreaming` | boolean | No | false | Disable streaming for agent workloads. |
| `ignoreDirect` | boolean | No | false | Ignore client model targeting, enforce tier order. |

### Route Format

Routes use the format `"provider,model"`:
- `provider`: Must match a provider's `name` field
- `model`: Must be listed in that provider's `models` array

Example:
```json
{
  "Router": {
    "default": "deepseek,deepseek-chat",
    "think": "deepseek,deepseek-reasoner"
  }
}
```

### Ignore Direct Routing

By default, if a client sends a request with an explicit `model` in `provider,model` format (e.g., `openrouter,openrouter/aurora-alpha`), CCR-Rust will prioritize that tier and move it to the front of the cascade.

This behavior exists because some clients cache the last successful model. However, this can cause problems:

- **Tier bypass**: Cheaper/faster tiers get skipped entirely
- **Load imbalance**: All requests funnel to one provider
- **Unexpected costs**: Expensive fallback tiers become the default

To disable this behavior and strictly enforce your configured tier order:

```json
{
  "Router": {
    "ignoreDirect": true
  }
}
```

With `ignoreDirect: true`, all requests start from tier 0 regardless of what model the client requests.

### Per-Tier Retry Config

The `tierRetries` object defines retry behavior for each backend tier.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_retries` | number | 3 | Maximum retry attempts per request. |
| `base_backoff_ms` | number | 100 | Initial backoff delay in milliseconds. |
| `backoff_multiplier` | number | 2.0 | Exponential backoff multiplier. |
| `max_backoff_ms` | number | 10000 | Maximum backoff delay in milliseconds. |

Example:
```json
{
  "Router": {
    "tierRetries": {
      "tier-0": { "max_retries": 3, "base_backoff_ms": 100 },
      "tier-1": { "max_retries": 2, "base_backoff_ms": 200 }
    }
  }
}
```

#### Backoff Calculation

The backoff delay for attempt `n` (0-indexed) is:

```
delay = min(base_backoff_ms * multiplier^n, max_backoff_ms)
```

With defaults (100ms base, 2.0 multiplier, 10000ms max):
- Attempt 0: 100ms
- Attempt 1: 200ms
- Attempt 2: 400ms
- Attempt 3: 800ms
- ...

## Server Configuration

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `PORT` | number | 3456 | HTTP server port. |
| `HOST` | string | `127.0.0.1` | Bind address. |
| `API_TIMEOUT_MS` | number | 600000 | Request timeout in milliseconds (10 minutes). |
| `PROXY_URL` | string | null | Optional HTTP proxy URL. |

## Connection Pool Configuration

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `POOL_MAX_IDLE_PER_HOST` | number | 64 | Maximum idle connections per host. |
| `POOL_IDLE_TIMEOUT_MS` | number | 90000 | Idle connection timeout in milliseconds (90s). |

## SSE Configuration

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `SSE_BUFFER_SIZE` | number | 32 | SSE channel buffer size (number of chunks). |

## Complete Example

```json
{
  "Providers": [
    {
      "name": "deepseek",
      "api_base_url": "https://api.deepseek.com/chat/completions",
      "api_key": "${DEEPSEEK_API_KEY}",
      "models": ["deepseek-chat", "deepseek-reasoner"],
      "transformer": {
        "use": ["deepseek"],
        "deepseek-chat": { "use": ["tooluse"] }
      }
    },
    {
      "name": "gemini",
      "api_base_url": "https://generativelanguage.googleapis.com/v1beta/openai",
      "api_key": "${GEMINI_API_KEY}",
      "models": ["gemini-3-flash-preview"],
      "transformer": { "use": ["anthropic"] }
    },
    {
      "name": "openrouter",
      "api_base_url": "https://openrouter.ai/api/v1/chat/completions",
      "api_key": "${OPENROUTER_API_KEY}",
      "models": ["anthropic/claude-3.5-sonnet"],
      "transformer": { "use": ["openrouter"] }
    }
  ],
  "Router": {
    "default": "deepseek,deepseek-chat",
    "think": "deepseek,deepseek-reasoner",
    "tierRetries": {
      "tier-0": { "max_retries": 3, "base_backoff_ms": 100 },
      "tier-1": { "max_retries": 2, "base_backoff_ms": 200 }
    }
  },
  "Presets": {
    "coding": { "route": "deepseek,deepseek-chat" },
    "reasoning": { "route": "deepseek,deepseek-reasoner" },
    "documentation": { "route": "gemini,gemini-3-flash-preview" }
  },
  "PORT": 3456,
  "HOST": "127.0.0.1",
  "API_TIMEOUT_MS": 600000
}
```
