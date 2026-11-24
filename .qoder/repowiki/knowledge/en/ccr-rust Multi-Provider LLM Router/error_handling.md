# Error Handling in ccr-rust

## Approach Overview

The codebase uses a layered Rust error model:

1. **`anyhow::Result`** is the default return type for library functions (transformers, config loading, MCP tools, worker utilities). It carries opaque `anyhow::Error` values that preserve backtraces but do not expose variant structure to callers.
2. **Domain-specific error enums** are introduced at boundaries where callers need to distinguish failure modes — most notably `TryRequestError` in `src/router/types.rs`, which separates rate-limits, deterministic rejections, and other errors.
3. **No `panic!`/`catch_unwind`** infrastructure exists in application code; panics appear only in test assertions and internal assumptions (e.g. `expect("expected deterministic rejection")`).
4. **Axum handlers** return `impl IntoResponse`; request-path errors surface as `TryRequestError` through `dispatch.rs` rather than via Axum error middleware or custom layers.

## Key Files and Types

- `src/router/types.rs` — defines `TryRequestError` enum with three variants (`RateLimited(Option<Duration>)`, `Rejected(u16, anyhow::Error)`, `Other(anyhow::Error)`), plus `Display`/`Error` impls and `From<anyhow::Error>`.
- `src/router/dispatch.rs` — central request dispatcher; converts provider HTTP failures into `TryRequestError` via `provider_upstream_error()` and classifies 400/401–404/422 as `Rejected` unless they are context-window failures.
- `src/transformer/mod.rs` — defines the `Transformer` trait whose methods return `anyhow::Result<Value>`; every transformer implementation propagates errors this way.
- `src/main.rs` — entry point; wiring uses `anyhow::Result` throughout and does not install a global panic hook.

## Architecture and Conventions

### Request dispatch classification

`provider_upstream_error()` in `src/router/dispatch.rs` is the single place where upstream HTTP status codes are mapped to `TryRequestError`. The rule is explicit in its comment and match arm:

```
match status.as_u16() {
    400 | 401..=404 | 422 if !is_context_window_rejection(body) => {
        TryRequestError::Rejected(status.as_u16(), error)
    }
    _ => TryRequestError::Other(error),
}
```

This means 4xx errors from providers are treated as *deterministic* failures of the current request (auth/billing/permission/unknown-model/validation) — retrying the same tier cannot succeed and may trip cross-model account rate limits. Context-window rejections (400 with specific body) fall through to `Other` so the retry budget can try a larger-context tier. A 401 response also gets a credential-placeholder hint appended when the configured `api_key` still contains an unexpanded env placeholder.

### Rate limiting propagation

`TryRequestError::RateLimited` wraps an optional `Retry-After` duration extracted from either the `Retry-After` header or the `retry_after` / `reset_at` fields of an embedded JSON error body (see `src/router/dispatch.rs` around lines 780–906). Callers use this to record per-tier backoff via `record_rate_limit_backoff` rather than relying on raw HTTP 429 handling.

### Transformer pipeline errors

Every transformer's `transform_request`/`transform_response` returns `anyhow::Result<Value>`. Errors bubble up through the chain without being converted to domain types until they reach the dispatch layer. This keeps transformers simple while letting the caller decide whether a transformation error is retriable (it is wrapped as `TryRequestError::Other`).

### No middleware-level error mapper

There is no Axum `Layer` that maps `TryRequestError` to HTTP responses. Handlers call `try_request` and then convert the result to an Axum `Response` inline (status code, body, headers). Test code in `dispatch.rs` asserts on the exact `TryRequestError` variants rather than checking HTTP status alone.

### Panics

Panics are used sparingly and only in tests or obvious invariant checks:
- `src/router/dispatch.rs` line ~1775: `panic!("expected deterministic rejection")` in a unit test asserting `TryRequestError::Rejected`.
- Signal handler setup: `.expect("failed to listen for ctrl+c")`.
- Many `unwrap()` calls in test modules under `tests/`, `src/router/responses_api/*_tests.rs`, and integration tests.

Application code paths generally propagate `Result` upward; there is no `catch_unwind` usage.

## Conventions Observed

- **Use `anyhow::Result`** for library/internal functions where callers should not enumerate error causes (transformers, config parsing, MCP tool implementations).
- **Introduce a typed enum** (`TryRequestError`) only at API boundaries where the caller needs to branch on failure semantics (rate-limit vs. deterministic rejection vs. transient).
- **Classify provider 4xx as non-retriable** by default; context-window 400 is the documented exception.
- **Wrap all non-domain errors** in `TryRequestError::Other(anyhow::anyhow!(...))` so `Display` output remains human-readable while preserving the original cause.
- **Extract retry guidance** (duration, header hints) inside the error value rather than returning it separately.
- **Avoid panics in production paths**; reserve them for test assertions and process-initialization helpers.

## Constraints Enforced by Code

- Deterministic provider rejections (400/401/402/403/404/422) are classified as `TryRequestError::Rejected` by `provider_upstream_error()`; callers can branch on this variant to avoid unnecessary retries.
- `TryRequestError` implements `std::error::Error` and `From<anyhow::Error>`, so `?` propagation from transformer results automatically wraps into `Other`.
- The `credential_placeholder_hint` function gates a diagnostic suffix on whether any configured credential still contains an unexpanded `${VAR}` placeholder, making misconfigured env vars surfaced to operators instead of generic "unauthorized" messages.