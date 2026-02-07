# CCR-Rust

> **Universal AI coding proxy.** Use Claude Code, Codex CLI, or any Anthropic-compatible tool with DeepSeek, GLM, MiniMax, and more—without rate limits breaking your flow.

You want to use your favorite AI coding assistant (Claude Code or Codex), but rate limits, downtime, or regional restrictions get in the way. Context-switching between tools kills productivity.

**CCR-Rust** is a local proxy that sits between your frontend and multiple LLM providers. When one backend fails, requests automatically cascade to the next tier. Same interface, uninterrupted workflow.

## Dual Frontend Support

CCR-Rust works with **both** leading AI coding assistants:

| Frontend | Setup | Use Case |
|----------|-------|----------|
| **Claude Code** | `export ANTHROPIC_BASE_URL=http://127.0.0.1:3456` | Anthropic's agentic coding assistant |
| **Codex CLI** | `export OPENAI_BASE_URL=http://127.0.0.1:3456` | OpenAI's command-line coding tool |

No configuration changes needed—just point your frontend at CCR-Rust and it handles the rest.

## How It Works

```
┌─────────────────┐     ┌──────────────────────┐     ┌──────────────-──-─┐
│  Claude Code    │────→│                      │────→│  Tier 0: GLM-4.7  │
│  or Codex CLI   │     │  CCR-Rust            │     │  Tier 1: DeepSeek │
│                 │     │  (localhost:3456)    │────→│  Tier 2: MiniMax  │
└─────────────────┘     └──────────────────────┘     └───────────────--──┘
```

If Tier 0 fails or hits a rate limit, the request automatically retries on Tier 1, then Tier 2, and so on.

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (1.75+)
- API keys from at least one provider (see examples below)

### 1. Clone and Build

```bash
git clone https://github.com/RESMP-DEV/ccr-rust.git
cd ccr-rust
cargo build --release
```

### 2. Create Your Config

Create `~/.claude-code-router/config.json`:

```json
{
    "Providers": [
        {
            "name": "zai",
            "api_base_url": "https://api.z.ai/api/coding/paas/v4",
            "api_key": "YOUR_ZAI_API_KEY",
            "models": ["glm-4.7"],
            "transformer": { "use": ["anthropic"] }
        },
        {
            "name": "deepseek",
            "api_base_url": "https://api.deepseek.com",
            "api_key": "YOUR_DEEPSEEK_API_KEY",
            "models": ["deepseek-chat", "deepseek-reasoner"],
            "transformer": { "use": ["anthropic", "deepseek"] }
        },
        {
            "name": "minimax",
            "api_base_url": "https://api.minimax.io/anthropic/v1",
            "api_key": "YOUR_MINIMAX_API_KEY",
            "models": ["MiniMax-M2.1"],
            "protocol": "anthropic",
            "transformer": { "use": ["anthropic"] }
        }
    ],
    "Router": {
        "default": "zai,glm-4.7",
        "think": "deepseek,deepseek-reasoner",
        "longContext": "minimax,MiniMax-M2.1",
        "longContextThreshold": 60000
    }
}
```

Optional persistence (recommended for long-running dashboards/metrics):

```json
"Persistence": {
    "mode": "redis",
    "redis_url": "redis://127.0.0.1:6379/0",
    "redis_prefix": "ccr-rust:persistence:v1"
}
```

**What each field means:**

| Field | Description |
|-------|-------------|
| `Providers` | List of LLM backends you want to use |
| `api_base_url` | The provider's API endpoint |
| `protocol` | Upstream API dialect: `openai` (default) or `anthropic` |
| `transformer.use` | Optional request/response transformer chain |
| `Router.default` | Primary tier—requests go here first |
| `Router.think` | Used for reasoning-heavy tasks |
| `Router.longContext` | Used when token count exceeds `longContextThreshold` |
| `Persistence.mode` | `none` (default) or `redis` for restart-safe observability state |
| `Persistence.redis_url` | Redis connection URL (or `CCR_REDIS_URL`) |
| `Persistence.redis_prefix` | Redis key prefix used by CCR persistence |

