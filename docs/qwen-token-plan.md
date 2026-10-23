# Qwen Token Plan integration

CCR-Rust can route Anthropic- and OpenAI-compatible clients through Alibaba
Cloud Model Studio's Qwen Token Plan. This guide is the copy-paste provider
shape used by the live 2026-10-18 qualification, including the endpoint
quirk, explicit model IDs, thinking budget, and the off-peak timezone table.

## Provider configuration

The plan key is an `sk-sp-` key, not a standard DashScope key. Put the
placeholder in your environment or the credentials file consumed by your CCR
installation:

```bash
export CCR_QWEN_API_KEY=sk-sp-your-token-plan-key
```

Add the provider to `~/.claude-code-router/config.json`. The final `/v1` is
required on the Anthropic-compatible base URL:

```json
{
  "Providers": [
    {
      "name": "qwen",
      "api_base_url": "https://token-plan.maas.qwencloudapi.com/apps/anthropic/v1",
      "api_key": "${CCR_QWEN_API_KEY}",
      "protocol": "anthropic",
      "models": [
        "qwen3.8-flash",
        "qwen3.8-max",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.6-flash",
        "deepseek-v4.1-flash-1017",
        "deepseek-v4.1-flash",
        "deepseek-v4-pro",
        "glm-5.3",
        "glm-5.2",
        "auto"
      ],
      "transformer": {
        "use": [["maxtoken", {"max_tokens": 32768, "only_if_missing": true}]]
      },
      "tier_name": "ccr-qwen"
    }
  ],
  "Router": {
    "default": "qwen,qwen3.8-flash",
    "tiers": ["qwen,qwen3.8-flash"]
  }
}
```

For a volume-worker chain, put Qwen after an entitled Flash tier and before
the final fallback. The governed workstation uses:

```text
zai,glm-5.3-flash
qwen,qwen3.8-flash
minimax,MiniMax-M3.1-Flash
qwen,deepseek-v4.1-flash-1017
qwen,deepseek-v4.1-flash
```

GLM-5.3 remains the main/auth primary but is deliberately absent from this
worker chain: worker volume should not consume the judgment seat.
Main/auth additionally place `qwen,qwen3.8-max` immediately after GLM-5.3 and
before the Flash/Qwen volume fallbacks.

Qwen serves `deepseek-v4.1-flash-1017` and `deepseek-v4.1-flash` as distinct
IDs: each request echoes the exact ID named. The dated `1017` ID is Qwen's
published current iteration, so CCR tries it first; the undated ID remains an
explicit final fallback rather than being silently rewritten.

## Verification

Reload the configuration with your normal idle-gated restart procedure, then
use a supported interactive coding or agent tool through CCR. For example,
start Codex on the explicit Qwen route and send `Reply with QWEN_OK.`:

```bash
codex-ccr --model qwen,qwen3.8-flash
```

The current flash models think by default; keep any client output cap at 512
tokens or above unless thinking is explicitly disabled.
CCR's Qwen provider injects `max_tokens: 32768` only when a client omits the
field. Qwen rejects an Anthropic request without an output limit even when
some Anthropic-compatible providers tolerate the omission; Codex is the known
affected client.

Confirm the tool round trip completed and inspect the router log or metrics
to prove the `qwen` tier served it rather than a fallback. Repeat with
`qwen,deepseek-v4.1-flash-1017` or `qwen,deepseek-v4.1-flash` if those lanes
are configured. Development-only protocol diagnostics with standalone HTTP
clients must use a PAYG credential,
not a personal Token Plan `sk-sp-` key.

## Timezone and lane rules

Use the billing authority's local clock as the source of truth. Do not store
a fixed Pacific-Time rule: the UTC-to-Pacific offset changes with daylight
saving time.

| Billing lane | Authoritative window | Pacific-Time equivalent on 2026-10-18 |
|---|---|---|
| Qwen night discount | daily 22:00-08:00 in `Asia/Shanghai` | 07:00-17:00 in `America/Los_Angeles` (PDT) |
| Qwen day rate | daily 08:00-22:00 in `Asia/Shanghai` | 17:00-07:00 in `America/Los_Angeles` (PDT) |
| Native DeepSeek peak | 01:00-04:00 and 06:00-10:00 UTC, Monday-Friday UTC, excluding Chinese public holidays | the previous Pacific calendar evening, 18:00-21:00 and 23:00-03:00 PDT |
| Native DeepSeek off-peak | every other hour, plus Saturday and Sunday UTC and Chinese holidays | varies by date; convert from UTC, not from a fixed PT hour |

The weekday anchor matters: a Monday 01:00-04:00 UTC DeepSeek peak appears on
Sunday evening in Pacific Time. After the US daylight-saving transition, the
same UTC boundaries land one Pacific hour earlier.

CCR does not silently rewrite routes at a wall-clock boundary. Pin the route
explicitly, use a preset, or make a reviewed configuration change at the
boundary. This keeps every served model auditable.

## Off-peak decision table

The following is the operational baseline from the 2026-10-18 lane
measurement; console credit coefficients were still pending at commit time,
so treat the exact plan coefficient as unmeasured until the corresponding
console reads are entered.

| Workload | Winning lane |
|---|---|
| Hot, cache-heavy DeepSeek session with about 92% cache hit | Native DeepSeek off-peak when available; its $0.003-0.006/M cache-hit input remains the strongest cache lane. |
| Interactive coding or agent session with cold, low-cache long context | Prefer Qwen plan `deepseek-v4.1-flash-1017`, the dated current checkpoint; the undated ID remains an explicit fallback. Both are listed at $0.08/$0.32 per M day and $0.04/$0.16 night, better than native off-peak $0.15/$0.60 on uncached tokens. |
| Broad interactive Qwen flash use | Qwen plan `qwen3.8-flash`, especially in the night window; listed blended credit cost is about $0.047/M day and $0.019/M night. |
| Judgment prompt | Choose the model first and preserve its quota; price is secondary. Keep GLM-5.3 out of worker fallback chains. |

The 2026-10-18/19 noninteractive wire measurement passed both Qwen routes
through CCR in both windows: 306 night and 96 day responses for
`qwen3.8-flash`, and 198 night and 96 day responses for
`deepseek-v4.1-flash`; every response was HTTP 200 with the expected served
model and usage fields. It characterizes the endpoint and token accounting,
but is not a personal-plan terms-compliance example or supported-tool
acceptance run. The raw logs and token ledger live outside the repository in
the operator's private measurement directory; the source receipt is
`evidence/qwen-token-plan-20261019.md`.

## Troubleshooting

- **404 on direct upstream probes:** confirm the base URL ends in `/v1`. The
  token-plan Anthropic endpoint rejects the suffix-less path.
- **Authentication failure:** an `sk-` DashScope key is not an `sk-sp-`
  Token Plan key. Use the key issued for the token-plan console.
- **Very short or empty answers:** raise `max_tokens` to at least 512 or
  disable thinking where the client/model supports it.
- **Unexpected fallback:** inspect the response model and per-tier metrics.
  A provider/model pin still follows the configured retry policy; it does not
  isolate an account by itself.
