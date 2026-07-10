# CCR-Rust

CCR-Rust is a local LLM router for **Claude Code**, **Codex**, **OpenCode**, and other **OpenAI-compatible** clients.

The easiest way to think about it:

- your client keeps talking to one local URL,
- CCR-Rust decides which provider/model should handle the request,
- and you keep the same workflow even when you need to switch away from Claude.

For many people, the main use case is simple: **when your Claude plan runs out, keep using Claude Code with a GLM-5.2/MiniMax M3 instead of changing tools.**

## Features

- **Automatic failover** — tiered provider cascade on 5xx/timeouts; 429s pass through to client
- **Multi-protocol** — Anthropic and OpenAI APIs behind one endpoint
- **Cost routing** — send traffic classes (default/think/background) to different models
- **Observability** — Prometheus metrics, live TUI dashboard, token/latency tracking
- **MCP aggregation** — optional tool server proxying
- **Compression** — response and tool-output compression for long-running client sessions

~15 MB binary, <50 ms P99 routing overhead, designed to stay out of your way.

## Getting Started

If you are opening this repository to make changes and you do not normally work in Rust repos, read [`AGENTS.md`](AGENTS.md) first. It explains the expected workflow, validation steps, and local conventions.

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

If your main goal is “keep working after Claude usage runs out”, start with the step-by-step guide:

- [Claude Code fallback how-to](docs/claude_code_setup.md)

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

If Claude Code complains about a missing `ANTHROPIC_API_KEY`, keep that environment variable set locally as well. CCR-Rust still uses the upstream provider keys from its own config file.

## What it is doing

If you have ever looked at CCR-Rust and thought, “what in the world is this thing doing?”, here is the short version:

1. **Your client sends one request to CCR-Rust**.
  - Claude Code talks to the Anthropic-style endpoint.
  - Codex and most SDKs talk to the OpenAI-style endpoint.
2. **CCR-Rust picks a configured provider/model** using your routing rules.
3. **If the upstream provider uses a different API format, CCR-Rust translates the request.**
4. **It sends the request upstream, collects the response, and translates it back if needed.**
5. **Your client still sees the format it expects.**

That means you can keep using Claude Code as your interface even when the actual model behind it changes.

## API Surface

| Endpoint               | Method | Purpose                     |
| ---------------------- | ------ | --------------------------- |
| `/v1/messages`         | POST   | Anthropic messages API      |
| `/v1/chat/completions` | POST   | OpenAI chat completions API |
| `/v1/responses`        | POST   | Stream batch responses      |
| `/v1/models`           | GET    | List configured models      |
| `/health`              | GET    | Health check                |
| `/metrics`             | GET    | Prometheus metrics          |

### Native MCP daemon

The separate native MCP daemon requires bearer authentication on both `/health`
and `/mcp`. Prefer the environment variable so the token is not exposed in the
process argument list:

```bash
export CCR_MCP_AUTH_TOKEN="replace-with-a-private-random-token"
ccr-rust mcp-daemon

curl -H "Authorization: Bearer ${CCR_MCP_AUTH_TOKEN}" \
  http://127.0.0.1:3457/health
```

`--auth-token` supplies the same value explicitly. To enable the Pyright MCP
tool, configure both the project and its dedicated private workspace root:

```bash
ccr-rust mcp-daemon \
  --pyright-root /path/to/project \
  --pyright-workspace-dir ~/.cache/ccr-rust/pyright-workspaces
```

The equivalent environment variables are `PYRIGHT_PROJECT_ROOT` and
`CCR_MCP_PYRIGHT_WORKSPACE_DIR`. CCR-Rust enforces mode `0700` on the workspace
root and each request directory and removes stale CCR-owned request directories
when the daemon starts.

## Configuration

CCR-Rust reads `~/.claude-code-router/config.json`. Supports `${ENV_VAR}` substitution.

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/coding/paas/v4",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5.2", "glm-5.1", "glm-5-turbo"],
      "transformer": {
        "use": ["anthropic", "glm"]
      },
      "tier_name": "ccr-glm"
    },
    {
      "name": "minimax",
      "api_base_url": "https://api.minimax.io/anthropic/v1",
      "api_key": "${MINIMAX_API_KEY}",
      "models": ["MiniMax-M3", "MiniMax-M2.7"],
      "transformer": {
        "use": ["minimax"]
      },
      "protocol": "anthropic",
      "tier_name": "ccr-mm"
    }
  ],
  "Router": {
    "default": "zai,glm-5.2",
    "tiers": [
      "zai,glm-5.2",
      "minimax,MiniMax-M3"
    ]
  },
  "PORT": 3456,
  "HOST": "127.0.0.1"
}
```

For full schema and provider setup, see [Configuration guide](docs/configuration.md).
For common presets (Claude-only, multi-tier, cost-optimized), see [Presets](docs/presets.md).

## A simple mental model for Claude Code users

If you only remember one thing, remember this:

- **Claude Code stays the interface.**
- **CCR-Rust becomes the local switchboard.**
- **Your actual model provider can change underneath without changing your day-to-day CLI flow.**

That is why it is useful when Claude usage limits kick in: you keep your editor, prompts, and habits, and only swap the engine behind the curtain.

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
- **Integrations:** [Claude Code fallback how-to](docs/claude_code_setup.md) · [Codex](docs/codex_setup.md) · [OpenAI SDK](docs/openai_sdk_setup.md) · [Kimi](docs/kimi_setup.md) · [Gemini](docs/gemini-integration.md)
- **Operations:** [Observability](docs/observability.md) · [Debug capture](docs/debug_capture.md) · [Streaming](docs/streaming_incremental_design.md) · [Token optimization](docs/token_optimization.md)
- **Troubleshooting:** [Common issues](docs/troubleshooting.md)

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

**Network service clause:** Modified versions of CCR-Rust offered as a network service must provide source code to users of that service.

Built for reliability. Made for scale. Join the [discussions](https://github.com/RESMP-DEV/ccr-rust/discussions).
