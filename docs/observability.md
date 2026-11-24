# Observability

CCR-Rust exposes metrics, dashboards, and debugging endpoints for monitoring your routing setup.

## Persistence (Redis)

By default, observability state is in-memory and resets when the CCR-Rust server restarts.

To persist dashboard and metrics state across restarts, configure Redis in `config.json`:

```json
"Persistence": {
  "mode": "redis",
  "redis_url": "redis://127.0.0.1:6379/0",
  "redis_prefix": "ccr-rust:persistence:v1"
}
```

When enabled, CCR-Rust restores:

- counters and gauges used by `/metrics`,
- histogram offsets used for latency/token distributions,
- `/v1/token-drift` aggregate state,
- `/v1/token-audit` ring buffer,
- EWMA tier latency state used by `/v1/latencies`.

Quick verification after restart:

```bash
# Redis health
redis-cli -h 127.0.0.1 -p 6379 ping

# CCR health
curl -sS http://127.0.0.1:3456/health

# Restored JSON endpoints
curl -sS http://127.0.0.1:3456/v1/usage | jq .
curl -sS http://127.0.0.1:3456/v1/token-drift | jq .
curl -sS http://127.0.0.1:3456/v1/frontend-metrics | jq .

# Prometheus endpoint
curl -sS http://127.0.0.1:3456/metrics | grep '^ccr_'
```

Reset persisted observability state (keeps other Redis data untouched):

```bash
ccr-rust clear-stats
```

Override Redis target explicitly when needed:

```bash
ccr-rust clear-stats \
  --redis-url redis://127.0.0.1:6379/0 \
  --redis-prefix ccr-rust:persistence:v1
```

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

| Endpoint              | Description                           |
| --------------------- | ------------------------------------- |
| `GET /v1/usage`       | Aggregate token usage per tier (JSON) |
| `GET /v1/latencies`   | Real-time EWMA latency stats (JSON)   |
| `GET /v1/token-drift` | Token estimation accuracy per tier    |
| `GET /v1/token-audit` | Recent pre-request token breakdowns   |
| `GET /metrics`        | Prometheus scrape endpoint            |
| `GET /health`         | Health check                          |

## Per-Model Benchmark Harness

`scripts/ccr_model_benchmark.py` runs a fixed, verifiable task suite against the
local CCR HTTP API and writes one durable scorecard per route under
`build/reports/model-benchmark/<slug>.json`. It exists so route comparison
rests on observable behavior, did the response satisfy the contract, how fast
did it arrive, and how repeatable is that, rather than on impressions.

```bash
# One route, three repetitions per scenario
python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --reps 3

# Compare two routes
python3 scripts/ccr_model_benchmark.py \
  --models zai,glm-5.4 \
  --models zai,glm-5.3-flash

# Parse the suite and print the plan without contacting the router
python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --dry-run

# Machine-readable summary on stdout
python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --json
```

A route id is `provider,model`, so a comma is never a separator. Pass
`--models` once per route, or separate several routes with `;`.

### Scenarios

| Scenario | Kind | What it proves | Skipped when |
| --- | --- | --- | --- |
| `exact_instruction` | `exact_text` | the whole response is the requested token after trimming surrounding whitespace | never |
| `strict_json` | `strict_json` | the whole response is one JSON object with the exact keys and value types | never |
| `code_fix` | `code_fix` | the returned Python snippet, executed locally, satisfies fixed assertions | `--no-exec` is passed |
| `arithmetic_chain` | `arithmetic` | the final integer of a multi-step word problem is correct | never |
| `vision_two_color` | `vision` | the left block of a generated two-color PNG is named correctly | the route is not declared with `--vision-capable` |

A skipped scenario is recorded with an explicit reason and contributes no
accuracy signal. The accuracy denominator counts attempted runs only.

### What a scorecard records

Per run: pass or fail, time to first token, wall time, output tokens, input
tokens, error class, output text length, and the tier provenance of the
response. Aggregated per route: accuracy, median TTFT, median wall time,
tokens per second, stability, failure rate, and tier provenance.

- `accuracy` is passed runs over attempted runs.
- `tokens_per_second` is reported output tokens over summed generation time,
  wall time minus TTFT. Upstream-reported usage is preferred; when a route
  reports none, output tokens are estimated from the response text and the run
  records `usage_source: "estimated"`, which makes that throughput approximate.
- `stability` is the share of runs that agree with the majority outcome of
  their own scenario, so a route that fails the same scenario every time
  scores `1.0` there.
- `failure_rate` is the complement of accuracy over attempted runs.

Error classes separate transport failures from wrong answers: `http_error`,
`timeout`, `connection_error`, `protocol_error`, `network_error`,
`check_failed`, and `skipped`.

### Tier provenance

A scorecard attributes a response to the requested route, but the router
answers a request with whichever tier serves it. When the requested tier is
rate limited, the next tier in the chain answers instead, and the scorecard
would otherwise credit that response to the requested model. The harness
records the two response headers that expose this, so the scorecard shows
the contamination instead of hiding it.

Per run:

- `served_tier` is the `x-ccr-tier` header, the configured display name of
  the tier that served the request. It is `null` when the response carried
  no such header. On a terminal 429 it names the last tier attempted, which
  is not a tier that completed a response.
