# Provider-limit mismatch investigation, 2026-11-12

## Claim

The apparent “all providers limited” condition is not one shared token-meter
failure. Live evidence separates four overlapping causes:

1. The MiniMax Token Plan is genuinely blocked at the provider for the router's
   own credential, even for a four-token request. The provider returns HTTP 429
   code 2056, “Token Plan usage limit reached”. The `mmx` quota snapshot
   simultaneously reports the five-hour interval at status 1/100% but the
   weekly window at status 2/0%, so reading only the interval is misleading.
2. Z.AI is not account-exhausted. A tiny direct CCR request returned HTTP 200
   in 1.49 seconds. In the preceding two hours, the worker listener recorded
   440 Z.AI successes, 38 Z.AI 429s, and Z.AI preflight estimates from 18,948
   to 270,925 tokens (median about 62,091). Its 429s are therefore transient
   request/concurrency/context limits, not proof that the displayed 77%
   five-hour allowance is absent.
3. Only MiniMax and Z.AI are usable in the active worker chain. DeepSeek and
   Meta return live 402 billing errors; `local27` has no listener; the B550
   SSH tunnel exists but its remote listener is absent. A Z.AI 429 during a
   MiniMax 429 therefore becomes “all tiers exhausted” even though two
   independent problems, not provider-wide token exhaustion, caused it.
4. The OCR listener is being hammered by two stale OpenRouter profiles.
   `poolside/laguna-s-2.1:free` is absent from the live model catalog, and
   `thinkingmachines/inkling:free` is no longer free; OpenRouter returns
   deterministic 404 model-availability errors. In ten minutes these two
   profiles generated 1,397 HTTP 404s and 1,507 all-tier terminations. Live
   `ocr-v1.12.9` processes were identified for both profiles, each with
   concurrency four, alongside MiniMax Preview and Z.AI reviewer processes.

The quota displays do not measure admission. `modelctl status.available` counts
reset opportunities, not tokens. `modelctl usage` reports aggregate provider
windows and does not include RPM, concurrency, model availability, plan-tier
admission, or per-request prefill state. MiniMax also exposes no token
accounting through the Token Plan surface (`modelctl tokens --provider minimax`
returns “does not expose token accounting”).

## Live evidence

- Router MiniMax probe, using `CCR_MINIMAX_API_KEY` without printing it:
  `POST /anthropic/v1/messages` with `max_tokens=4` returned HTTP 429 with
  `rate_limit_error`, message code 2056, and request ID
  `0745d619ae908263413498ae85cdd687`.
- `mmx quota show --output json`: general interval status 1, 100% remaining;
  general weekly status 2, 0% remaining, ending 2026-11-15 16:00 America/Los_Angeles
  (about 66.4 hours after the probe).
- `modelctl usage --provider zcode --json`: 77% remaining in the five-hour
  model-prompt window. `modelctl tokens --provider zcode --json`: 3,663,245,032
  tokens and 29,255 model calls in the weekly telemetry window.
- Worker listener metrics: MiniMax 224 real HTTP 429 hits and AIMD limit 1;
  Z.AI 141 real HTTP 429 hits but recovered to AIMD limit 64 after successes.
- Recent worker log parse: 38 MiniMax prefills and 38 MiniMax 429s; 535 Z.AI
  prefills, 38 Z.AI 429s, and 440 Z.AI successes in the same two-hour window.
- Fallback preflight: Z.AI, OpenRouter, SenseNova, Meta authentication, Holo4,
  and MiniMax model probes were reachable; DeepSeek returned 402, `local27`
  was refused, and B550 port 18081 was reset. The B550 SSH tunnel was live,
  but `ss` on `b550-zt` showed no remote listener.
- OCR log parse over ten minutes: 725 requests for
  `openrouter,thinkingmachines/inkling:free`, 674 for
  `openrouter,poolside/laguna-s-2.1:free`, 1,397 HTTP 404 failures, and 1,507
  all-tier terminations.
- OpenRouter live catalog: no `poolside/laguna*` model; `thinkingmachines/inkling`
  exists only as a paid slug, while `thinkingmachines/inkling-2-small:free` is a
  separate current free slug.

## Deployment and source checks

- Source-authority policy check exited 0 with policy SHA-256
  `8cc2f4e4058d0ad3e0a8a4fea09e2a1e96062daf7ee0d7d3d86b68a5270d0f37` and no
  changed consumers.
