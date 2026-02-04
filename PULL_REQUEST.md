# feat: EWMA routing, token drift verification, and stress testing

## Summary

This PR adds several major features to ccr-rust, bringing it closer to production-ready status for high-concurrency LLM proxy use cases.

## What's Changed

### 🚀 EWMA-Based Latency Tracking (`src/routing.rs`)

A new `EwmaTracker` module provides per-tier latency tracking with exponentially-weighted moving averages:

- **Per-attempt tracking**: Measures actual backend responsiveness, not total request time across retries
- **Failure penalty**: Failed requests apply a 2x penalty to the tier's EWMA instead of recording timeout duration
- **Automatic tier reordering**: Tiers with enough samples (≥3) are sorted by observed latency—fast tiers get promoted automatically
- **Scoped `AttemptTimer`**: RAII timer that records failure on drop if not explicitly finished

### 📊 Token Drift Verification (`src/metrics.rs`)

Pre-request token estimation and post-request verification to catch tokenizer drift:

- **Pre-request audit**: Estimates input tokens using tiktoken's `cl100k_base` before dispatch
- **Per-component breakdown**: Tracks tokens from messages, system prompt, and tool definitions separately
- **Drift verification**: Compares local estimate against upstream-reported `input_tokens`
- **Alert thresholds**: Fires Prometheus counters at 10% (warning) and 25% (critical) drift
- **New endpoints**: 
  - `GET /v1/token-drift` - Cumulative drift stats per tier
  - `GET /v1/token-audit` - Ring buffer of recent pre-request audits

### 🔄 Per-Tier Retry Configuration (`src/config.rs`)

Fine-grained retry control per backend tier:

```json
{
  "Router": {
    "tierRetries": {
      "tier-0": { "max_retries": 5, "base_backoff_ms": 50, "backoff_multiplier": 1.5 }
    }
  }
}
```

- **Adaptive backoff**: `backoff_duration_with_ewma()` scales delays by the tier's latency—fast tiers retry aggressively, slow tiers back off
- **Full serde support**: Deserializes from config, falls back to defaults for missing fields

### 📡 SSE Stream Improvements (`src/sse.rs`)

- **Usage extraction**: Parses `usage` block from SSE events (message_delta, message_stop) for streaming requests
- **Backpressure tracking**: Records when the channel buffer is full (`ccr_stream_backpressure_total`)
- **Token verification**: Verifies drift on stream completion, same as non-streaming requests
- **Configurable buffer**: `SSE_BUFFER_SIZE` config option (default: 32 chunks)

### 🧪 Stress Test Suite (`benchmarks/`)

Self-contained Python stress test for validating concurrency:

```bash
./benchmarks/run_stress_test.sh --streams 100 --chunks 20
```

- `mock_sse_backend.py` - Configurable mock LLM backend
- `stress_sse_streams.py` - Launches N concurrent streams, measures TTFB/duration/throughput
- `run_stress_test.sh` - Orchestrates the full test with cleanup
- No real API keys or token costs required

### 🔧 Other Changes

- **Shared HTTP client**: Single `reqwest::Client` with configurable pool size (`POOL_MAX_IDLE_PER_HOST`, `POOL_IDLE_TIMEOUT_MS`)
- **AppState refactor**: Router now uses `AppState { config, ewma_tracker }` instead of bare `Config`
- **New Prometheus metrics**: `ccr_tier_ewma_latency_seconds`, `ccr_peak_active_streams`, `ccr_pre_request_tokens_total`, `ccr_token_drift_*`
- **Integration tests**: `tests/test_routing.rs` with wiremock-based backend simulation

## New Dependencies

- `tiktoken-rs = "0.6"` - BPE tokenizer for pre-request auditing
- `parking_lot = "0.12"` - Fast RwLock for metrics state
- `humantime = "2"` - RFC3339 timestamps for audit log (via `humantime` feature)
- `wiremock = "0.6"` (dev) - HTTP mocking for integration tests
- `tempfile = "3"` (dev) - Config file generation in tests

## Testing

```bash
# Unit tests (includes EWMA, backoff, config parsing)
cargo test

# Integration tests (requires tokio runtime)
cargo test --test test_routing

# Stress test (starts mock backend + ccr-rust)
./benchmarks/run_stress_test.sh --streams 100
```

## Breaking Changes

None. Existing configs work unchanged.

## Remaining Work (tracked in README roadmap)

- [ ] Anthropic → OpenAI request translation
- [ ] OpenAI → Anthropic response translation  
- [ ] Think-tag stripping
- [ ] Graceful shutdown

---

**Related issues:** None (greenfield feature work)
