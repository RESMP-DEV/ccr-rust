# CCR-Rust

> **Universal AI coding proxy.** Route Codex CLI or Claude Code through multiple LLM providers with automatic failover.

- **Automatic failover** — Tier 0 rate-limited? Falls back to Tier 1, then Tier 2
- **Same interface** — Just set an environment variable, no workflow changes
- **Task-based routing** — Fast models for code gen, reasoning models for complex refactors
- **Cost control** — Cheaper providers by default, expensive ones as fallback

## Supported Providers

| Provider | Models | Best For |
|----------|--------|----------|
| **Z.AI (GLM)** | GLM-5 | Fast code generation, daily driver |
| **DeepSeek** | deepseek-chat, deepseek-reasoner | Deep reasoning, complex refactors |
| **MiniMax** | MiniMax-M2.5 | High-performance reasoning |
| **Kimi (Moonshot)** | Kimi K2.5 | Extended context (1M+ tokens) |
| **Google Gemini** | gemini-3-flash-preview | Context compression, summarization |
| **OpenRouter** | 200+ models | Fallback to anything |

### Coding Plan Discounts

Several providers offer subscription plans with better rates than pay-as-you-go:

| Provider | Plan | Savings |
|----------|------|---------|
| **Z.AI** | [Coding Plan](https://z.ai/subscribe?ic=Y8HASOW1RU) | **10% off** — Best value for daily use |
| **MiniMax** | [Coding Plan](https://platform.minimax.io/subscribe/coding-plan?code=AnKU0nzXQG&source=link) | **10% off** |
| DeepSeek | Pay-as-you-go | Usage-based pricing |
| OpenRouter | Pay-as-you-go | Usage-based pricing |

## Works With Both Leading Assistants

| Frontend | Setup | Status |
|----------|-------|--------|
| **Codex CLI** | `export OPENAI_BASE_URL=http://127.0.0.1:3456/v1` | ✅ Full support |
| **Claude Code** | `export ANTHROPIC_BASE_URL=http://127.0.0.1:3456` | ✅ Full support |

## How It Works

1. Your assistant (Codex/Claude) sends a request to `localhost:3456`
2. CCR-Rust tries Tier 0 (e.g., GLM-5)
3. If that fails (rate limit, timeout, error), it retries on Tier 1 (e.g., DeepSeek)
4. Still failing? Tier 2 (e.g., MiniMax), and so on
5. Response goes back to your assistant—same format it expected

All transparent. No workflow changes.

---

## Quick Start

### 1. Build

```bash
git clone https://github.com/RESMP-DEV/ccr-rust.git
cd ccr-rust
cargo build --release
cargo install --path .
```

### 2. Configure

Create `~/.claude-code-router/config.json`:

```json
{
    "Providers": [
        {
            "name": "zai",
            "api_base_url": "https://api.z.ai/api/coding/paas/v4",
            "api_key": "YOUR_ZAI_API_KEY",
            "models": ["glm-5"],
            "transformer": { "use": ["anthropic"] }
        },
        {
            "name": "deepseek",
            "api_base_url": "https://api.deepseek.com",
            "api_key": "YOUR_DEEPSEEK_API_KEY",
            "models": ["deepseek-chat", "deepseek-reasoner"],
            "transformer": { "use": ["anthropic", "deepseek"] }
        }
    ],
    "Router": {
        "default": "zai,glm-5",
        "think": "deepseek,deepseek-reasoner"
    }
}
```

### 3. Run

```bash
ccr-rust start
```

### 4. Connect Your Assistant

**Codex CLI:**
```bash
export OPENAI_BASE_URL=http://127.0.0.1:3456/v1
export OPENAI_API_KEY=dummy  # Any non-empty string works
codex
```

**Claude Code:**
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude
```

That's it. Your frontend now routes through CCR-Rust with automatic fallback.

---

## Monitoring

```bash
ccr-rust status      # Health check + latencies
ccr-rust dashboard   # Live TUI with streams, throughput, failures
ccr-rust validate    # Check config for errors
```

---

## Configuration Reference

### Config Fields

| Field | Description |
|-------|-------------|
| `Providers` | List of LLM backends |
| `api_base_url` | Provider's API endpoint |
| `protocol` | `openai` (default) or `anthropic` |
| `transformer.use` | Request/response transformer chain |
| `Router.default` | Primary tier (requests go here first) |
| `Router.think` | Used for reasoning-heavy tasks |

### Transformer Notes

The `transformer` field is optional. Common uses:
- `{"use": ["anthropic"]}` — Translate OpenAI requests to Anthropic-style
- `{"use": ["deepseek"]}` — Normalize DeepSeek's `reasoning_content`
- `{"use": ["minimax"]}` — Extract MiniMax structured reasoning
- `{"use": ["openrouter"]}` — Add OpenRouter attribution headers

### Multi-Tier Fallback Example

```json
{
    "Providers": [
        {
            "name": "zai",
            "api_base_url": "https://api.z.ai/api/coding/paas/v4",
            "api_key": "sk-xxx",
            "models": ["glm-5"],
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
            "api_base_url": "https://api.minimax.io/v1",
            "api_key": "sk-xxx",
            "models": ["MiniMax-M2.5"]
        }
    ],
    "Router": {
        "default": "zai,glm-5",
        "think": "deepseek,deepseek-reasoner"
    }
}
```

### Retry Tuning

```json
{
    "Router": {
        "tierRetries": {
            "tier-0": { "max_retries": 5, "base_backoff_ms": 50 },
            "tier-1": { "max_retries": 3, "base_backoff_ms": 100 }
        }
    }
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `max_retries` | 3 | Retry attempts per tier |
| `base_backoff_ms` | 100 | Initial retry delay |
| `backoff_multiplier` | 2.0 | Exponential backoff factor |

### Agent Mode (Non-Streaming)

For automated agent workloads, disable streaming to avoid SSE frame parsing errors:

```json
{
    "Router": {
        "forceNonStreaming": true
    }
}
```

**Recommended for:** CI/CD, batch processing, agent orchestration.  
**Not recommended for:** Interactive coding where you want token-by-token output.

### Enforce Tier Order

Clients like Codex CLI cache the last successful model. If a request falls back to `openrouter,aurora-alpha`, subsequent requests will target that tier directly, bypassing cheaper tiers.

To force all requests to start from tier 0:

```json
{
    "Router": {
        "ignoreDirect": true
    }
}
```

See [Troubleshooting: Requests Bypassing Tier Order](docs/troubleshooting.md#requests-bypassing-tier-order) for details.

### Persistence (Optional)

For long-running dashboards/metrics that survive restarts:

```json
{
    "Persistence": {
        "mode": "redis",
        "redis_url": "redis://127.0.0.1:6379/0",
        "redis_prefix": "ccr-rust:persistence:v1"
    }
}
```

---

## API Endpoints

| Endpoint | Wire Format | Streaming Default |
|----------|-------------|-------------------|
| `/v1/messages` | Anthropic Messages API | `stream: false` |
| `/v1/chat/completions` | OpenAI Chat Completions | `stream: false` |
| `/v1/responses` | OpenAI Responses API | `stream: true` |

For detailed streaming behavior and failure semantics, see [docs/streaming.md](docs/streaming.md).

---

## Development

```
src/
├── main.rs          # CLI entry point
├── config.rs        # Config parsing
├── router.rs        # Request routing & fallback
├── transformer.rs   # Protocol translation
├── dashboard.rs     # TUI dashboard
└── metrics.rs       # Prometheus metrics
```

```bash
cargo test           # Run tests
cargo build --release # Build release binary
```

## Advanced Topics

- [Presets](docs/presets.md) — Named routing presets for different workloads
- [Gemini Integration](docs/gemini-integration.md) — Context compression with Gemini Flash
- [Observability](docs/observability.md) — Prometheus metrics, token drift monitoring
- [Deployment](docs/deployment.md) — Docker, Kubernetes, systemd

## Contributing

PRs welcome! This project started because we got tired of rate limits interrupting our flow. If that resonates, we'd love your help making it better.

## License

MIT
