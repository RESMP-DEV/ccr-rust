# Exposing the CCR-Rust API

CCR-Rust can serve ordinary OpenAI- and Anthropic-compatible clients without
requiring them to understand CCR's provider routing. Keep the router bound to
loopback and put a Cloudflare Tunnel (or another authenticated HTTPS reverse
proxy) in front of it. Configure a client API key first: the router's upstream
credentials must never be shared with client applications.

## Client authentication

Add an environment-backed client key to the router configuration:

```json
{
  "CLIENT_API_KEY": "${CCR_CLIENT_API_KEY}"
}
```

Then export `CCR_CLIENT_API_KEY` through the service launcher. Applications
present it in either standard form:

```text
Authorization: Bearer $CCR_CLIENT_API_KEY
x-api-key: $CCR_CLIENT_API_KEY
```

The first form is standard for OpenAI-compatible clients. The second supports
native Anthropic clients. Both protect all API, preset, metrics, usage, and
observability routes. `/health` remains unauthenticated so service managers can
perform liveness checks; it exposes only `ok`. If `CLIENT_API_KEY` is omitted,
the listener retains its historical unauthenticated behavior and should remain
loopback-only.

Client applications can use these base URLs:

| Client family | Base URL |
| ------------- | -------- |
| OpenAI Chat Completions | `https://<public-host>/v1` |
| Anthropic Messages | `https://<public-host>` |
| OpenAI Responses | `https://<public-host>/v1` |

The model name is the routing identity (`zai,glm-5.3`, for example) or a
configured bare model alias. CCR still selects and protects the upstream
provider credential.

## Cloudflare Tunnel ingress

A public ingress should forward only the API namespace, not the dashboard,
Prometheus endpoint, or broad router surface:

```yaml
tunnel: m4-ccr-api
credentials-file: /path/to/m4-ccr-api.json
ingress:
  - hostname: ccr-rust.example.com
    path: ^/v1/.*$
    service: http://127.0.0.1:3456
  - service: http_status:404
```

After `cloudflared tunnel route dns m4-ccr-api ccr-rust.example.com`, verify all
three failure paths before sharing the hostname:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' https://ccr-rust.example.com/v1/models
curl -sS -H "Authorization: Bearer $CCR_CLIENT_API_KEY" \
  https://ccr-rust.example.com/v1/models
curl -sS https://ccr-rust.example.com/metrics
```

The expected results are `401`, a model catalog, and `404` (because metrics were
not forwarded). API-key authentication remains necessary at the edge; the tunnel
is transport, not authorization.
