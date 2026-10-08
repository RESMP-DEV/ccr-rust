# Z.AI GLM and MiniMax coding plans

Use [Switching coding plans](switching-plans.md) for opt-in client commands,
named profiles, presets, and fallback. Use [Authentication suites](auth-suites.md)
when personal and corporate credentials must stay separate.

## Z.AI: choose the plan key and transport

Obtain the key for the intended individual or team coding plan from the Z.AI
console. Z.AI's current Codex guide explicitly distinguishes Team Plan keys
from other Z.AI keys; choosing a team model name does not select team billing.
Check current pricing and entitlements in the provider console instead of
relying on a copied price/model table.

| Use | Base URL | Protocol |
| --- | --- | --- |
| Direct Claude/Anthropic SDK | `https://api.z.ai/api/anthropic` | Anthropic client appends `/v1/messages` |
| CCR native Anthropic provider | `https://api.z.ai/api/anthropic/v1` | `anthropic`; CCR appends `/messages` |
| OpenAI Chat Completions coding endpoint | `https://api.z.ai/api/coding/paas/v4` | `openai` |
| Direct Codex or CCR native Responses upstream | `https://api.z.ai/api/v1` | `responses` |

These bases are not interchangeable. In particular, an OpenAI-compatible Chat
Completions base is not automatically a Responses endpoint. Avoid accidentally
using a pay-as-you-go endpoint when you intend to spend coding-plan quota.

### Shared Claude/Codex configuration through CCR

The native Anthropic route was verified with both clients:

```json
{
  "Providers": [{
    "name": "zai",
    "api_base_url": "https://api.z.ai/api/anthropic/v1",
    "api_key": "${ZAI_API_KEY}",
    "protocol": "anthropic",
    "auth_header": "authorization",
    "models": ["glm-5.3"],
    "honor_ratelimit_headers": false
  }],
  "Router": {
    "default": "zai,glm-5.3",
    "tiers": ["zai,glm-5.3"]
  },
  "HOST": "127.0.0.1",
  "PORT": 3456
}
```

Load `ZAI_API_KEY` into the router process before starting. The workstation's
managed service uses its own `CCR_ZAI_API_KEY` placeholder and credential loader;
keep the placeholder and loaded variable name aligned.

`honor_ratelimit_headers: false` only changes handling of rate-limit header
hints. It does **not** ignore actual HTTP 429 responses or grant more quota.
CCR still tracks 429s and skips that tier while it is limited.

### Native Responses alternative for Codex

For a dedicated Codex route, use this provider entry instead of the Anthropic
entry above (retain the surrounding config and matching router routes):

```json
{
  "name": "zai",
  "api_base_url": "https://api.z.ai/api/v1",
  "api_key": "${ZAI_API_KEY}",
  "protocol": "responses",
  "models": ["glm-5.3"]
}
```

Do not attach request transformers to a native Responses passthrough provider;
CCR rejects that combination to avoid dropping native Responses fields. Codex
still points at CCR's `http://127.0.0.1:3456/v1` using `wire_api = "responses"`.
The upstream transport is selected separately in this provider config.

A **direct** Codex connection can use the same Z.AI Responses base in an opt-in
Codex provider profile, with `env_key = "ZAI_API_KEY"` rather than a literal
secret in TOML. In that case the client process needs the real key and uses the
bare upstream model `glm-5.3`. Follow the installed client's profile schema and
Z.AI's model-catalog instructions; preserve normal Codex settings. The live test
below exercised the endpoint through CCR, not a separate direct Codex profile.

### Models and reasoning settings

The account catalog checked September 17, 2026 included `glm-5.3`,
`glm-5.3-flash`, `glm-5.2`, `glm-5.1`, and older models. Only the specifically
tested routes establish working inference; a listed model is not a promise
about every plan or model capability.

The previous `ZAI_REASONING_EFFORT` environment example was not implemented by
CCR. `Presets.extra_params` is also not supported. Use request fields supported
by the selected transport, or the documented `force_reasoning_effort` provider
field for **OpenAI-protocol** upstreams; it is not valid for `anthropic` or
`responses` provider entries. See [Configuration](configuration.md).

### Verified behavior

Claude Code 2.1.276 successfully read the test file through GLM-5.3. In the latest
run its final answer contained the correct marker with Markdown around it;
the earlier exact-output tool-loop acceptance is recorded in
[Local operation](local-operation.md). Codex 0.0.0 completed tool round trips
with exact file contents through both the Anthropic and native Responses
upstreams. These were temporary router instances using the main release binary;
normal client defaults and the managed service were not changed by the tests.

## MiniMax

Use a key and model available to the intended MiniMax plan. A subscription key
and a pay-as-you-go key need not spend the same quota even when the endpoint
is the same. The official Anthropic-compatible base for CCR is
`https://api.minimax.io/anthropic/v1`:

