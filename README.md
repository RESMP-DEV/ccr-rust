# CCR-Rust

High-throughput multi-protocol LLM router for **Claude Code**, **Codex**, **OpenCode**, and any **OpenAI-compatible** client.

Your Claude Code instance hits daily usage limits and you're blocked. CCR-Rust automatically routes to backup providers—Opus, Kimi, Gemini—keeping you unblocked. When limits reset, it routes back to Claude. One endpoint, automatic failover, zero client changes.

## Features

- **Automatic failover** — tiered provider cascade on 5xx/timeouts; 429s pass through to client
- **Multi-protocol** — Anthropic and OpenAI APIs behind one endpoint
- **Cost routing** — send traffic classes (default/think/background) to different models
- **Observability** — Prometheus metrics, live TUI dashboard, token/latency tracking
- **MCP aggregation** — optional tool server proxying
- **Compression** — response and tool-output compression for agent workloads

~15 MB binary, <50 ms P99 routing overhead, built for high-concurrency agent swarms.

## Getting Started

### 1. Build and install

```bash
cargo build --release
cargo install --path . --force
```

### 2. Create a config

```bash
mkdir -p ~/.claude-code-router
cp config.example.json ~/.claude-code-router/config.json
```

Edit the config to add your provider API keys. See [Configuration guide](docs/configuration.md) for the full schema.

### 3. Start the router

```bash
ccr-rust start
ccr-rust status       # verify it's running
```

### 4. Point your client at CCR

```bash
# Claude Code
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude

# Codex
export OPENAI_BASE_URL=http://127.0.0.1:3456/v1
codex

# OpenCode
export OPENAI_BASE_URL=http://127.0.0.1:3456/v1
opencode
```

Any OpenAI-compatible client works the same way — just set the base URL.

## API Surface

| Endpoint               | Method | Purpose                     |
| ---------------------- | ------ | --------------------------- |
| `/v1/messages`         | POST   | Anthropic messages API      |
| `/v1/chat/completions` | POST   | OpenAI chat completions API |
| `/v1/responses`        | POST   | Stream batch responses      |
| `/v1/models`           | GET    | List configured models      |
| `/health`              | GET    | Health check                |
| `/metrics`             | GET    | Prometheus metrics          |

## Configuration

CCR-Rust reads `~/.claude-code-router/config.json`. Supports `${ENV_VAR}` substitution.

```json
{
  "router": {
    "default_provider": "claude",
    "backends": {
      "claude": { "type": "anthropic", "api_key": "${ANTHROPIC_API_KEY}" },
      "opus": {
        "type": "openai",
        "api_key": "${OPUS_API_KEY}",
        "base_url": "..."
      }
    }
  }
}
```

For full schema and provider setup, see [Configuration guide](docs/configuration.md).
For common presets (Claude-only, multi-tier, cost-optimized), see [Presets](docs/presets.md).

### Rate Limiting

Rate limiting is handled with transparency for orchestrators and clients:

- **429 responses are passed through** with a normalized error body (`type: "rate_limit_error"`, `code: "rate_limited"`) and an `x-ccr-tier` header identifying which provider was rate-limited. The rate limit is still tracked internally for future tier-skipping decisions.
- **5xx/timeout errors** cascade to the next tier automatically. The client only sees an error if all tiers are exhausted.
- **Informational headers** (`X-RateLimit-Remaining: 0` on 200 responses) trigger proactive tier-skipping by default. Set `"honor_ratelimit_headers": false` per provider for those (like Z.AI) that send these as informational warnings without actual enforcement.

This design works well with external orchestrators and retry-aware clients that need accurate rate-limit signal for intelligent routing. Standalone users should implement client-side retry logic for 429s when they want automatic recovery from rate limits.

## Observability

```bash
# Prometheus metrics
curl http://localhost:3456/metrics

# Live TUI dashboard
ccr-rust dashboard

# Remote dashboard target via environment
CCR_DASHBOARD_HOST=10.0.0.5 CCR_DASHBOARD_PORT=3456 ccr-rust dashboard
```

Tracks: token counts (in/out), latencies (p50/p90/p99), provider success rates, circuit-breaker states, cost per tier.

## Documentation

See [docs/index.md](docs/index.md) for the full documentation index:

- **Setup:** [CLI reference](docs/cli.md) · [Configuration](docs/configuration.md) · [Presets](docs/presets.md) · [Deployment](docs/deployment.md)
- **Integrations:** [Claude Code](docs/claude_code_setup.md) · [Codex](docs/codex_setup.md) · [OpenAI SDK](docs/openai_sdk_setup.md) · [Kimi](docs/kimi_setup.md) · [Gemini](docs/gemini-integration.md)
- **Operations:** [Observability](docs/observability.md) · [Debug capture](docs/debug_capture.md) · [Streaming](docs/streaming_incremental_design.md) · [Token optimization](docs/token_optimization.md)
- **Troubleshooting:** [Common issues](docs/troubleshooting.md)

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

**Network service clause:** Modified versions of CCR-Rust offered as a network service must provide source code to users of that service.

Built for reliability. Made for scale. Join the [discussions](https://github.com/RESMP-DEV/ccr-rust/discussions).
