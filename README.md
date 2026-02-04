# CCR-Rust

> Route your Claude Code requests to any LLM backend—DeepSeek, GLM-4, OpenRouter, and more.

**CCR-Rust** is a Rust rewrite of the [Claude Code Router](https://github.com/musistudio/claude-code-router). It sits between Claude Code and your preferred LLM providers, letting you use cheaper or specialized models without changing your workflow. Drop-in compatible with existing CCR configurations.

## Why?

Love Claude Code's interface but want to use other models? CCR-Rust lets you:

- **Use any OpenAI-compatible API** as a Claude Code backend
- **Chain multiple providers** with automatic fallback (try DeepSeek first, then GLM-4, then OpenRouter)
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

PRs welcome! If you've got a provider that doesn't quite work, or you're hitting weird edge cases, open an issue. This project grew out of real-world frustrations with routing LLM traffic, and we'd love to hear about yours.

## License

MIT