```json
{
  "name": "minimax",
  "api_base_url": "https://api.minimax.io/anthropic/v1",
  "api_key": "${MINIMAX_API_KEY}",
  "protocol": "anthropic",
  "models": ["MiniMax-M3.1-Flash-Preview"],
  "transformer": {"use": ["minimax"]}
}
```

This is a provider fragment; add a matching `Router.default`/`Router.tiers`
entry such as `minimax,MiniMax-M3.1-Flash-Preview` to a complete config.
Native Anthropic content blocks do not need a format-conversion transformer
solely because of the name.

MiniMax's current reference lists M3 and M2.x models with different context,
multimodal, and thinking behavior. Check that reference for the chosen model;
do not copy one model's settings to another.

### M3 and M3.1 model IDs (2026-10-08)

Every `MiniMax-M3.*` model ID, including `MiniMax-M3.1-Flash-Preview`, is
treated as the native Anthropic reasoning family: CCR injects
`thinking: {type: "adaptive"}` and does not add the OpenAI-only
`reasoning_split: true`. For the `minimax` provider, CCR also rewrites the
requested model to `MiniMax-M3.1-Flash-Preview` before building the
transformer chain, so a config or route naming `MiniMax-M3` or
`MiniMax-M3.1-Flash` is served by the exact preview ID. This is deliberate:
a direct upstream request for `MiniMax-M3.1-Flash` returned HTTP 200 while
actually serving `MiniMax-M3`, while the explicit preview ID returned HTTP 200
and served the requested model. M2.x models keep the `reasoning_split` path.

MiniMax can also emit malformed visible output: quote-only assistant text, or
text containing MiniMax transport artifacts such as its zero-width tool-call
marker, the `]<]minimax[>[` provider delimiter, or an embedded
NUL. CCR removes those text blocks from assistant history before
replaying it upstream, keeping `tool_use` blocks and tool-result pairing
intact, and cleans the same artifacts from complete responses and streaming
deltas. User messages are never rewritten. When a turn's only text was
malformed, CCR substitutes `[MALFORMED_MINIMAX_OUTPUT_REMOVED]` so the turn
stays structurally valid instead of replaying an empty assistant message.

The `transformer` entry is required. The transformer registry is not selected
by provider name, so a MiniMax provider without `transformer: {use:
["minimax"]}` builds an empty chain and none of the behavior above runs. The
shared fallback policy in `scripts/ccr_fallback_policy.py` pins that entry for
the MiniMax provider, and `ccr-fallback-policy sync` propagates it to every
governed consumer.

These repairs are transformer-local, so they apply to the governed fallback
chain without changing the source-pinned route policy in
`scripts/ccr_fallback_policy.py`. Sanitized output is transport hygiene, not a
model-quality claim: unit and mocked coverage cannot prove the live endpoint
is clean.

### Live verification (2026-09-23, coding-plan key)

The provider fragment above is wired into the workstation router
(`minimax` provider, `MiniMax-M3` alias) and fully verified end-to-end
through CCR's Anthropic path with a coding-plan key (`sk-cp-...`):

- Text: exact-answer probe completed with usage accounting flowing through
  CCR's token audit (40 in / 2 out on the smoke prompt).
- Vision: on the structured two-color probe, **MiniMax-M3 answered exactly
  (`left=red, right=blue`) through both ingress shapes** — `/v1/responses`
  `input_image` (233 input tokens) and `/v1/messages` native Anthropic
  `image` blocks (108 input tokens) — where glm-5.3 standard misread the
  same probe's colors. That is the tested scope: structured color questions
  on this probe, not a general vision-quality claim.
- Streaming: standard Responses SSE through the conversion pipeline
  (`response.created` -> `output_item.added` -> `output_text.delta` ->
  `*.done` -> `response.completed`).
- Failure signatures worth knowing: a malformed key (for example a
  shell-quoted `.env` value that keeps its surrounding quotes) returns
  `401 authentication_error: "login fail: Please carry the API secret..."`;
  an unfunded pay-as-you-go key returns `402 insufficient_balance_error:
  "insufficient balance (1008)"`. Both include a `request_id`.
- CCR's config loader expands `${VAR}` per value after JSON parsing, so keys
  containing quotes or backslashes no longer corrupt the config document.

## Credentials and fallback

Check required variables without printing them, then validate and restart the
intended router instance. CCR does not load `.env` itself. The
[combined Kimi/GLM example](../examples/coding-plans.json) keeps selection
explicit; the [multi-tier example](../examples/config.multitier.json) deliberately
permits fallback between providers. A pinned route in a tier list can still
fall back to another plan. Do not use that as personal/corporate isolation.

References checked September 17, 2026:

- [Z.AI Claude Code](https://docs.z.ai/devpack/tool/claude)
- [Z.AI Codex and individual/team keys](https://docs.z.ai/devpack/tool/codex)
- [Z.AI endpoint guide](https://docs.z.ai/devpack/tool/others)
- [MiniMax Anthropic API](https://platform.minimax.io/docs/api-reference/text-anthropic-api)
