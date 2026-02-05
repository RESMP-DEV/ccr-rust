# CCR-Rust

> **Never get blocked by rate limits again.** Keep using Claude Code's interface while seamlessly falling back to DeepSeek, GLM-4, MiniMax, or any OpenAI-compatible API.

Claude Code has the best AI coding interface, but hit a rate limit and your flow is broken. You're stuck waiting, or worse, context-switching to a different tool.

**CCR-Rust** is a local proxy that sits between Claude Code and your LLM providers. When one backend is rate-limited, overloaded, or down, requests automatically cascade to the next tier. Same interface, uninterrupted workflow.

## How It Works

```
Claude Code  →  CCR-Rust (localhost:3456)  →  Tier 0: GLM-4.7
                                           →  Tier 1: DeepSeek
                                           →  Tier 2: MiniMax
                                           →  ...
```

If Tier 0 fails or hits a rate limit, the request automatically retries on Tier 1, then Tier 2, and so on.

## Getting Started

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
            "api_base_url": "https://api.minimax.io/v1",
            "api_key": "YOUR_MINIMAX_API_KEY",
            "models": ["MiniMax-M2.1"],
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

**What each field means:**

| Field | Description |
|-------|-------------|
| `Providers` | List of LLM backends you want to use |
| `api_base_url` | The provider's API endpoint |
| `transformer.use` | Format translation (most providers need `["anthropic"]`) |
| `Router.default` | Primary tier—requests go here first |
| `Router.think` | Used for reasoning-heavy tasks |
| `Router.longContext` | Used when token count exceeds `longContextThreshold` |

### 3. Start the Server

```bash
# Install globally
cargo install --path .

# Run it
ccr-rust start --config ~/.claude-code-router/config.json

# Or run directly from target/
./target/release/ccr-rust start --config ~/.claude-code-router/config.json
```

### 4. Point Claude Code at CCR-Rust

Set the environment variable before launching Claude Code:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
```

That's it. Claude Code now routes through CCR-Rust.

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
            "api_base_url": "https://api.minimax.io/v1",
            "api_key": "sk-xxx",
            "models": ["MiniMax-M2.1"],
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
| MiniMax | `https://api.minimax.io/v1` | MiniMax-M2.1, good for long context |
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
