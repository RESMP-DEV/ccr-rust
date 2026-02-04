# Monitoring ccr-rust

The Rust implementation of CCR provides high-performance Prometheus metrics exposed on the `/metrics` endpoint.

## Key Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `router_requests_total` | Counter | Total number of requests processed, partitioned by `model` and `tier`. |
| `router_request_duration_seconds` | Histogram | Latency distribution of requests. |
| `router_retries_total` | Counter | Number of retries attempted per request. |
| `router_errors_total` | Counter | Total number of terminal failures. |
| `sse_streaming_active` | Gauge | Number of concurrent active SSE streams. |

## Dashboard Setup

1. **Prometheus**: Add the snippet in `prometheus-snippet.yml` to your `prometheus.yml`.
2. **Grafana**: Import the provided `dashboard.json` (planned) to visualize tiers and latency.

## Tracking Fallbacks

The fallback logic is exposed via labels. You can see how often Tier 1 (CCR-GLM) fails and triggers Tier 2 (CCR-DS) by querying:

```promql
sum(rate(router_requests_total{tier="2"}[5m])) / sum(rate(router_requests_total{tier="1"}[5m]))
```
