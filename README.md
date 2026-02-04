# ccr-rust

> A high-throughput SSE proxy for running 100+ AI agents in parallel—without breaking a sweat.

**ccr-rust** is a Rust rewrite of the [Claude Code Router](https://github.com/musistudio/claude-code-router), designed for orchestration systems like [AlphaHENG](https://github.com/RESMP-DEV/AlphaHENG) that need to fan out requests across dozens (or hundreds) of concurrent agents. It's drop-in compatible with existing CCR configurations.

## Why Rust?

The original Node.js router works great for single-user setups. But when you're dispatching 100+ parallel agent tasks, GC pauses and SSE string allocations start to hurt. This rewrite eliminates those bottlenecks:

| Feature | Node.js CCR | ccr-rust |
|---------|-------------|----------|
| Concurrent streams | ~20-30 stable | 200+ tested |
| SSE handling | String concat | Zero-copy buffers |
| Memory under load | Spiky (GC) | Flat ~15MB |
| Latency P99 | Variable | Predictable |

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
    "longContextThreshold": 60000
  }
}
```

## API Endpoints

| Endpoint | Description |
|----------|-------------|
| `POST /v1/messages` | Anthropic-compatible chat completions |
| `GET /v1/usage` | Aggregate token usage per tier (JSON) |
| `GET /v1/latencies` | Real-time EWMA latency stats (JSON) |
| `GET /metrics` | Prometheus scrape endpoint |
| `GET /health` | Health check |

## Observability

Prometheus metrics are exposed at `:3456/metrics`:

```
ccr_requests_total{tier="tier-0"}           # Requests per backend
ccr_request_duration_seconds{tier="tier-0"} # Latency histogram
ccr_failures_total{tier="tier-0",reason="timeout"}
ccr_active_streams                          # Current SSE connections
ccr_input_tokens_total{tier="tier-0"}       # Token accounting
ccr_output_tokens_total{tier="tier-0"}
```

## Intelligent Fallback

Requests cascade through your configured tiers with exponential backoff:

```
Request
   ↓
Tier 0 (default) ──[3 retries]──→ Tier 1 (think) ──[3 retries]──→ Tier 2 (long) ──→ Error
```

**New in ccr-rust:** Tiers are dynamically reordered by observed latency (EWMA). If Tier 2 is consistently faster than Tier 1, it gets promoted automatically.

---

## Roadmap

We're actively working toward full feature parity with the Node.js version, plus some extras:

### v0.2.0 — Format Parity (In Progress)
- [ ] **Anthropic → OpenAI translation** — Auto-convert request/response formats
- [ ] **Nested transformer config** — Full support for per-model transformers
- [ ] **Think-tag stripping** — Clean up reasoning tokens before forwarding

### v0.3.0 — Production Hardening
- [ ] **Graceful shutdown** — Drain active streams before exit
- [ ] **Rate limit awareness** — Back off when providers return 429s
- [ ] **Request deduplication** — Collapse identical concurrent requests

### v1.0.0 — Full Replacement
- [ ] **Web search integration** — Proxy to search-enabled models
- [ ] **Preset namespaces** — `/preset/my-config/v1/messages` routing
- [ ] **CLI parity** — `ccr-rust start`, `ccr-rust status`, etc.

### Future
- [ ] **Distributed mode** — ZeroMQ backend for multi-machine pools
- [ ] **Cost tracking** — Real-time spend estimates per tier

---

## Contributing

PRs welcome. If you're hitting a specific bottleneck with high-concurrency agent setups, open an issue—this project exists to solve exactly those problems.

## License

MIT
