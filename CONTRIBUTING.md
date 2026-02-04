# Contributing to CCR-Rust

Thanks for your interest in contributing! CCR-Rust is a high-throughput LLM proxy router, and we welcome improvements that make it faster, more reliable, or easier to use.

## Quick Start

```bash
# Clone and build
git clone https://github.com/RESMP-DEV/ccr-rust.git
cd ccr-rust
cargo build

# Run tests
cargo test

# Run with a config
cargo run -- --config path/to/config.json
```

## Development Setup

### Prerequisites

- **Rust 1.75+** (2024 edition features)
- **Python 3.10+** with `uv` (for stress tests)
- Optional: `wiremock` knowledge for integration tests

### Building

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Check without building
cargo check

# Run clippy lints
cargo clippy
```

### Testing

```bash
# Run all tests
cargo test

# Run specific test module
cargo test transformer::tests
cargo test routing::tests

# Run with output
cargo test -- --nocapture

# Run integration tests only
cargo test --test test_routing
```

### Stress Testing

See `benchmarks/README.md` for the stress test suite:

```bash
# Full orchestrated test
./benchmarks/run_stress_test.sh --streams 100 --chunks 20

# Manual testing
uv run python benchmarks/mock_sse_backend.py --port 9999 &
cargo run --release -- --config benchmarks/config_mock.json &
uv run python benchmarks/stress_sse_streams.py --streams 100
```

## Code Organization

```
src/
├── main.rs          # CLI and server setup
├── lib.rs           # Module exports
├── config.rs        # Config parsing, provider resolution
├── router.rs        # Request handling, format translation
├── routing.rs       # EWMA tracking, tier reordering
├── sse.rs           # SSE streaming, usage extraction
├── metrics.rs       # Prometheus metrics, token tracking
└── transformer.rs   # Request/response transformers
```

### Key Types

| Type | Purpose |
|------|---------|
| `AppState` | Shared state (config, EWMA tracker, transformer registry) |
| `EwmaTracker` | Per-tier latency tracking |
| `TransformerChain` | Composable request/response transformations |
| `StreamVerifyCtx` | Context for token drift verification |

## Adding Features

### Adding a New Transformer

1. Create the transformer struct in `src/transformer.rs`:

```rust
#[derive(Debug, Clone)]
pub struct MyTransformer {
    // config fields
}

impl Transformer for MyTransformer {
    fn name(&self) -> &str {
        "mytransformer"
    }

    fn transform_request(&self, request: Value) -> Result<Value> {
        // modify request
        Ok(request)
    }

    fn transform_response(&self, response: Value) -> Result<Value> {
        // modify response
        Ok(response)
    }
}
```

2. Register in `TransformerRegistry::new()`:

```rust
registry.register("mytransformer", Arc::new(MyTransformer::default()));
```

3. Add tests:

```rust
#[test]
fn mytransformer_does_thing() {
    let transformer = MyTransformer::default();
    let input = serde_json::json!({...});
    let output = transformer.transform_request(input).unwrap();
    assert_eq!(output["field"], expected);
}
```

### Adding a New Metric

1. Define the metric in `src/metrics.rs`:

```rust
lazy_static! {
    static ref MY_COUNTER: IntCounterVec = register_int_counter_vec!(
        "ccr_my_metric_total",
        "Description of what this counts",
        &["tier"]
    ).unwrap();
}
```

2. Create a helper function:

```rust
pub fn record_my_metric(tier: &str) {
    MY_COUNTER.with_label_values(&[tier]).inc();
}
```

3. Call from router/sse as needed.

### Adding a New Endpoint

1. Define the handler in `src/router.rs` or new module:

```rust
pub async fn my_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    // implementation
}
```

2. Register in `main.rs`:

```rust
.route("/v1/myendpoint", get(router::my_handler))
```

## Code Style

### Formatting

```bash
cargo fmt
```

### Lints

```bash
cargo clippy -- -D warnings
```

### Conventions

- Use `tracing` for logging, not `println!` or `log`
- Prefer `anyhow::Result` for error handling
- Keep functions under ~50 lines when practical
- Add doc comments for public APIs
- Use `#[cfg(test)]` for test modules

### Commit Messages

```
feat: add think-tag stripping transformer
fix: handle empty tool_calls array in OpenAI response
perf: reduce allocations in SSE parsing
docs: update transformer configuration examples
test: add integration test for rate limiting
```

## Pull Request Process

1. **Fork** the repo and create a feature branch
2. **Write tests** for new functionality
3. **Run the test suite** and fix any failures
4. **Update documentation** if adding user-facing features
5. **Open a PR** with a clear description

### PR Checklist

- [ ] `cargo test` passes
- [ ] `cargo clippy` has no warnings
- [ ] `cargo fmt` applied
- [ ] New features have tests
- [ ] README updated if needed

## Reporting Issues

When reporting bugs, please include:

1. **CCR-Rust version** (`cargo --version`, commit hash)
2. **Config snippet** (redact API keys)
3. **Steps to reproduce**
4. **Expected vs actual behavior**
5. **Error messages** or logs

For performance issues, include:

- Number of concurrent streams
- Request sizes (approximate)
- Memory usage (`/metrics` output)
- `htop` or similar resource monitoring

## Areas We'd Love Help With

- **Provider quirks**: DeepSeek, OpenRouter, and others have API differences
- **Edge cases**: Streaming interruptions, malformed JSON, etc.
- **Performance**: Profiling, reducing allocations, connection pooling
- **Documentation**: Examples, troubleshooting guides

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
