# CCR-Rust

> Route your Claude Code requests to any LLM backend—DeepSeek, GLM-4, OpenRouter, and more.

**CCR-Rust** is a Rust rewrite of the [Claude Code Router](https://github.com/musistudio/claude-code-router). It sits between Claude Code and your preferred LLM providers, letting you use cheaper or specialized models without changing your workflow. Drop-in compatible with existing CCR configurations.

## Why?

Love Claude Code's interface but want to use other models? CCR-Rust lets you:

- **Use any OpenAI-compatible API** as a Claude Code backend
- **Chain multiple providers** with automatic fallback (try DeepSeek first, then GLM-4.7 via Z.AI, then OpenRouter)
- **Handle high concurrency** when running many agents or batch jobs

### Why Rust over the Node.js version?

For most users, either works fine. But if you're running multiple Claude Code instances, automated pipelines, or heavy workloads, ccr-rust handles the load better:

- Supports 200+ concurrent streams (vs ~30 in Node.js)
- Steady memory usage (~15MB) instead of GC spikes
- More predictable response times under pressure

## Quick Start

```sh
# Build and install
cargo install --path .

# Start the server (default: http://127.0.0.1:3456)
ccr-rust start --config ~/.claude-code-router/config.json

# Launch interactive TUI dashboard
ccr-rust dashboard

# Check if running
ccr-rust status

# Validate config
ccr-rust validate

# Show version
ccr-rust version
```

### CLI Subcommands

| Command | Description |
|---------|-------------|
| `start` | Start the CCR server (default if no command given) |
| `dashboard` | Launch interactive TUI for real-time monitoring |
| `status` | Check if server is running and show latencies |
| `validate` | Validate config file syntax and providers |
| `version` | Show version and build info |

**Options:**
- `--config, -c` - Path to config file (env: `CCR_CONFIG`)
- `--host` - Server host (default: 127.0.0.1)
- `--port, -p` - Server port (default: 3456)
- `--max-streams` - Max concurrent streams (default: 512)
- `--shutdown-timeout` - Graceful shutdown timeout in seconds (default: 30)

## Configuration

Uses the standard CCR JSON format—your existing `config.json` should just work.

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/...",
      "api_key": "your-key",
      "models": ["glm-4.7"]
    }
  ],
  "Router": {
    "default": "zai,glm-4.7",
    "think": "deepseek,deepseek-reasoner",
    "longContext": "openrouter,minimax-m2.1",
    "longContextThreshold": 1048576,
    "tierRetries": {
      "tier-0": { "max_retries": 5, "base_backoff_ms": 50, "backoff_multiplier": 1.5 },
      "tier-1": { "max_retries": 3, "base_backoff_ms": 100 }
    }
  }
}
```

### Per-Tier Retry Configuration

New in ccr-rust: fine-tune retry behavior for each tier.

| Field | Default | Description |
|-------|---------|-------------|
| `max_retries` | 3 | Retry attempts per tier (total = 1 initial + max_retries) |
| `base_backoff_ms` | 100 | Initial backoff delay |
| `backoff_multiplier` | 2.0 | Exponential multiplier per attempt |
| `max_backoff_ms` | 10000 | Maximum backoff cap |

Backoff is also dynamically scaled by the tier's observed EWMA latency—fast tiers retry more aggressively.

### Preset Namespaces (v1.0.0)

Define named routing presets with parameter overrides:

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
    }
  }
}
```

**Usage:**
```bash
# Route via preset
curl http://localhost:3456/preset/fast/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "user", "content": "Hello"}]}'

# List all presets
curl http://localhost:3456/v1/presets
```

### Web Search Integration (v1.0.0)

Automatically route requests with `[search]` or `[web]` tags to a search-capable provider:

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

Tags are stripped before sending to the provider.

## API Endpoints

| Endpoint | Description |
|----------|-------------|
| `POST /v1/messages` | Anthropic-compatible chat completions |
| `POST /preset/{name}/v1/messages` | Route via named preset |
| `GET /v1/presets` | List all configured presets |
| `GET /v1/usage` | Aggregate token usage per tier (JSON) |
| `GET /v1/latencies` | Real-time EWMA latency stats (JSON) |
| `GET /v1/token-drift` | Token estimation accuracy per tier |
| `GET /v1/token-audit` | Recent pre-request token breakdowns |
| `GET /metrics` | Prometheus scrape endpoint |
| `GET /health` | Health check |

## Observability

Prometheus metrics are exposed at `:3456/metrics`:

```
ccr_requests_total{tier="tier-0"}           # Requests per backend
ccr_request_duration_seconds{tier="tier-0"} # Latency histogram
ccr_tier_ewma_latency_seconds{tier="tier-0"} # EWMA latency gauge
ccr_failures_total{tier="tier-0",reason="timeout"}
ccr_active_streams                          # Current SSE connections
ccr_peak_active_streams                     # High-water mark
ccr_stream_backpressure_total               # Buffer overflow events
ccr_input_tokens_total{tier="tier-0"}       # Token accounting
ccr_output_tokens_total{tier="tier-0"}
ccr_pre_request_tokens_total{tier,component} # Estimated tokens before dispatch
ccr_token_drift_pct{tier="tier-0"}          # Local vs upstream token accuracy
```

