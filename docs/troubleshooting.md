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

**Frequent 429 errors**
- Check `/v1/latencies` for affected tiers
- Monitor `ccr_rate_limit_hits_total` metric
- Increase `base_backoff_ms` in config

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

## Debugging

```bash
# Enable debug logging
RUST_LOG=ccr_rust=debug,tower_http=debug ./ccr-rust

# Check metrics
curl localhost:3456/metrics | grep ccr_

# Test health
curl localhost:3456/health
```
