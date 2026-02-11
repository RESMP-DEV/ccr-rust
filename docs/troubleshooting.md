# Troubleshooting CCR-Rust

## Connection Issues

**"Connection refused"**
- Check provider URL in config
- Verify API endpoint is accessible
- Check firewall rules

**SSL/TLS errors**
- Ensure system CA certificates are installed
- Try with `RUST_LOG=reqwest=debug` for details

## Rate Limiting

### How CCR-Rust handles 429s

- A `429 Too Many Requests` marks that tier as rate-limited and immediately moves the current request to the next tier.
- Remaining retries for the same tier are skipped once a 429 is seen.
- Backoff is exponential and uses `Retry-After` when the provider returns it.
- Tiers can also be skipped proactively when headers indicate exhausted quota (`x-ratelimit-remaining: 0` with future `x-ratelimit-reset`).
- A successful request or completed stream clears backoff state for that tier.

### Detect rate-limit pressure

```bash
# Per-tier 429s and backoffs
curl -s localhost:3456/metrics | grep -E 'ccr_rate_limit_hits_total|ccr_rate_limit_backoffs_total'

# Failure reasons, including rate_limited
curl -s localhost:3456/metrics | grep ccr_failures_total

# Current tier latencies/ordering signal
curl -s localhost:3456/v1/latencies
```

```bash
# Log-level detection
RUST_LOG=ccr_rust=debug,tower_http=debug ccr-rust start --config ~/.claude-code-router/config.json
```

Look for these log lines:
- `Rate limited on ...`
- `Rate limited, backing off`
- `Skipping tier: backoff in effect`
- `Skipping tier: quota exhausted`

### Handle sustained rate limits

1. Add at least one additional provider tier so failover has capacity.
2. Put frequently rate-limited providers lower in the route order.
3. Verify provider account quota/limits and request higher limits if needed.
4. Keep `tierRetries` for transient non-429 failures; 429 behavior is handled separately by the rate-limit tracker.

## SSE Failure Events (`/v1/responses`)

For streaming Responses requests, CCR-Rust preserves SSE transport on terminal upstream failures and emits `response.failed`.

**Example 1: upstream returned JSON error body**

```text
event: response.failed
data: {"type":"response.failed","response":{"id":"resp_failed","object":"response","status":"failed","error":{"message":"{\"error\":{\"message\":\"upstream failed\"}}"}}}
```

**Example 2: upstream returned plain text body**

```text
event: response.failed
data: {"type":"response.failed","response":{"id":"resp_failed","object":"response","status":"failed","error":{"message":"upstream gateway timeout"}}}
```

Client handling rule:
- Treat `event: response.failed` as terminal.
- Parse `response.error.message` for provider details.

## Format Translation

**Tool calls not working**
- Ensure transformer chain includes `anthropic-to-openai`
- Check provider supports function calling

**Missing system prompt**
- Anthropic `system` field is auto-converted to OpenAI format
- Verify it's not being filtered by provider

## Performance

**High latency**
- Check `/v1/latencies` for slow tiers
- EWMA will auto-reorder, but may take 3+ requests
- Consider adjusting tier order in config

## Requests Bypassing Tier Order

**Symptom**: Lower-priority tiers (e.g., OpenRouter) receive all traffic while higher-priority tiers (e.g., GLM, Minimax) are never hit.

**Cause**: Clients like Codex CLI and Claude Code cache the `model` field from successful responses. If a request once fell back to `openrouter,openrouter/aurora-alpha`, subsequent requests will include that exact model string, causing CCR-Rust to prioritize that tier.

**Detection**:
```bash
tail -500 /tmp/ccr-rust.log | grep -E "Direct routing|moved to front"
```

If you see lines like:
```
Direct routing: openrouter,openrouter/aurora-alpha moved to front
```

This confirms the client is requesting a specific tier.

**Solution**: Enable `ignoreDirect` in your config:

```json
{
  "Router": {
    "ignoreDirect": true
  }
}
```

This forces all requests to start from tier 0, regardless of what model the client specifies.

**Restart required** after changing config:
```bash
ccr-rust stop && ccr-rust start
```

## Debugging

```bash
# Enable debug logging
RUST_LOG=ccr_rust=debug,tower_http=debug ./ccr-rust

# Check metrics
curl localhost:3456/metrics | grep ccr_

# Test health
curl localhost:3456/health
```