- The source policy module, deployed module, and config-root symlink target all
  had SHA-256 `677e67e86bafe7cfce688ece296d2eb55579d8e8879955b615de70495c73ead5`.
- No active consumer contained `maxInflightTokens`. MiniMax deliberately has no
  static preflight budget; the source comment records the 2026-10-25 decision to
  remove the 250,000-token guard and let count AIMD react to token-volume 429s.
- The installed release binary and Cargo target release binary had the same
  SHA-256, `f6563932a0534f2b451e6c692aaa929c8120e0a06075d1e4fa4fa52330a43738`.
  All relevant listeners started after the binary and consumer-config mtimes.
  This kills stale binary/config/module state as the current cause.

Source behavior is consistent with the observations. HTTP 429 is counted per
full `provider,model` route and enters local backoff; a later all-tier 429 can
include a locally skipped route rather than a fresh provider rejection. Local
AIMD halves a route's request-count limit on 429 and grows it only after clean
success. Backoff and admission state are process-local and are not restored as
active gates from Redis.

## Worker evidence

Four read-only audits completed:

- `20261112-212952-a708finc`: 429 classification and propagation.
- `20261112-213002-yjq3vok4`: admission, backoff, reset, and persistence.
- `20261112-213009-svymos5q`: source/deployed drift; completed through Qoder.
- `20261112-213159-q1hr7n9v`: modelctl/mmx quota semantics.

Their reports are consolidated in ignored
`build/reports/provider-limit-investigation/`; this tracked receipt is the
durable synthesis. Worker claims were checked against live metrics, logs,
configuration, hashes, and provider responses by the parent.

### Qoder follow-up

Qoder is a `codex-ccr-worker` launcher-level lane, not a CCR `Router.tiers`
provider. Therefore a successful Qoder attempt bypasses CCR and cannot appear
in the CCR provider metrics, while a rate-limited or otherwise ineligible
Qoder attempt falls back to the normal CCR chain. In this investigation, the
deployed-drift audit completed directly through Qoder; the other three briefs
attempted Qoder, observed provider rate limiting, and then completed through
the CCR fallback chain.

The current launcher pins Qoder display model `DeepSeek-Flash`, machine key
`dfmodel`, catalog identity `DeepSeek-V4.1-Flash-1017`, and reasoning effort
`max`. The live Qoder catalog also exposes `Qwen4-Max-Preview`. A bounded
no-tools/no-MCP probe of that model on 2026-11-12 returned success in 8.17
seconds with result `OK`, request ID
`82f51dc3-ebe9-4fd8-8f93-e41422b519c4`, machine key `qmodel_latest`, and
1.0726371428571428 credits. It is available as a candidate, but it is not the
currently deployed worker model and has not been qualified for the worker
tool/read-only contract.

### Qoder deployment update

The candidate was subsequently promoted and deployed. CCR worker launcher
commit `220808ac5c4961434cf6d410e5b28011e5d476a2` pins
`Qwen4-Max-Preview`/`qmodel_latest`; receipt commit `644da6b` records the
qualification. The global `ccr-worker` installation now matches that source.
A fresh installed-launcher run completed with `engine: qoder`,
`requested_route: qoder,Qwen4-Max-Preview`,
`served_route: qoder,qmodel_latest`, and request ID
`267e5f1c-0cac-4c69-a03f-2313c1605d1b`. A subsequent run through the
user-facing `codex-ccr-worker` wrapper also completed on Qwen4 with request ID
`e085dff4-66d4-451f-b233-283a8dba6d4d`. See
`/Users/kearm/ccr-worker-launcher/evidence/qwen4-max-preview-primary-20261112.md`.

## Verification

`git diff --check -- evidence/provider-limit-mismatch-20261112.md` exited 0.
An index refresh was attempted after adding the receipt and was interrupted
with exit 130 because the local embedding endpoint at
`http://127.0.0.1:1236/v1/embeddings` was unavailable; this does not affect the
live investigation evidence above.

## Non-claims

- No service restart, route change, model-profile change, reset-card use,
  purchase, credential change, or process termination was performed.
- The exact Z.AI 429 sub-limit is not exposed by its HTTP response; concurrency
  or request-rate is inferred from the success/429 mix and tiny-request
  success, not claimed as a named vendor limit.
- OpenRouter catalog availability is a live 2026-11-12 snapshot and may change.
- This investigation does not prove that either replacement OpenRouter slug is
  suitable for OCR quality; only that the configured free slugs are currently
  invalid.
