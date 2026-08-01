# CCR-Rust

CCR-Rust is a lightweight, self-hosted **LLM router** that sits between your AI coding tools (Claude Code, Codex, OpenCode, or any OpenAI-compatible client) and your model providers. Your client talks to one local URL; CCR-Rust decides which provider and model actually handles each request, translates between API formats when needed, and fails over automatically when a provider has problems. It is aimed at developers and teams who want to keep using their favorite coding CLI even when they need to switch providers, spread load across several models, or keep working after a usage limit runs out.

The most common reason people install it: **when your Claude plan runs out, keep using Claude Code with GLM, MiniMax, DeepSeek, or another provider instead of changing tools.**

## Features

- **Automatic failover** — providers are arranged in tiers; 5xx errors and timeouts cascade to the next tier automatically. Rate limits (429) are tracked and reported transparently.
- **Multi-protocol** — one local endpoint speaks both the Anthropic API (`/v1/messages`) and the OpenAI API (`/v1/chat/completions`, `/v1/responses`), with request/response translation handled for you.
- **Cost- and class-aware routing** — send different traffic classes (default / think / background / web search) to different models, optionally with GP-backed request-aware reranking.
- **Observability** — Prometheus metrics, a live terminal dashboard, and token/latency/cost tracking per provider tier.
- **MCP aggregation** — optional Model Context Protocol server and shared daemon for tool proxying.
- **Compression** — response and tool-output compression transformers for long-running agent sessions.

Small footprint: roughly a 15 MB binary with <50 ms P99 routing overhead, designed to stay out of your way.

## How it works

```
 ┌──────────────┐        ┌────────────┐        ┌─────────────────────┐
 │ Claude Code  │ ─────▶ │            │ ─────▶ │ Z.AI (GLM-5.2)      │  tier 0
 │ Codex        │ ─────▶ │  CCR-Rust  │ ─────▶ │ MiniMax (M3)        │  tier 1
 │ OpenAI SDKs  │ ─────▶ │ :3456      │ ─────▶ │ DeepSeek            │  tier 2
 └──────────────┘        └────────────┘        └─────────────────────┘
   one local URL          picks a tier,          translates formats,
                          retries + fails over   streams responses back
```

1. Your client sends a request to CCR-Rust (Anthropic-style or OpenAI-style).
2. CCR-Rust picks a configured provider/model using your routing rules.
3. If the upstream provider uses a different API format, CCR-Rust translates the request.
4. It sends the request upstream, collects (or streams) the response, and translates it back if needed.
5. Your client sees the format it expects — the engine underneath can change without changing your workflow.

## Installation

### From source (recommended)

