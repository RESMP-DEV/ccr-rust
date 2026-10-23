# Holo4 local computer-use route — 2026-10-23

## Scope

Registered the local Holo4-35B-A3B vision model as a governed, OpenAI-compatible
CCR provider and exposed it as a **direct-only** route for the main, auth, and
worker consumers. The route can be pinned explicitly, but it never enters the
shared fallback chain, so a vision grounding model cannot become the general
text fallback.

The artifact, native macOS client, resolution measurements, and permissions
receipt are in the `omni-use` repository:
`evidence/2026-10-23-holo4-native-macos-computer-use-client.md`.

## Provider contract

```text
name:         holo4
protocol:     openai
base URL:     http://127.0.0.1:1234/v1
model:        holo4-35b-a3b
maxInflight: 1
credential:   ${CCR_HOLO4_API_KEY} (loopback placeholder; the server needs no auth)
transformer:  maxtoken 4096, only_if_missing
direct route: holo4,holo4-35b-a3b
consumers:    config.json, config-auth.json, glm-workers.json
```

`holo4-35b-a3b` is Apache-2.0 (`Hcompany/Holo4-35B-A3B` at `5084587`). It is
served locally by LM Studio's Metal `llama.cpp` runtime with a 65,536-token
context. The model's own template always reasons unless the client disables
thinking, so requests must leave enough completion budget for reasoning plus
the answer; the `maxtoken` injector supplies 4,096 when a client omits a value.

## Source and live reconciliation

The source policy defines the provider and per-consumer direct routes in
`scripts/ccr_fallback_policy.py`; tests pin the endpoint, model, credential
reference, direct-route membership, and absence from `MACHINE_ROUTE_CHAIN`.
While this work was in flight, another session added the Meta `muse-spark-1.3`
tier and a DeepSeek transformer to the same live policy body. The final source
and live body are their union:

```text
live/source policy body sha256:
d7fa0faaa87cd5b78291eeca7ef5bdded88e4070caa121dddd68fd051ce35ca8
deployed execution copy sha256:
99275dbeefd0d4bdf33a51f2fceb2d6ae712fc5f31f8085952e2efae30263f28
```

Commits: `3748e48` (Holo4 route, includes the concurrent Meta edits present in
the same file) and `a86db25` (DeepSeek transformer reconciliation and pin).
`scripts/test_ccr_fallback_policy.py` passed 38 tests. The deployed copy is
byte-identical to the source.

## Live verification

```text
ccr-fallback-policy check:  policy_sha256 d7fa0faa..., changed [], restart_required []
ccr-fallback-policy sync:   all four consumer configs converged
preflight holo4:            ok, HTTP 200 on /v1/models
worker 3457 / auth 3461:    restarted idle-gated; advertise holo4,holo4-35b-a3b
main 3456:                  restarted through com.kearm.ccr-idle-restart-main
```

A real image request through the worker listener:

```text
POST /v1/chat/completions  model=holo4,holo4-35b-a3b
x-ccr-tier: holo4
served model: holo4-35b-a3b
prompt_tokens 1935, completion_tokens 210 (196 reasoning), 5.89 s
content {"x":18,"y":14}  -> (31.1,15.6) screen points inside AX Apple [10,0,44,33]
```

A second real probe through the main listener, the Codex path, returned
`x-ccr-tier: holo4`, served model `holo4-35b-a3b`, `prompt_tokens=1935`, and
`content={"x":18,"y":14}` in 2.72 s.

The `/v1/models` listing advertises both the qualified route and a bare
`holo4-35b-a3b` id. The bare id has no alias entry, so consumers must request
the explicit `holo4,holo4-35b-a3b` route.

## Delegation surfaces

Agents that need the model request the explicit CCR route. Agents that need the
desktop acted on call the native CLI surfaces on the main Mac:

```text
omni-cu doctor | schema | ground --goal TEXT | act --goal TEXT [--execute] [--foreground]
holo run --base-url http://127.0.0.1:1234/v1 --model holo4-35b-a3b "TASK"
holo mcp    # vendor stdio MCP server exposing holo_desktop
cua shot|state|click|type|key|scroll
```

The `omni-cu` wrapper lives at `~/.local/bin/omni-cu`; the vendor `holo` CLI is
installed as an isolated uv tool.

## Non-claims

The model's grounding accuracy on one sampled desktop is measured in the
`omni-use` receipt, not proved generally here. The `local27` and `strata-b550`
providers were already unreachable before this change (connection refused and
reset), so overall preflight remains false for those pre-existing local tiers.
The main listener was verified by health, `/v1/models`, and a real image
completion, but not by a full Codex turn; its idle-restart agent remained armed
for the concurrent Meta tier reconciliation. OCR was left to the session using
it because it had active review requests. No upstream change was made.
