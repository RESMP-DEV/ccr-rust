# Upstream routing research and local 429 diagnosis, 2026-10-18

## Scope

Three read-only CCR workers researched current public gateways and routers, then
compared them with the local checkout at `bfb525ef37b3f6feb6f06b98b2ccf13a2b019d3a`
(dirty recovery/runtime-health work present). No upstream issue, PR, comment, or
patch was opened.

## Investigated upstream revisions

- `BerriAI/litellm` docs and repository metadata; worker report:
  `build/reports/upstream-research/litellm.txt`.
- `Portkey-AI/gateway` @ `669825cbe89ee51569918b8f78a9db486fd69dd4`;
  `envoyproxy/ai-gateway` redirects to `theagentrouter/agent-router` @
  `12fef6422579f544e84be8167d0979527d3576d4`; `llm-d` @
  `fd6dc3dd2071ff3f067b5a3e33e9b8f1ac2f9818`; and `llm-d/llm-d-router` @
  `23dea2f75087d61a0b7293412a553f51d004bb31`. Worker report:
  `build/reports/upstream-research/portkey-envoy-llmd.txt`.
- `tensorzero/tensorzero` @ `62eb8f63e8ec62018d70420dbf1a8c5d1c026315` and
  `QuantumNous/new-api` @ `55bb6b903c63fcddbf71740d7267b653fe1dbd47`.
  Worker report: `build/reports/upstream-research/tensorzero-newapi.txt`.

All three workers exited 0 on the exact MiniMax preview route with no failed
tool commands. Their reports are worker evidence, not acceptance of a local
implementation.

## Highest-value local gaps

1. **Account-scoped limits across listener processes.** LiteLLM supports Redis
   router-state/cooldowns; Agent Router and TensorZero implement distributed
   quota/rate policies. CCR currently creates one in-memory
   `RateLimitTracker` and `AdmissionTracker` per listener. Main, worker, auth,
   and OCR therefore learn MiniMax rejection independently while sharing one
   credential.
2. **Token- or cost-denominated budgets.** Upstream projects expose token,
   spend, request, and window budgets before dispatch. CCR has reactive 429
   backoff, AIMD in-flight admission, and cost inputs to GP ordering, but no
   pre-call tenant/provider budget.
3. **Cascade provenance and classified failures.** LiteLLM and TensorZero retain
   per-candidate failure evidence and provider statistics. CCR logs attempts,
   but responses and durable metrics do not fully explain which route failed,
   why, and whether a later tier recovered the turn.
4. **Readiness versus liveness/draining.** Agent Router separates request
   timeout, per-try idle timeout, and readiness. CCR's runtime `/health` is new;
   it still does not expose a supervisor-specific draining state.
5. **Cache-aware routing and response caching.** llm-d routes by prefix/KV-cache
   locality; Portkey caches responses with explicit HIT/MISS semantics. CCR
   counts cached tokens but neither routes by cache locality nor caches
   deterministic responses.
6. **Declarative affinity rules.** new-api supports scoped regex/header/JSON
   affinity rules with hybrid Redis/LRU state and invalidation. CCR stickiness
   derives one conversation key and keeps only an in-process map.
7. **Cross-vendor encrypted-reasoning cleanup.** LiteLLM strips reasoning a
   fallback provider cannot decrypt. CCR has provider-family stickiness, but no
   proven fallback-time cleanup/test for cross-vendor reasoning replay.
8. **Canary/shadow traffic.** Agent Router supports weighted backend refs. CCR's
   GP optimization cannot deliberately reserve a small percentage for candidate
   qualification or shadow a request to a second tier.
9. **Capability eligibility.** new-api filters channels by path/task and
   converter requirements. CCR can pin direct routes but lacks a general
   provider capability predicate before fallback selection.

CCR is already ahead of these projects in several respects: strict tier order,
retry amplification budgets, tail-latency hedging, EWMA failure penalties,
credible-set/cost GP ordering, request batching, and strong protocol
translation coverage. These should be preserved rather than replaced.

## Live 429 diagnosis

At 2026-10-18 14:55 UTC, main and worker both had recent MiniMax 429 backoffs:

- Main: 7 MiniMax rate-limited failures, 46 Z.AI backoffs, 2 active streams.
- Worker: 6 MiniMax and 6 Z.AI rate-limited failures, 6 active streams.
- OCR: 12 active requests, 3 MiniMax and 145 OpenRouter rate-limited failures.
- MiniMax usage was not account-exhausted: the five-hour window showed 18%
  remaining and weekly usage showed 91% remaining.

This is therefore transient rate/concurrency rejection, not exhausted quota.
Each listener independently reported MiniMax admission successes and 429s while
sharing the credential. Their independent AIMD limits had grown to 35, 54, and
64 respectively, so local ceilings did not represent one shared upstream
account. A prior Codex turn did terminate at 10:00 UTC with
`exceeded retry limit, last status: 429 Too Many Requests`; router logs around
that window show mixed Z.AI 429s and MiniMax/Z.AI successes, consistent with
overload rather than a permanently dead tier.

No routing policy was changed during this research turn.

## Recommended sequence

1. Add a conservative explicit MiniMax `maxInflight` in the reviewed source
   policy as an immediate local guard, then measure aggregate 429s before
   selecting a permanent value.
2. Design account-scoped state shared by all listeners (initially a local
   coordination service or durable store), including inflight, 429 generation,
   and retry-after.
3. Add cascade provenance metrics/response fields so a client-visible 429 lists
   every candidate and reason.
4. Add pre-call token/cost budgets for API-key suites.

## Non-claims

- This is source/docs research plus local log inspection; no new upstream
  feature was executed locally.
- The workers did not prove absence of every feature across the full CCR tree;
  "gap" means not found in the named subsystems.
- The MiniMax concurrency ceiling was not measured. The independent per-listener
  ceilings and mixed 429/success behavior are verified; their exact aggregate
  threshold remains unknown.