You need a Rust toolchain ([rustup](https://rustup.rs/) is the easiest way to get one).

```bash
git clone https://github.com/RESMP-DEV/ccr-rust.git
cd ccr-rust
cargo build --release
cargo install --path . --force
```

This installs the `ccr-rust` binary into `~/.cargo/bin` (make sure that directory is on your `PATH`).

### Docker

A `Dockerfile` and `docker-compose.yml` are included. The compose file mounts `./config.json` from the repository root:

```bash
cp config.example.json config.json   # then edit with your providers/keys
docker compose up --build
```

Or build and run the image directly:

```bash
docker build -t ccr-rust .
docker run --rm -p 3456:3456 \
  -v ./config.json:/etc/ccr/config.json:ro \
  -e CCR_CONFIG=/etc/ccr/config.json \
  ccr-rust
```

### Prebuilt binaries

Prebuilt release binaries are not published yet — build from source or use Docker. Watch the repository's Releases page for future binary releases.

## Quickstart

This gets a router running locally with one provider in about two minutes.

### 1. Install

Follow [Installation](#installation) above, then verify:

```bash
ccr-rust version
```

### 2. Create a config

```bash
mkdir -p ~/.claude-code-router
cp config.example.json ~/.claude-code-router/config.json
```

Open `~/.claude-code-router/config.json` and keep only providers you have API keys for. A minimal single-provider config looks like this (see [`examples/config.minimal.json`](examples/config.minimal.json) for a copy-paste version):

```json
{
  "Providers": [
    {
      "name": "deepseek",
      "api_base_url": "https://api.deepseek.com",
      "api_key": "${DEEPSEEK_API_KEY}",
      "models": ["deepseek-chat"]
    }
  ],
  "Router": {
    "default": "deepseek,deepseek-chat"
  },
  "PORT": 3456,
  "HOST": "127.0.0.1"
}
```

`${DEEPSEEK_API_KEY}` is expanded from your environment, so export your key first:

```bash
export DEEPSEEK_API_KEY="sk-..."
```

### 3. Validate and start

```bash
ccr-rust validate     # checks the config before you start
ccr-rust start        # starts the router on 127.0.0.1:3456
```

In a second terminal:

```bash
ccr-rust status       # ✓ CCR-Rust running on 127.0.0.1:3456
```

### 4. Send your first request

```bash
curl -s http://127.0.0.1:3456/v1/messages \
  -H "content-type: application/json" \
  -d '{
    "model": "deepseek-chat",
    "max_tokens": 256,
    "messages": [{"role": "user", "content": "Say hello in one sentence."}]
  }'
```

### 5. Point your coding tool at CCR-Rust

```bash
# Claude Code
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude

# Codex (configure CCR-Rust as a custom provider first)
# See docs/codex_setup.md for the current config.toml settings.
codex --profile ccr

# Other OpenAI-compatible clients
# Set the client's base URL to http://127.0.0.1:3456/v1 using its
# provider configuration; the exact setting varies by client.
```

If Claude Code complains about a missing `ANTHROPIC_API_KEY`, keep that variable set to any non-empty value locally — CCR-Rust uses the upstream provider keys from its own config file, not the one from your client.

More examples (multi-tier failover, presets, a smoke-test script) live in [`examples/`](examples/).

## Usage

Run `ccr-rust --help` for the full reference. The most common commands:

| Command | Purpose |
| ------- | ------- |
| `ccr-rust start` | Start the router (default command — running bare `ccr-rust` does the same) |
| `ccr-rust status` | Check whether the router is running |
| `ccr-rust validate` | Validate your config file without starting the server |
| `ccr-rust dashboard` | Live terminal dashboard with latencies, usage, and tier health |
| `ccr-rust captures` | List and inspect captured request/response pairs (when debug capture is enabled) |
| `ccr-rust clear-stats` | Clear persisted stats in Redis |
| `ccr-rust mcp` | Run as a stdio MCP server (tool aggregation) |
| `ccr-rust mcp-daemon` | Run as a shared HTTP MCP daemon (requires a bearer token) |
| `ccr-rust version` | Show version and build info |

### Examples

```bash
# Start on a custom address/port
ccr-rust start --host 0.0.0.0 --port 8080

# Use an alternate config file (global flag, works with any command)
ccr-rust --config /etc/ccr/config.json start

# Unlimited concurrent streams
ccr-rust start --max-streams 0

# Check a router running somewhere else
ccr-rust status --host 10.0.0.5 --port 3456

# Dashboard against a remote router (also: CCR_DASHBOARD_HOST / CCR_DASHBOARD_PORT)
ccr-rust dashboard --host 10.0.0.5 --port 3456

# Debug capture statistics, or the 10 most recent captures for one provider
ccr-rust captures --stats
ccr-rust captures --provider minimax --limit 10 --full

# MCP daemon with bearer auth (prefer the env var over the flag)
export CCR_MCP_AUTH_TOKEN="a-private-random-token"
ccr-rust mcp-daemon --port 3457
```

### API surface

| Endpoint | Method | Purpose |
| -------- | ------ | ------- |
| `/v1/messages` | POST | Anthropic messages API |
| `/v1/chat/completions` | POST | OpenAI chat completions API |
| `/v1/responses` | POST | OpenAI responses API (streaming) |
| `/v1/models` | GET | List configured models |
| `/preset/{name}/v1/messages` | POST | Messages API routed through a named preset |
| `/v1/presets` | GET | List configured presets |
| `/v1/latencies` | GET | Per-tier latency stats |
| `/v1/usage` | GET | Token/cost usage summary |
| `/v1/token-drift` | GET | Estimated vs. actual token drift |
| `/v1/token-audit` | GET | Recent per-request token audits |
| `/v1/throughput` | GET | Throughput stats |
| `/v1/frontend-metrics` | GET | Per-client-type request/latency metrics |
| `/health` | GET | Health check |
| `/metrics` | GET | Prometheus metrics |

## Configuration

CCR-Rust reads `~/.claude-code-router/config.json` by default (override with `--config` or `CCR_CONFIG`). Values like `"${MY_API_KEY}"` are expanded from the environment, so secrets never need to live in the file.

The three sections you'll touch most:

- **`Providers`** — upstream endpoints: name, base URL, API key, models, optional transformers.
- **`Router`** — routing rules: `default` route, optional `background` / `think` / `webSearch` routes, the `tiers` failover cascade, retries, and GP routing.
- **`Presets`** — named routes with parameter overrides, reachable at `/preset/{name}/v1/messages`.

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/coding/paas/v4",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5.2", "glm-5.1", "glm-5-turbo"],
      "transformer": { "use": ["anthropic", "glm"] }
    },
    {
      "name": "minimax",
      "api_base_url": "https://api.minimax.io/anthropic/v1",
      "api_key": "${MINIMAX_API_KEY}",
      "models": ["MiniMax-M3", "MiniMax-M2.7"],
      "transformer": { "use": ["minimax"] },
      "protocol": "anthropic"
    }
  ],
  "Router": {
    "default": "zai,glm-5.2",
    "tiers": ["zai,glm-5.2", "minimax,MiniMax-M3"]
  },
  "PORT": 3456,
  "HOST": "127.0.0.1"
}
```

Full schema, transformer reference, and provider-specific setup: [docs/configuration.md](docs/configuration.md). Ready-made configs for common scenarios: [docs/presets.md](docs/presets.md) and [`examples/`](examples/).

### How rate limits and failures behave

- **429 responses** are passed through to the client with a normalized error body (`type: "rate_limit_error"`, `code: "rate_limited"`) and an `x-ccr-tier` header identifying which provider was rate-limited. The rate limit is tracked internally so future requests skip that tier while it backs off.
- **5xx / timeout errors** cascade to the next tier automatically. The client only sees an error if every tier is exhausted.
- **Informational headers** (`X-RateLimit-Remaining: 0` on 200 responses) trigger proactive tier-skipping by default. Set `"honor_ratelimit_headers": false` per provider for those (like Z.AI) that send these as informational warnings without actual enforcement.

## Observability

```bash
# Prometheus metrics
curl http://localhost:3456/metrics