### Terminal Dashboard (TUI)

CCR-Rust includes a built-in interactive dashboard for monitoring traffic in real-time without external dependencies.

```bash
ccr-rust dashboard --port 3456
```

**Layout:**
```
┌─────────────────────────────────────────────────────────────────────┐
│ CCR-Rust Dashboard | 127.0.0.1:3456                                 │
│ Active Streams: 5    │ Requests: 1,234 / Failures: 12 (99.0%)       │
│                      │ In: 450.2k / Out: 89.1k                      │
├─────────────────────────────────────────────────────────────────────┤
│ Token Drift Monitor                                                 │
│ Tier      │ Samples │ Cumulative Drift % │ Last Sample Drift %      │
│ tier-0    │ 117     │ 2.3%               │ 1.8%                     │
│ tier-1    │ 66      │ -1.2%              │ 0.5%                     │
├─────────────────────────────────────────────────────────────────────┤
│ Session Info         │ Tier Statistics                              │
│ CWD: /path/to/proj   │ Tier   │ EWMA (ms) │ Requests │ Tokens       │
│ Git Branch: main     │ tier-0 │ 1,921     │ 100/5    │ 350k/80k     │
│ Version: 1.0.0       │ tier-1 │ 2,722     │ 60/3     │ 100k/9k      │
└─────────────────────────────────────────────────────────────────────┘
```

**Panels:**
- **Header**: Active streams (green when >0), success rate with color coding, token throughput (In/Out in 'k' units)
- **Token Drift Monitor**: Per-tier comparison of local tiktoken estimates vs upstream-reported usage. Yellow for >10% drift, red for >25%
- **Session Info**: Current working directory, git branch, and project version
- **Tier Statistics**: Per-tier EWMA latency (color-coded by speed), request success/failure counts, token consumption, average duration

**Keyboard shortcuts:**
- `q` or `Esc` — Exit dashboard

### Token Drift Verification

CCR-Rust estimates token counts *before* dispatching requests (using tiktoken's `cl100k_base`) and compares against upstream-reported usage. The `/v1/token-drift` endpoint surfaces cumulative accuracy stats:

```json
[{
  "tier": "tier-0",
  "samples": 150,
  "cumulative_drift_pct": 2.3,
  "last_drift_pct": 1.8
}]
```

Alerts fire automatically when drift exceeds 10% (warning) or 25% (critical).

## Intelligent Fallback

Requests cascade through your configured tiers with exponential backoff:

```
Request
   ↓
Tier 0 (default) ──[4 attempts]──→ Tier 1 (think) ──[4 attempts]──→ Tier 2 (long) ──→ Error
```

Each tier makes **1 initial + N retries** attempts (default: 1+3=4). The `max_retries` setting controls the retry count, not total attempts.

**Dynamic tier reordering:** Tiers are automatically reordered by observed latency (EWMA). If Tier 2 is consistently faster than Tier 1, it gets promoted. Tiers with fewer than 3 samples keep their configured priority.

**Adaptive backoff:** Retry delays are scaled by the tier's EWMA latency—fast tiers get shorter backoffs, degraded tiers back off longer to avoid pile-on.

---

## What's Implemented

### ✅ Core (v0.1.0)
- Zero-copy SSE streaming
- Multi-tier cascade with EWMA routing
- Token drift verification
- Prometheus metrics

### ✅ Format Parity (v0.2.0)  
- OpenAI ↔ Anthropic translation
- Reasoning model support
- Transformer infrastructure

### ✅ Production (v0.3.0)
- Graceful shutdown
- Rate limit handling
- Think-tag stripping

### ✅ Full Feature (v1.0.0)
- CLI subcommands (start/status/validate/version)
- Preset namespaces
- Web search integration
- Docker/Kubernetes deployment

## Installation

```bash
# From source
cargo install --path .
ccr-rust start

# Docker
docker compose up -d

# Or manually
docker run -v ./config.json:/etc/ccr/config.json ghcr.io/resmp-dev/ccr-rust

# Kubernetes
kubectl apply -f k8s/

# Systemd (Linux)
sudo cp deploy/ccr-rust.service /etc/systemd/system/
sudo systemctl enable --now ccr-rust

# Launchd (macOS)
cp deploy/com.ccr.rust.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.ccr.rust.plist
```

See `docs/` for detailed deployment guides.

---

## Stress Testing

The `benchmarks/` directory contains a self-contained stress test suite:

```bash
# Run the full orchestrated test (starts mock backend + ccr-rust + 100 streams)
./benchmarks/run_stress_test.sh --streams 100 --chunks 20

# Manual setup for debugging
uv run python benchmarks/mock_sse_backend.py --port 9999 &
./target/release/ccr-rust --config benchmarks/config_mock.json &
uv run python benchmarks/stress_sse_streams.py --streams 100
```

See `benchmarks/README.md` for options and metrics collected.

---

## Contributing

PRs welcome! If you've got a provider that doesn't quite work, or you're hitting weird edge cases, open an issue. This project grew out of real-world frustrations with routing LLM traffic, and we'd love to hear about yours.

## License

MIT