- `rate_limited_tiers` is the `x-ccr-rate-limited-tiers` header, split back
  into the full `provider,model` routes it names. CCR joins those routes
  with commas, so the header for two routes reads
  `zai,glm-5.4,minimax,MiniMax-M3.1-Flash`. The header is only set on a
  terminal 429, so an empty list means no terminal rate limit, not that no
  tier was rate limited during the request.

Per route, `summary.tier_provenance` holds:

- `requested_provider`, the provider component of the requested route.
- `runs_with_served_tier` and `runs_without_served_tier`, which split the
  attempted runs by whether they reported a served tier. A run without one
  is unknown provenance, not a fallback.
- `served_tier_mismatch_runs`, the runs whose served tier names a different
  provider than the requested route.
- `rate_limited_runs`, the runs that observed rate limited tiers.
- `contaminated_runs`, the runs that satisfy either condition. A run that
  was both rate limited and served by another tier counts once.
- `served_tiers` and `rate_limited_tier_names`, the counts per served tier
  and per rate limited route.
- `clean`, which is true only when no run was contaminated and every
  attempted run reported its served tier.

`x-ccr-tier` carries the tier display name, which is the provider name
unless the provider configures a custom `tier_name`. A provider with a
custom display name therefore reads as a different provider than its route
id names, and the recorded `served_tier` value is the field to read when
that happens.

The mismatch test compares providers, not models. A same-provider fallback,
for example `zai,glm-5.4` falling through to `zai,glm-5.3`, still reports the
same provider and is therefore not counted as contamination. The served-tier
fields detect cross-provider fallbacks; the listener log is the authority for
which model inside one provider actually answered.

OpenAI-protocol streaming responses do not carry `x-ccr-tier` in this router
build, so such a route reports unknown provenance and can never be marked
clean even when it served the request itself. Responses-protocol and
Anthropic-protocol routes do carry the header. This fails closed: no run is
credited with provenance it did not report.

A clean per-model comparison needs a single-tier listener, or a reading of
the served-tier fields. A multi-tier listener answers the requested model
only while that model serves the request, so accuracy measured across a
fallback window describes the chain that served it, not the requested model.

Tier provenance does not change the exit code. A contaminated run still
passes or fails its checker on its own terms; the contamination is reported
in the scorecard and in the `tier provenance` section of the run summary.

Exit codes: `0` when every attempted run passed, `1` when at least one
attempted run failed a checker or errored, `2` for a usage error such as a
missing route.

### What these numbers do not prove

- The suite is five fixed prompts at one moment in time. It does not measure
  general model quality, reasoning depth, or long-horizon agentic behavior.
- A score is attributed to the requested route only when the response's
  served tier names the same provider. A run served by a fallback tier is
  counted as contaminated, and its score does not describe the requested
  model.
- Latency and throughput describe this machine, this listener, and this
  network path. They are not provider-intrinsic numbers.
- A skipped scenario is not a failed scenario, and a route with more skipped
  scenarios has a narrower accuracy denominator than a route with none.
- Token counts use upstream-reported usage when the route reports it and a
  character-based estimate otherwise, so estimated tokens make tokens per
  second approximate.
- The code-fix checker executes model-generated Python in a local subprocess
  with a timeout. That is not a security sandbox. Use `--no-exec` to skip it.
- The vision scenario is skipped unless the route is declared vision-capable.
  An undeclared route is never sent an image.
- No score here proves which route is better for a real workflow. It only
  compares responses to these fixed checks.

### Offline gate

The harness has no network dependency in its tests. Every checker, the
aggregation logic, and the dry-run CLI are exercised offline:

```bash
python3 -m unittest discover -s scripts -p "test_ccr_model_benchmark.py"
```

## Terminal Dashboard (TUI)

CCR-Rust includes an interactive dashboard for real-time monitoring:

```bash
ccr-rust dashboard                                # localhost:3456
ccr-rust dashboard --host 10.0.0.5 --port 3456    # remote server
CCR_DASHBOARD_HOST=10.0.0.5 CCR_DASHBOARD_PORT=3456 ccr-rust dashboard
```

The dashboard reads `CCR_DASHBOARD_HOST` and `CCR_DASHBOARD_PORT` environment variables,
falling back to `127.0.0.1:3456` when unset.

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

CCR-Rust estimates token counts _before_ dispatching requests (using tiktoken's `cl100k_base`) and compares against upstream-reported usage.

### Why This Matters

If your local token estimates are off, you might:

- Route to the wrong tier (e.g., `longContext` threshold not triggered)
- Exceed provider limits unexpectedly
- Underestimate costs

### Checking Drift

The `/v1/token-drift` endpoint returns:

```json
[
  {
    "tier": "tier-0",
    "samples": 150,
    "cumulative_drift_pct": 2.3,
    "last_drift_pct": 1.8
  }
]
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

Tiers are automatically reordered by observed latency (EWMA). If Tier 2 is consistently faster than Tier 1, it gets promoted. With `Router.strictTierOrder: true`, tiers instead serve strictly in configured order and a later tier is only tried when every earlier tier fails.

Tiers with fewer than 3 samples keep their configured priority—no premature reordering.

### Adaptive Backoff

Retry delays are scaled by the tier's EWMA latency:

- Fast tiers get shorter backoffs (retry quickly)
- Degraded tiers back off longer (avoid pile-on)

This prevents cascading failures when a tier is temporarily overloaded.