### 3. Start the Server

```bash
# Install globally
cargo install --path .

# Run it
ccr-rust start --config ~/.claude-code-router/config.json

# Or run directly from target/
./target/release/ccr-rust start --config ~/.claude-code-router/config.json
```

### 4. Point Your Frontend at CCR-Rust

**For Claude Code:**
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude
```

**For Codex CLI:**
```bash
export OPENAI_BASE_URL=http://127.0.0.1:3456
# Any non-empty string works (for example: dummy-key, test, abc123)
# CCR-Rust does not use this value for upstream provider auth
export OPENAI_API_KEY=any-non-empty-string
codex
```

`OPENAI_API_KEY` here is only for the Codex client process and can be any non-empty string. CCR-Rust authenticates to upstream providers using `Providers[].api_key` from `config.json`.

That's it. Your frontend now routes through CCR-Rust with automatic fallback.

## Streaming and Responses Behavior

CCR-Rust serves three wire-compatible endpoints:

| Endpoint | Input Shape | Streaming Default | Output Shape |
|----------|-------------|-------------------|--------------|
| `/v1/messages` | Anthropic Messages API | `stream: false` when omitted | Anthropic JSON or Anthropic SSE |
| `/v1/chat/completions` | OpenAI Chat Completions API | `stream: false` when omitted | OpenAI JSON or OpenAI SSE |
| `/v1/responses` | OpenAI Responses API | `stream: true` when omitted | Responses JSON or Responses SSE |

### `/v1/responses` behavior

- Incoming Responses requests are normalized into an internal chat-completions request for backend routing.
- `instructions` is treated as a system message before dispatch.
- `input` message/tool items are converted into OpenAI-compatible chat/tool message forms.
- Non-streaming requests return a Responses object (`"object": "response"`, `"status": "completed"`).
- Streaming requests emit Responses-style SSE events in this order:
  `response.created` → `response.output_item.added` → `response.output_text.delta` (and optional `response.reasoning_text.delta`) → `response.output_item.done` → `response.completed`.

### Streaming failure semantics

For `/v1/responses` with streaming enabled, CCR-Rust keeps the transport in SSE mode even when the upstream request fails. Instead of returning a non-2xx JSON body, it returns `200 OK` with `text/event-stream` and emits `response.failed`:

```text
event: response.failed
data: {"type":"response.failed","response":{"id":"resp_failed","object":"response","status":"failed","error":{"message":"{\"error\":{\"message\":\"upstream failed\"}}"}}}
```

## Frontend-Specific Setup

### Claude Code Setup

Claude Code works seamlessly with CCR-Rust, allowing you to use it with providers like DeepSeek, GLM, and more.

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude
```

For detailed setup instructions, see [docs/claude_code_setup.md](docs/claude_code_setup.md).

### Codex CLI Setup

You can use the Codex CLI with CCR-Rust to route requests to any supported backend.

```bash
export OPENAI_BASE_URL=http://127.0.0.1:3456
export OPENAI_API_KEY=any-non-empty-string
codex
```

For detailed setup instructions, see [docs/codex_setup.md](docs/codex_setup.md).

## Monitoring

### Check Status

```bash
ccr-rust status
```

Shows whether the server is running and current latencies per tier.

### Live Dashboard

```bash
ccr-rust dashboard
```

Opens an interactive TUI showing:
- Active streams and success rates
- Per-tier latency (EWMA)
- Token throughput
- Failure counts

### Validate Config

```bash
ccr-rust validate --config ~/.claude-code-router/config.json
```

Checks your config file for syntax errors and missing fields.

### Provider Smoke Matrix

Run a live matrix against all configured `provider,model` routes (discovered from `/v1/models`):

```bash
scripts/ccr_provider_smoke.py --base-url http://127.0.0.1:3456 --api-key test
```

