# Observability

CCR-Rust exposes metrics, dashboards, and debugging endpoints for monitoring your routing setup.

## Prometheus Metrics

Metrics are exposed at `:3456/metrics`:

```
# Request counts per tier
ccr_requests_total{tier="tier-0"}
ccr_failures_total{tier="tier-0",reason="timeout"}

# Latency
ccr_request_duration_seconds{tier="tier-0"}  # Histogram
ccr_tier_ewma_latency_seconds{tier="tier-0"} # EWMA gauge

# Streaming
ccr_active_streams                    # Current SSE connections
ccr_peak_active_streams               # High-water mark
ccr_stream_backpressure_total         # Buffer overflow events

# Token accounting
ccr_input_tokens_total{tier="tier-0"}
ccr_output_tokens_total{tier="tier-0"}
ccr_pre_request_tokens_total{tier,component}  # Estimated before dispatch
ccr_token_drift_pct{tier="tier-0"}            # Local vs upstream accuracy
```

## API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /v1/usage` | Aggregate token usage per tier (JSON) |
| `GET /v1/latencies` | Real-time EWMA latency stats (JSON) |
| `GET /v1/token-drift` | Token estimation accuracy per tier |
| `GET /v1/token-audit` | Recent pre-request token breakdowns |
| `GET /metrics` | Prometheus scrape endpoint |
| `GET /health` | Health check |

## Terminal Dashboard (TUI)

CCR-Rust includes an interactive dashboard for real-time monitoring:

```bash
ccr-rust dashboard --port 3456
```

### Layout

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

### Panels

- **Header**: Active streams (green when >0), success rate with color coding, token throughput (In/Out)
- **Token Drift Monitor**: Per-tier comparison of local tiktoken estimates vs upstream-reported usage. Yellow for >10% drift, red for >25%
- **Session Info**: Current working directory, git branch, version
- **Tier Statistics**: Per-tier EWMA latency (color-coded), request success/failure counts, token consumption

### Keyboard Shortcuts

- `q` or `Esc` — Exit dashboard

## Token Drift Verification

CCR-Rust estimates token counts *before* dispatching requests (using tiktoken's `cl100k_base`) and compares against upstream-reported usage.

### Why This Matters

If your local token estimates are off, you might:
- Route to the wrong tier (e.g., `longContext` threshold not triggered)
- Exceed provider limits unexpectedly
- Underestimate costs

### Checking Drift

The `/v1/token-drift` endpoint returns:

```json
[{
  "tier": "tier-0",
  "samples": 150,
  "cumulative_drift_pct": 2.3,
  "last_drift_pct": 1.8
}]
```

### Alerts

Drift alerts are automatic:
- **Warning**: >10% cumulative drift
- **Critical**: >25% cumulative drift

These appear in the TUI dashboard and can be scraped via Prometheus.

## Intelligent Fallback Details

Requests cascade through configured tiers with exponential backoff:

```
Request
   ↓
Tier 0 (default) ──[4 attempts]──→ Tier 1 (think) ──[4 attempts]──→ Tier 2 (long) ──→ Error
```

Each tier makes **1 initial + N retries** attempts (default: 1+3=4). The `max_retries` setting controls the retry count.

### Dynamic Tier Reordering

Tiers are automatically reordered by observed latency (EWMA). If Tier 2 is consistently faster than Tier 1, it gets promoted.

Tiers with fewer than 3 samples keep their configured priority—no premature reordering.

### Adaptive Backoff

Retry delays are scaled by the tier's EWMA latency:
- Fast tiers get shorter backoffs (retry quickly)
- Degraded tiers back off longer (avoid pile-on)

This prevents cascading failures when a tier is temporarily overloaded.