# Live TUI dashboard
ccr-rust dashboard

# Dashboard against a remote router
CCR_DASHBOARD_HOST=10.0.0.5 CCR_DASHBOARD_PORT=3456 ccr-rust dashboard
```

Tracks token counts (in/out), latencies (p50/p90/p99), provider success rates, cost per tier, and estimated-vs-actual token drift. Stats can optionally be persisted to Redis so they survive restarts (see [docs/cli.md](docs/cli.md#redis-persistence)).

## Building from source & development

```bash
cargo check          # fast compile check
cargo fmt            # format
cargo clippy         # lint
cargo test           # unit + integration tests (mocked upstreams)
cargo build --release
cargo install --path . --force   # keep the installed CLI in sync with your build
```

Default build features: `dashboard`, `gp` (GP routing), `sindexer`. The repo layout and contribution workflow are described in [AGENTS.md](AGENTS.md) and [CONTRIBUTING.md](CONTRIBUTING.md).

## Troubleshooting / FAQ

**`ccr-rust: command not found` after installing**
`cargo install` puts binaries in `~/.cargo/bin`. Add it to your `PATH`: `export PATH="$HOME/.cargo/bin:$PATH"`.

**`status` says "Not running" but I just started it**
`status` checks `127.0.0.1:3456` by default. If you started with a different host/port, pass the same values: `ccr-rust status --host 0.0.0.0 --port 8080`.

**Claude Code asks for `ANTHROPIC_API_KEY`**
Set it to any non-empty value in the client's environment. CCR-Rust authenticates upstream using the keys in its own config file.

**My config changes had no effect**
Config is read at startup. Restart the router after editing `config.json` (Ctrl+C, then `ccr-rust start`). Run `ccr-rust validate` first to catch syntax errors.

**`${MY_KEY}` isn't being substituted**
Substitution uses the router process's environment. Export the variable in the same shell/session that starts `ccr-rust` (or configure it in your service manager). Validate with `ccr-rust validate`.

**All my traffic goes to one (wrong) tier**
Clients like Claude Code and Codex cache the last successful model and echo it back, which CCR-Rust honors as "direct routing". Set `"Router": { "ignoreDirect": true }` to enforce your configured tier order. See [docs/troubleshooting.md](docs/troubleshooting.md#requests-bypassing-tier-order).

**I need deeper debugging**

```bash
RUST_LOG=ccr_rust=debug,tower_http=debug ccr-rust start
curl localhost:3456/health
curl localhost:3456/metrics | grep ccr_
```

More common issues, rate-limit diagnostics, and streaming failure events: [docs/troubleshooting.md](docs/troubleshooting.md).

## Documentation

See [docs/index.md](docs/index.md) for the full index:

- **Setup:** [CLI reference](docs/cli.md) · [Configuration](docs/configuration.md) · [Presets](docs/presets.md) · [Deployment](docs/deployment.md)
- **Integrations:** [Claude Code fallback how-to](docs/claude_code_setup.md) · [Codex](docs/codex_setup.md) · [OpenAI SDK](docs/openai_sdk_setup.md) · [Kimi](docs/kimi_setup.md) · [Gemini](docs/gemini-integration.md) · [Z.AI & MiniMax](docs/zai_minimax_setup.md)
- **Operations:** [Observability](docs/observability.md) · [Debug capture](docs/debug_capture.md) · [Streaming](docs/streaming_incremental_design.md) · [Token optimization](docs/token_optimization.md)
- **Troubleshooting:** [Common issues](docs/troubleshooting.md)

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

**Network service clause:** Modified versions of CCR-Rust offered as a network service must provide source code to users of that service.

Join the [discussions](https://github.com/RESMP-DEV/ccr-rust/discussions).