This validates:
- `/v1/chat/completions` (`stream: false` and `stream: true`)
- `/v1/responses` (`stream: false` and `stream: true`)
- SSE frame integrity (JSON parse + terminal markers/events)

## Configuration Reference

### Minimal Config (Single Provider)

```json
{
    "Providers": [
        {
            "name": "deepseek",
            "api_base_url": "https://api.deepseek.com",
            "api_key": "sk-xxx",
            "models": ["deepseek-chat"],
            "transformer": { "use": ["anthropic"] }
        }
    ],
    "Router": {
        "default": "deepseek,deepseek-chat"
    }
}
```

### Multi-Tier Fallback Config

```json
{
    "Providers": [
        {
            "name": "zai",
            "api_base_url": "https://api.z.ai/api/coding/paas/v4",
            "api_key": "sk-xxx",
            "models": ["glm-4.7"],
            "transformer": { "use": ["anthropic"] }
        },
        {
            "name": "deepseek",
            "api_base_url": "https://api.deepseek.com",
            "api_key": "sk-xxx",
            "models": ["deepseek-chat", "deepseek-reasoner"],
            "transformer": { "use": ["anthropic", "deepseek"] }
        },
        {
            "name": "minimax",
            "api_base_url": "https://api.minimax.io/anthropic/v1",
            "api_key": "sk-xxx",
            "models": ["MiniMax-M2.1"],
            "protocol": "anthropic",
            "transformer": { "use": ["anthropic"] }
        }
    ],
    "Router": {
        "default": "zai,glm-4.7",
        "think": "deepseek,deepseek-reasoner",
        "longContext": "minimax,MiniMax-M2.1",
        "longContextThreshold": 60000
    }
}
```

### Retry Configuration (Optional)

Fine-tune how aggressively each tier retries:

```json
{
    "Router": {
        "default": "zai,glm-4.7",
        "tierRetries": {
            "tier-0": { "max_retries": 5, "base_backoff_ms": 50 },
            "tier-1": { "max_retries": 3, "base_backoff_ms": 100 }
        }
    }
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `max_retries` | 3 | Retry attempts (total = 1 initial + max_retries) |
| `base_backoff_ms` | 100 | Initial delay before retry |
| `backoff_multiplier` | 2.0 | Exponential backoff factor |

## Supported Providers

| Provider | API Base URL | Notes |
|----------|--------------|-------|
| Z.AI (GLM) | `https://api.z.ai/api/coding/paas/v4` | GLM-4.7, requires `anthropic` transformer |
| DeepSeek | `https://api.deepseek.com` | deepseek-chat, deepseek-reasoner |
| MiniMax | `https://api.minimax.io/anthropic/v1` | Set `protocol: "anthropic"` for best tool-call compatibility |
| OpenRouter | `https://openrouter.ai/api/v1` | Access to many models |
| Groq | `https://api.groq.com/openai/v1` | Fast inference, Llama models |

## Development

### Project Structure

```
src/
├── main.rs          # CLI entry point
├── config.rs        # Config parsing
├── router.rs        # Request routing & fallback logic
├── transformer.rs   # Protocol translation (OpenAI ↔ Anthropic)
├── dashboard.rs     # TUI dashboard
└── metrics.rs       # Prometheus metrics
```

### Running Tests

```bash
cargo test
```

### Building Release

```bash
cargo build --release
```

The binary will be at `./target/release/ccr-rust`.

## Advanced Topics

For advanced configuration options, see the [docs/](docs/) directory:

- [Presets](docs/presets.md) - Named routing presets
- [Observability](docs/observability.md) - Prometheus metrics, token drift monitoring
- [Deployment](docs/deployment.md) - Docker, Kubernetes, systemd

## Contributing

PRs welcome! If you've got a provider that doesn't quite work, or you're hitting weird edge cases, open an issue. This project started because we got tired of rate limits interrupting our flow—if that resonates, we'd love your help making it better.

## License

MIT
