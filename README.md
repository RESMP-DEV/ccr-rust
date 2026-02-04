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

# Run with your existing CCR config
ccr-rust --config ~/.claude-code-router/config.json

# Or via environment variable
export CCR_CONFIG=~/.claude-code-router/config.json
ccr-rust
```

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
    "longContextThreshold": 60000,
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
| `max_retries` | 3 | Maximum retry attempts per tier |
| `base_backoff_ms` | 100 | Initial backoff delay |
| `backoff_multiplier` | 2.0 | Exponential multiplier per attempt |
| `max_backoff_ms` | 10000 | Maximum backoff cap |

Backoff is also dynamically scaled by the tier's observed EWMA latency—fast tiers retry more aggressively.

## API Endpoints

| Endpoint | Description |
|----------|-------------|
| `POST /v1/messages` | Anthropic-compatible chat completions |
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
Tier 0 (default) ──[3 retries]──→ Tier 1 (think) ──[3 retries]──→ Tier 2 (long) ──→ Error
```

**Dynamic tier reordering:** Tiers are automatically reordered by observed latency (EWMA). If Tier 2 is consistently faster than Tier 1, it gets promoted. Tiers with fewer than 3 samples keep their configured priority.

**Adaptive backoff:** Retry delays are scaled by the tier's EWMA latency—fast tiers get shorter backoffs, degraded tiers back off longer to avoid pile-on.

---

## What's Implemented

### ✅ Core Features (v0.1.0)
- [x] Zero-copy SSE streaming with backpressure detection
- [x] Shared HTTP connection pool (configurable idle connections/timeout)
- [x] Multi-tier cascade with per-tier retry configuration
- [x] EWMA latency tracking (per-attempt, not per-request total)
- [x] Latency-aware tier reordering
- [x] Adaptive backoff with EWMA scaling
- [x] Pre-request token estimation (tiktoken cl100k_base)
- [x] Token drift verification (local vs upstream)
- [x] Full nested transformer config parsing
- [x] Provider-level and model-level transformer chains
- [x] Comprehensive Prometheus metrics
- [x] Stream usage extraction from SSE events
- [x] Integration test suite with wiremock
- [x] Python stress test suite (100+ concurrent streams)

### ✅ Format Parity (v0.2.0)
- [x] **Anthropic → OpenAI translation** — Full request conversion (system prompt, messages, tools)
- [x] **OpenAI → Anthropic translation** — Response format conversion (streaming + non-streaming)
- [x] **Reasoning model support** — DeepSeek-R1 reasoning_content → thinking blocks
- [x] **Transformer infrastructure** — Trait, chain, registry for composable transformations
- [x] **Built-in transformers** — anthropic, deepseek, openrouter, tooluse, maxtoken, reasoning, enhancetool

### 🔨 Transformer Config Support
The config parser fully supports the Node.js nested transformer patterns:

```json
{
  "transformer": {
    "use": ["deepseek", ["maxtoken", {"max_tokens": 65536}]],
    "deepseek-chat": { "use": ["tooluse"] }
  }
}
```

**Transformers available:**

| Name | Description |
|------|-------------|
| `anthropic` | Anthropic API passthrough |
| `anthropic-to-openai` | Convert tool definitions and tool_choice |
| `deepseek` | DeepSeek-specific tool normalization |
| `openrouter` | OpenRouter format handling |
| `tooluse` | Ensure tool blocks have IDs and input_schema |
| `maxtoken` | Cap/override max_tokens (configurable) |
| `reasoning` | Convert reasoning_content to thinking blocks |
| `enhancetool` | Add cache_control metadata to tool blocks |
| `identity` | No-op passthrough |

---

## Roadmap

### v0.3.0 — Production Hardening (Next)
- [ ] **Graceful shutdown** — Drain active streams before exit
- [ ] **Request cancellation** — Abort upstream when client disconnects
- [ ] **Rate limit awareness** — Back off on 429s, circuit breaker
- [ ] **Think-tag stripping** — Clean up `<think>` blocks from reasoning models

### v1.0.0 — Full Replacement
- [ ] **Web search integration** — Proxy to search-enabled models
- [ ] **Preset namespaces** — `/preset/my-config/v1/messages` routing
- [ ] **CLI parity** — `ccr-rust start`, `ccr-rust status`, etc.

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
