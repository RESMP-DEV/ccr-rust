# Optional OpenTelemetry export

CCR keeps its local logs and Prometheus endpoint when telemetry is disabled.
Build with `--features telemetry` (or `--all-features`) to include the exporter.
Default builds omit its dependencies. Only server commands initialize it.
To send minimal request spans to a local OpenTelemetry Collector, set:

```sh
CCR_OTEL_ENDPOINT=http://127.0.0.1:4318/v1/traces ccr-rust start
```

Only literal loopback HTTP endpoints ending in `/v1/traces` are accepted.
The exporter disables redirects and proxy discovery to preserve this boundary.
Configure the Collector's Azure Monitor exporter with the Application Insights
connection string; CCR does not need Azure credentials. The Collector can also
scrape `http://127.0.0.1:3456/metrics` for existing request, token, cache,
first-token, throughput, rate-limit, and estimated-cost metrics.

Exported request spans contain method (GET/POST/OPTIONS/OTHER), the matched route
template, HTTP status, and whether the body reached end of stream. They exclude
raw URLs and query strings, path parameters, headers, request/response bodies,
error strings, tool outputs, and source-file locations. Request spans last until
the response body completes or is dropped; cancelled bodies remain incomplete.
HTTP 4xx/5xx and body errors mark the span as an error. Both end-of-stream polls
and final-frame size hints record completion. Trace completion measures body production, not receipt by
the remote client.

Only the `ccr_telemetry` tracing target reaches the exporter. Local logging's
`RUST_LOG` filter remains independent. No incoming trace context is trusted or
propagated in this initial integration.

The SDK queues at most 1,024 spans, exports batches of at most 128 on its own
worker, and applies a two-second HTTP timeout. Export failures can lose telemetry;
they do not delay request handling. Normal CLI shutdown attempts a bounded
three-second flush. An invalid endpoint or exporter initialization failure
disables export and reports a fixed diagnostic without displaying configuration.

Apply tail sampling at the Collector: retain error traces within bounded memory
and queue limits, and sample 10% of successful traces. Keep Prometheus counters
unsampled. Use a workspace ingestion cap and alerts as secondary cost controls;
daily caps can overshoot. Do not forward unrestricted application logs.

Validation includes response preservation, secret sentinels in headers, bodies,
path parameters and query strings, error status, streaming, cancellation and
disabled-export routing. The Azure destination and Collector require separate
deployment and live verification.

References:
- https://learn.microsoft.com/azure/developer/rust/sdk/logging
- https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/v0.161.0/exporter/azuremonitorexporter
