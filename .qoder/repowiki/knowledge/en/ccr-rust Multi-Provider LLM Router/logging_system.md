## Approach

The application uses **`tracing` (0.1)** as its structured logging framework, configured at process start in `src/main.rs`. Log output goes to stderr via `tracing_subscriber::fmt::layer`, filtered by `EnvFilter` read from the `RUST_LOG` environment variable (with a fallback default of `ccr_rust=info,tower_http=info`). An optional OpenTelemetry exporter layer (`tracing-opentelemetry`) is added behind the `telemetry` Cargo feature and only emits spans whose target is `ccr_telemetry`, routed to a loopback HTTP endpoint (`CCR_OTEL_ENDPOINT`, validated to be `http://127.0.0.1:PORT/v1/traces`).

The crate also declares `log = "0.4"` and `env_logger = "0.11"` as dependencies, but the application code does not use them — all runtime log emission goes through `tracing::info!` / `debug!` / `warn!` / `trace!` macros.

## Key Files

- `src/main.rs` — CLI entry point; installs `tracing_subscriber` registry, `fmt` layer with `EnvFilter`, and conditional OTel layer.
- `src/telemetry.rs` — Optional bounded OpenTelemetry provider construction, validation that the collector is local loopback `/v1/traces`, and an Axum middleware that wraps each HTTP request in a span under target `ccr_telemetry`.
- `src/telemetry/runtime.rs` — `initialize_provider` / `finish_command` helpers used from `main`.
- Scattered callers across `src/router/`, `src/mcp/`, `src/config/`, `src/ratelimit.rs`, `src/sse.rs`, `src/schema_validate.rs`, etc., all using `tracing::info!/debug!/warn!/trace!`.

## Architecture and Conventions

### Subscriber setup (src/main.rs)

```rust
let subscriber = tracing_subscriber::registry().with(
    tracing_subscriber::fmt::layer().with_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "ccr_rust=info,tower_http=info".into()),
    ),
);
```

- The `fmt` layer is always installed; its filter is driven by `RUST_LOG` when present, otherwise defaults to `info` for `ccr_rust` and `tower_http`.
- When the `telemetry` feature is enabled and the command is `Start` (or no subcommand), an additional `tracing_opentelemetry::layer()` is attached. It is further constrained by a `filter_fn` that selects only metadata whose `target() == "ccr_telemetry"`, so normal `tracing::info!` calls do NOT leak into OTel.
- The OTel tracer is named `"ccr-rust"`; location and thread info are disabled (`with_location(false)`, `with_threads(false)`).

### Telemetry privacy constraints (src/telemetry.rs)

- `valid_endpoint` enforces that the collector URL must use scheme `http`, resolve to `127.0.0.1` or `[::1]`, contain no username/password/query/fragment, and have path exactly `/v1/traces`.
- Request spans record only `otel.kind`, `otel.status_code`, `http.request.method`, `http.route`, `http.response.status_code`, and `stream.completed`. A comment explicitly states: *Never record URI/query, headers, bodies, errors, provider responses, or paths.*
- The batch processor is bounded: queue size 1024, export batch size 128, scheduled delay 2s, client/exporter timeout 2s.

### Log levels observed in code

| Level | Typical use | Examples |
|-------|-------------|----------|
| `info` | Startup, listener bind, tool enablement, signal receipt | `Loaded config from …`, `Tier order: …`, `mcp-daemon listening on {addr}`, `Received SIGINT/SIGTERM` |
| `debug` | Per-request routing decisions, rate-limit state, upstream disconnects | `Skipping deterministically rejected tier`, `Client disconnected, aborting upstream` |
| `warn` | Non-fatal misconfiguration, missing transformers, failed init | `transformer not found, skipping`, `Failed to initialize debug capture` |
| `trace` | Rare low-volume events | `killing MCP backend child process` |

### Structured fields convention

Fields are passed as key-value pairs in macro invocations rather than positional format args:

- `tier = %tier_name` (display formatter for `String`)
- `order = ?strict_order.iter().map(...).collect::<Vec<_>>()` (debug formatter for collections)
- `name = entry.name()`
- `persist: {:?}` (manual debug formatting inside the message string)

This follows standard `tracing` field syntax. No custom `tracing::Value` types are introduced.

## Conventions and Constraints

1. **All application logging goes through `tracing`** — direct use of `log!` or `println!` for operational logging is not observed in production code; `eprintln!` is reserved for telemetry bootstrap failures in `src/telemetry.rs`.
2. **Log verbosity is controlled at startup via `RUST_LOG`** — there is no runtime flag to change it. The default filter is `ccr_rust=info,tower_http=info`.
3. **OpenTelemetry spans are opt-in via the `telemetry` Cargo feature** and gated on both the feature flag and the `Start` command path; non-start commands never install the OTel layer.
4. **OTel export is restricted to local loopback HTTP `/v1/traces`** — `valid_endpoint` rejects remote hosts, credentials, query strings, fragments, wrong schemes, and wrong paths. This is enforced by runtime validation before the provider is constructed.
5. **Spans emitted to OTel use target `ccr_telemetry`** — a separate `filter_fn` ensures only those spans reach the OTel layer, preventing normal logs from being exported.
6. **No sensitive data is recorded in spans** — the `request_span` function deliberately omits URI, query, headers, bodies, error messages, provider responses, and file paths.
7. **Structured fields prefer typed keys** — routing and config logs pass named fields (`tier`, `order`, `name`, `persist`) rather than embedding values in format strings.

## Applicable Scope

This pattern applies to the Rust source tree under `src/` and the integration tests under `tests/` (which also install a `tracing_subscriber` in `tests/telemetry_privacy.rs`). It does not apply to Python scripts under `scripts/`, `benchmarks/`, or `monitoring/`.