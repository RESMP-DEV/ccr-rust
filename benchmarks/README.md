# CCR-Rust SSE Stress Tests

Stress test suite for validating ccr-rust under 100+ concurrent SSE streams.

## Architecture

```
stress_sse_streams.py ──(100 streams)──> ccr-rust:3456 ──(proxy)──> mock_sse_backend.py:9999
```

The mock backend emits configurable SSE chunks with tunable delay, so tests are
fully self-contained with no real API keys or token costs.

## Quick Start

```bash
cd contrib/ccr-rust

# Build ccr-rust first
cargo build --release

# Run the full orchestrated test (starts mock + ccr-rust + stress test)
./benchmarks/run_stress_test.sh
```

## Manual Setup

If you want to run components separately (useful for debugging):

```bash
# Terminal 1: Mock SSE backend
uv run python benchmarks/mock_sse_backend.py --port 9999 --chunks 20 --delay-ms 50

# Terminal 2: ccr-rust pointing at mock
./target/release/ccr-rust --config benchmarks/config_mock.json --port 3456

# Terminal 3: Stress test
uv run python benchmarks/stress_sse_streams.py --streams 100
```

## Options

### run_stress_test.sh

| Flag | Default | Description |
|------|---------|-------------|
| `--streams` | 100 | Concurrent SSE streams |
| `--chunks` | 20 | SSE chunks per stream from mock |
| `--delay-ms` | 50 | Delay between chunks (ms) |
| `--ramp-ms` | 0 | Spread launches over this window |
| `--mock-port` | 9999 | Mock backend port |
| `--ccr-port` | 3456 | CCR proxy port |
| `--json-out` | | Write detailed JSON results to file |

### stress_sse_streams.py

| Flag | Default | Description |
|------|---------|-------------|
| `--ccr-url` | http://127.0.0.1:3456 | CCR proxy URL |
| `--streams` | 100 | Concurrent streams |
| `--ramp-ms` | 0 | Ramp-up window |
| `--timeout` | 120 | Per-stream timeout (seconds) |
| `--model` | mock,mock-model | Model string in request |
| `--json-out` | | JSON output file |

### mock_sse_backend.py

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | 9999 | Listen port |
| `--chunks` | 20 | Chunks per response |
| `--delay-ms` | 50 | Inter-chunk delay (ms) |

## Metrics Collected

Per-stream:
- Time-to-first-byte (TTFB)
- Total duration
- Chunks received
- Bytes received
- Error classification

Aggregate:
- p50/p95/p99/max for TTFB and duration
- Total throughput (Mbps)
- Peak concurrent streams (local and ccr gauge)
- CCR usage stats (requests, failures, active streams)

## Example Output

```
========================================================================
  CCR-RUST SSE STRESS TEST REPORT
========================================================================

  Streams:  100
  Success:  100  |  Failed: 0
  Wall time: 1.23s
  Peak local concurrency: 100
  Peak ccr active_streams: 100

  Time-to-First-Byte (TTFB):
    p50:        12.3 ms
    p95:        45.6 ms
    p99:        67.8 ms
    max:        89.0 ms

  Stream Duration:
    p50:      1050.2 ms
    p95:      1120.5 ms
    p99:      1180.3 ms
    max:      1200.1 ms

  Throughput:
    Total bytes:       234,000
    Total chunks:        2,000
    Aggregate:         1.52 Mbps

========================================================================
  RESULT: PASS - all streams completed successfully
========================================================================
```

## Dependencies

Python (installed via uv):
- `aiohttp` - async HTTP client for concurrent streams
- `uvicorn` - ASGI server for mock backend
- `starlette` - ASGI framework for mock backend

These are not added to the project's main dependencies. Install with:

```bash
uv pip install aiohttp uvicorn starlette
```

## Per-Model Benchmark Harness

This directory also holds the reference point for the per-model benchmark
harness, which compares provider routes on a fixed, verifiable task suite
instead of on load behavior. The harness itself lives in `scripts/` because it
is an operator tool with an offline unit-test gate, not a load generator.

```bash
# Parse the suite and print the plan without contacting the router
python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --dry-run

# Run the suite against one route, three repetitions per scenario
python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --reps 3

# Compare two routes and keep machine-readable scorecards
python3 scripts/ccr_model_benchmark.py \
  --models zai,glm-5.4 \
  --models zai,glm-5.3-flash
```

Scorecards are written to `build/reports/model-benchmark/<slug>.json`, one per
route. Correctness and speed are recorded separately: a run passes only when a
checker accepts the whole response, and latency, token throughput, and
stability sit beside that verdict instead of replacing it.

Each run also records the tier that served it, taken from the `x-ccr-tier` and
`x-ccr-rate-limited-tiers` response headers, and each route's scorecard
counts the runs that a fallback tier answered instead of the requested
provider. A clean per-model comparison should use a single-tier listener or
read those served-tier fields, because a multi-tier listener answers the
requested model only while that model serves the request.

The difference from the SSE stress suite above is the question being asked.
The stress suite asks whether the router stays healthy under concurrent
streams; the benchmark asks whether one route answers a known contract
correctly and repeatably. The vision scenario is skipped with an explicit
reason for routes that are not declared vision-capable, and the code-fix
scenario is skipped with `--no-exec` rather than executing returned snippets.

Offline gate:

```bash
python3 -m unittest discover -s scripts -p "test_ccr_model_benchmark.py"
```

Full documentation, including the metric definitions and the explicit
non-claims, is in
[docs/observability.md](../docs/observability.md#per-model-benchmark-harness).
