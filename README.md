# ccr-rust

Claude Code Router in Rust. High-throughput SSE proxy for agent swarms.

Inspired by [musistudio/claude-code-router](https://github.com/musistudio/claude-code-router). Compatible with CCR config format.

## Why

- 100+ concurrent agents
- Zero-copy SSE streaming
- Predictable latency under load
- Built-in per-tier metrics

## Install

```sh
cargo install --path .
```

## Run

```sh
ccr-rust --config ~/.claude-code-router/config.json
```

Or use environment variable:

```sh
export CCR_CONFIG=~/.claude-code-router/config.json
ccr-rust
```

## Config

Uses standard CCR JSON format. See `config.example.json`.

Minimum required fields:
- `providers`: Array of LLM provider configs
- `Router.default`: Default model route

## Endpoints

- `POST /v1/messages` - Anthropic-compatible chat completions
- `GET /health` - Health check
- `GET /metrics` - Prometheus metrics

## Metrics

Available at `:3456/metrics`:

- `ccr_requests_total{tier}` - Total requests per tier
- `ccr_request_duration_seconds{tier}` - Request latency histogram
- `ccr_failures_total{tier,reason}` - Failure counts
- `ccr_active_streams` - Current active SSE streams

## Tier Fallback

Follows `Router` config with 3 retries per tier:

```
Request → Tier 0 (Claude) → Tier 1 (CCR-GLM) → Tier 2 (CCR-DS) → ... → Error
            ↓ fail            ↓ fail            ↓ fail
          [3 retries]       [3 retries]       [3 retries]
```

## License

MIT
