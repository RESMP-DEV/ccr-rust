# Failover Architecture — Design and Execution Plan

Status: Phases 0 through 4 implemented and locally verified on 2026-10-07.
Phases 3 and 4 are disabled by default pending production qualification. All
`file:line` references in the original design were checked against the
pre-implementation tree and may drift as the repository changes; the tests
and contracts below are authoritative.

This document is the single plan for the "failover v2" work. An executing
agent should be able to implement it from this file plus `AGENTS.md` without
further conversation context. It supersedes nothing; it generalizes the
incident-driven fixes of 2026-10-07.

| Phase | Current status | Evidence owner |
| --- | --- | --- |
| 0. Failure classification | Implemented | `test_failover_controls.rs`, dispatch unit tests |
| 1. AIMD admission | Implemented | `src/admission.rs` and two-upstream integration test |
| 2. Bounded amplification | Implemented | `src/retry_budget.rs`, retry-sweep/failover tests |
| 3. Conversation stickiness | Implemented, disabled by default | `src/stickiness.rs` and conversation integration test |
| 4. Hedging | Implemented, disabled by default | `test_failover_controls.rs` and hedge Prometheus counters |

## How to execute this document

- Implement phases in order (0 → 1 → 2 → 3; phase 4 is optional and last).
  Each phase is independently shippable and must leave the repo green.
- Per phase: write the listed tests first where practical, implement, run the
  full gate (`cargo fmt`, `cargo clippy --all-targets --all-features --locked
  -- -D warnings`, `cargo test --all-features --locked`), update
  `CHANGELOG.md`, and only then `cargo install --path . --force`.
- Never make billable provider calls to verify behavior. Integration tests
  use mocked upstreams; live verification is loopback-only (`/health`,
  `/v1/models`, mocked-upstream round trips).
- Coordinate restarts: main 3456 may be restarted when its in-flight count is
  low; worker 3457 and OCR 3458 listeners must not be restarted while they
  carry active requests. They pick up synced configs on their next idle
  restart.
- If `AppState` gains a field, update every integration-test helper that
  constructs it directly (repo rule in `AGENTS.md`).
- Record completion as a dated entry in this file's Work log (append-only)
  and, for live activation, in `~/.claude-code-router/LOCAL_SETUP.md`.

## Why: the 2026-10-07 incident

Main-listener traffic stalled while the primary provider family (Z.AI GLM
coding plan) returned intermittent 429s without `Retry-After` under
concurrency. The plan was not out of quota (47% of the 5-hour window
remained; confirmed via `modelctl usage`): it is a per-request concurrency
cap that admits some requests while rejecting others. Measured during ~70
minutes on port 3456:

- 1,086 rate-limited zai failures; every zai tier carried `consecutive`
  429 counters in the hundreds and rolling 60s skip windows (pre-fix
  exponential behavior).
- 4,174 DeepSeek request failures, all deterministic 400s
  (`tool_use`/`tool_result` adjacency, `content[].thinking` validation),
  retried pointlessly across sweeps.
- 15 requests held open simultaneously in `retrySweeps` loops for 200-525
  seconds each; one succeeded on zai "after 525.20s".
- EWMA latency gauges poisoned to ~1e92 (unclamped durations fed into the
  softmax/gauge).
- After extending the chain and flattening 429 pacing to 1s: 134 successes
  in 2 minutes, zero hold events.

Root causes, one per link in the chain: deterministic 400s classified
retryable; time-based (not capacity-based) tier skipping against a
concurrency-capped upstream; no admission control anywhere; unbounded hold
by default with no retry budget; no conversation stickiness, so a Responses
-protocol conversation can land mid-stream on an Anthropic-protocol tier,
which is exactly when conversion bugs surface.

## Current architecture (ground truth)

Verified against the tree at the revision above.

### Tier cascade and sweep loop (`src/router/mod.rs`, `handle_messages`)

- Tier list from `config.backend_tiers()` (`Router.tiers` if set); strict
  order keeps config order when `strictTierOrder` (mod.rs:150-156),
  otherwise EWMA softmax sampling (`sort_tiers_with_config`,
  src/routing.rs:214-339).
- Direct routing (`provider,model` in the model field, `!ignoreDirect`)
  pins that tier at the front, `pinned_prefix_len=1` (mod.rs:176-201);
  web-search tier prepends and grows the prefix (mod.rs:204-215).
- GP rerank adopts only when `!strict_tier_order`; never moves the pinned
  prefix (src/gp_router.rs:139-147).
- Per tier: `for attempt in 0..=max_retries` (default 3 retries / 4
  attempts, 100ms base backoff, `TierRetryConfig`, src/config/types.rs:604-675).
  Backoff sleep (EWMA-scaled) only on `Other` failures; 429 and `Rejected`
  move to the next tier immediately (mod.rs:512, 536, 552-566).
- Sweep loop: continues while `sweeps.enabled && (saw_rate_limit ||
  saw_retryable_failure)` (mod.rs:575-579); `maxSweeps == 0` and
  `maxHoldMs == 0` mean unlimited (mod.rs:275-276, 580-583; types.rs:689-730).
  Cooldown floor `sweepCooldownMs` (default 2000), stretched to the largest
  rate-limit backoff hint capped at 60s, clamped to remaining hold budget
  (mod.rs:592-605).
- Terminal response: synthesized 429 (with `retry-after` + `x-ccr-tier`)
  only when the final sweep saw exclusively rate limiting; otherwise 503
  with per-tier attempt summary (mod.rs:103-127, 621-648).

### Failure classification (`src/router/dispatch.rs`)

- `TryRequestError` = `RateLimited(Option<Duration>)` | `Rejected(u16)` |
  `Other` (src/router/types.rs:20-29).
- `provider_upstream_error` (dispatch.rs:657-681): **401-404 → `Rejected`
  (deterministic); everything else, including 400 and 422, → `Other`
  (retryable)**. Anthropic-protocol path duplicates this
  (dispatch.rs:1251-1281).
- 429 is intercepted before classification: `record_429` + `RateLimited`
  (dispatch.rs:869-875).
- `rejected_tiers: HashSet<String>` memo (mod.rs:283) is populated only on
  `Rejected` (mod.rs:535) and skipped at sweep top (mod.rs:311-314). There
  is no memo for a 400-class "this provider rejects this request shape".

### Rate-limit tracker (`src/ratelimit.rs`)

- `should_skip_tier` (ratelimit.rs:45-85): skips while `backoff_until` is
  active; skips on `remaining == 0` before `reset_at` only when
  `honor_ratelimit_headers` (default true; zai sets false).
- `record_429` (ratelimit.rs:116-141, post-2026-10-07): server
  `Retry-After` honored verbatim capped at 60s; no-guidance 429 paces 1s
  flat (no escalation). Keyed by full `provider,model` route.
- Generation guard: dispatch snapshots `generation` before an attempt;
  `record_success_if_current` clears backoff only if no newer 429 landed
  (ratelimit.rs:151-178). **Called only from streaming completion**
  (src/router/streaming.rs:362, 646); non-streaming successes never clear
  a backoff.

### Streaming

- Pre-first-token failures (embedded error, transport error, peek timeout)
  → `Other` → cascade retries the next tier (dispatch.rs:147-238).
- Post-first-token: no cascade, ever. Idle timeout emits an in-stream error
  event (streaming.rs:430-440); upstream chunk errors forward as body
  errors (streaming.rs:549-553).

### EWMA (`src/routing.rs`)

- Alpha 0.3, min_samples 3, failure penalty 2.0× current EWMA. Updated per
  attempt; **durations unclamped** (routing.rs:76-83, 109-129, 362-367).
  Influences softmax ordering (only when not strict), per-attempt backoff
  scaling, and a gauge mirror.

### Concurrency

- None enforced. `max_streams` (default 512) and `active_streams` are
  metrics and GP features only (main.rs:105-108; streaming.rs pumps).

### Identity

- None. `detect_frontend` (src/frontend/detection.rs:27-39) is stateless.
  Tier order is recomputed per request.

## Invariants any change must preserve

1. Pinned prefix immobility: direct-routed and web-search tiers stay at
   positions `0..pinned_prefix_len` under EWMA ordering, GP rerank, and any
   new ordering signal.
2. Terminal semantics: synthesize 429 only when the final sweep saw
   exclusively rate limiting; 503 otherwise; sweeps must not continue when
   the last pass saw only deterministic rejections.
3. Deterministic rejections are attempted at most once per request
   (`rejected_tiers`), and never enter the retry budget.
4. Post-first-token opacity: once any byte has streamed, failures surface as
   in-stream error events; only pre-first-token failures may cascade.
5. Rate-limit bookkeeping: per-route keys, generation-guarded success
   clearing, 60s cap on server `Retry-After` and sweep-cooldown stretch,
   never re-cascade with zero cooldown.
6. Repository rules from `AGENTS.md`: transformer dual registry (n/a here),
   `AppState` field additions propagated to integration-test helpers,
   `CHANGELOG.md` updated for routing behavior changes, `cargo install
   --path . --force` after code changes.

## Prior art (evidence base)

Production LLM gateways converge on five mechanisms; parameters below come
from each system's official documentation.

| Concern | LiteLLM | OpenRouter | Portkey | Kong AI Gateway | Cloudflare AI Gateway |
| --- | --- | --- | --- | --- | --- |
| Down state | `allowed_fails` (3) → `cooldown_time` (5-30s), Redis-shared | rolling 30s outage window | none | `max_fails`/`fail_timeout`, ejection capped | none (retry headers) |
| 429 | retry group, then fallback; steering is immediate | steer now; `Retry-After` emitted | `on_status_codes` fallback | `http_429` in `failover_criteria` | per-request `cf-aig-*` headers |
| Deterministic 4xx | never retried; body parsed: context-window and content-policy get separate chains | `require_parameters` prefilters | fail fast | client errors never trigger failover | not retried |
| Amplification bound | `num_retries` ~2 then fallback | small | small | bounded retries | `cf-aig-max-attempts` ≤ 5 |
| Consistency | custom strategies | same model any provider | `sticky.hash_fields` + TTL | `hash_on_header` consistent hash | none |
| Streaming | fallback pre-first-chunk only; typed SSE errors after | same | same | same | timeout measured to first byte |

Classical practice adds the two controllers the gateways underuse, and both
fit our concurrency-capped upstream exactly:

- Adaptive concurrency limits (Netflix `concurrency-limits`, gradient2/AIMD):
  per-tier in-flight cap; multiplicative cut on rejection, additive growth
  on success; converges to the upstream's true admission rate with no dead
  time. Time-based backoff remains correct only for true outages (5xx,
  connect failures), where time heals.
- Retry budgets (Envoy `retry_budget` ~20% of active+pending with a floor;
  Finagle `RetryBudget` 20%): bound aggregate retry amplification, which
  per-request attempt caps cannot do alone.

Sources: docs.litellm.ai (routing, reliability, load balancing),
openrouter.ai/docs (provider routing, errors), docs.portkey.ai (fallbacks,
load balancing), developer.konghq.com (AI gateway load balancing,
ai-proxy-advanced), developers.cloudflare.com (AI gateway request handling),
envoyproxy.io (circuit breakers, outlier detection, router filter),
github.com/Netflix/concurrency-limits, netflixtechblog.com (adaptive
concurrency limits), cacm.acm.org (The Tail at Scale), grpc.io (request
hedging, proposal A6), aws.amazon.com (Exponential Backoff and Jitter),
RFC 9110 §10.2.3 (Retry-After).

## Design

### Phase 0 — failure classification (prerequisite)

**Goal:** deterministic upstream rejections cost exactly one attempt per
request per provider; context-window errors remain steerable.

Spec:

1. In `provider_upstream_error` (dispatch.rs:657-681) and the Anthropic
   duplicate path (dispatch.rs:1251-1281), classify 400 and 422 as
   `Rejected` **unless** the error body matches a context-window signature
   (`prompt is too long`, `context_length_exceeded`, `maximum context
   length`, `context window`), which stays `Other` so the cascade moves to
   a larger-context tier (LiteLLM's `ContextWindowExceededError` pattern).
2. `Rejected` already flows into `rejected_tiers` (once-per-request memo),
   stops setting `saw_retryable_failure`, and shapes the terminal 503.
   Verify a sweep that sees only deterministic rejections terminates
   immediately (invariant 2).
3. New counter `ccr_deterministic_rejections_total{tier}`.

Tests: integration test with a mock upstream always returning 400 → exactly
one upstream hit, fast 4xx/503 to the client, no sweep events; same setup
with a context-window body → cascades to the next tier and succeeds.

Acceptance: full gate green; the 4,174-waste class is structurally
impossible; `CHANGELOG.md` records the behavior change.

### Phase 1 — capacity-based steering (AIMD admission)

**Goal:** replace time-based tier skipping for 429s with per-tier adaptive
in-flight limits, so a partially-admitting plan is neither hammered nor
idled.

Spec:

1. New `src/admission.rs`: `AdmissionTracker` keyed by `provider,model`
   route, holding per tier `inflight: AtomicUsize` and `limit: AtomicUsize`
   plus config. AIMD: on 429 → `limit = max(min_limit, limit / 2)`; on
   upstream success (clean stream completion or non-streaming 2xx) →
   `limit = min(max_limit, limit + 1)`. Defaults: initial 8, min 1, max 64,
   per-provider override via new optional `Provider.maxInflight`.
2. Dispatch integration (mod.rs:315-324 selection loop): a tier with
   `inflight >= limit` is deferred to the end of this pass's eligible list
   (not skipped globally). Acquire the slot immediately before the attempt;
   release on attempt end for non-streaming, and on stream completion for
   streaming (hook the existing pump guards in streaming.rs).
3. `record_429` keeps `Retry-After` handling (server-directed no-dispatch
   window) and its counters; the 1s flat pacing window becomes unnecessary
   once admission controls concurrency and may be reduced to a hint. Do not
   remove it in this phase; Phase 1 must be revertible to current behavior
   via config (`admission.enabled`, default on).
4. Sweep semantics: each pass dispatches to any tier with a free slot. If
   every eligible tier is at its limit, sleep `sweepCooldownMs` with full
   jitter (AWS full-jitter: `random(0, cooldown)`), then re-pass. The
   sweep-cooldown stretch to rate-limit backoff (mod.rs:592-605) applies
   only to server-directed `Retry-After` windows.
5. Metrics: `ccr_tier_inflight{tier}`, `ccr_tier_limit{tier}`,
   `ccr_admission_defers_total{tier}`.

Tests: unit tests of AIMD transitions; integration test with a mock
concurrency-1 upstream and two concurrent requests → one admitted, one
steered to the second tier, neither retries the saturated tier; replay of
the 2026-10-07 shape (three zai tiers concurrency-capped, deterministic-400
final tier) completes within a bounded number of attempts.

Acceptance: full gate green; no time-based tier skip remains for
no-guidance 429s; live loopback verification that traffic flows when the
primary family is intermittently rejecting (mocked).

### Phase 2 — bounded amplification

**Goal:** no configuration can produce unbounded retry storms or poisoned
stats.

Spec:

1. Global retry budget: before any attempt beyond the first for a request,
   require `retries_in_flight + 1 <= max(3, 0.2 * active_requests)`. On
   overflow, end the sweep and synthesize the terminal response per
   invariant 2. Metric `ccr_retry_budget_overflow_total`. Config
   `Router.retryBudgetPercent` (default 20, 0 disables).
2. Bounded hold by default: when `retrySweeps.enabled`, default
   `maxHoldMs` to 60000; `0` remains the explicit infinite opt-in and
   validation warns on it. Document in `docs/configuration.md`.
3. EWMA robustness: clamp recorded attempt durations to the effective
   `API_TIMEOUT_MS`; clamp the failure-penalty product; clamp the gauge
   mirror (`sync_ewma_gauge`); add `ccr_hold_wait_seconds` histogram so
   hold time is visible separately from attempt latency.
4. Non-streaming successes clear backoff: call
   `record_success_if_current` (and admission success) in the non-streaming
   success path, mirroring streaming.rs:362.

Tests: N concurrent requests against always-429 mocks assert bounded total
upstream hits (≤ budget expression); EWMA clamp unit test (feed 1e9s
duration, gauge stays finite); non-streaming success clears a standing
backoff.

Acceptance: full gate green; the 1e92 gauge class is impossible; a
worst-case config cannot exceed the retry budget.

### Phase 3 — conversation stickiness

**Goal:** a conversation keeps its provider family across turns; crossing
families happens only on exhaustion, making failover invisible.

Spec:

1. Conversation key extraction, in priority order: Anthropic
   `metadata.user_id`; OpenAI `user`; fallback: first 64 bytes of SHA-256
   of the leading system prompt. Absent a key, behavior is exactly today's.
2. TTL map (default 1h, `Router.stickySessions.ttlMs`) key →
   `{provider, protocol_family}` updated on each successful response.
3. Ordering: stable-partition the eligible tier list so tiers of the
   remembered family come first, preserving relative order within each
   group (works under `strictTierOrder`, since within-family order is the
   configured order; never moves the pinned prefix). Cross-family movement
   happens only when the preferred family is exhausted or skipped.
4. Config: `Router.stickySessions {enabled: false, ttlMs: 3600000}` — off
   by default until live verification.

Tests: same conversation key served by the same provider across three
requests with two equal mock tiers; TTL expiry re-allows reordering; pinned
prefix never moves.

Acceptance: full gate green; live verification on 3456 with stickiness
enabled shows conversations staying on-family through intermittent primary
429s.

### Phase 4 — hedging (optional, last)

When enabled (`Router.hedging {enabled: false, ttftThresholdMs}`), CCR
launches the next eligible tier when the primary attempt has not produced a
usable result after `ttftThresholdMs`; the first usable result wins and the
loser is cancelled. Hedges draw from both the global retry budget and the
fallback tier's admission permits. Hedging remains disabled by default so
production TTFT and spend behavior can be reviewed before activation. For
streaming providers the existing pre-first-token peek bounds the primary;
for non-streaming providers the usable completed response is the race
boundary.

## Configuration surface summary

| Field | Default | Phase |
| --- | --- | --- |
| `Provider.maxInflight` | 8 (initial/max 8/64 AIMD bounds) | 1 |
| AIMD admission | always on for upstream attempts | 1 |
| `Router.retryBudgetPercent` | 20 (0 = off) | 2 |
| `retrySweeps.maxHoldMs` default when enabled | 60000 (0 = explicit infinite) | 2 |
| `Router.stickySessions.enabled / ttlMs` | false / 3600000 | 3 |
| `Router.hedging.enabled / ttftThresholdMs` | false / unset | 4 |

Backward compatibility: every knob defaults to current observable behavior
except Phase 0 (400/422 become non-retryable; intentional, CHANGELOG) and
Phase 2's bounded-hold default (was unbounded; validation warns when 0 is
explicit).

## Open choices

| Topic | Current position | Decision gate |
| --- | --- | --- |
| AIMD vs Gradient2 limiter | AIMD (simpler, loss-driven) | Switch if zai shows limit oscillation under live load; gradient needs RTT baselines we only have per-tier EWMA for |
| Strict-order stickiness semantics | Stable partition by family (preserves within-family order) | If partitioning surprises in live traffic, demote stickiness to strict-order-incompatible like topK |
| Sticky identity source | system-prompt SHA-256 (normalized request has no metadata/user field) | Add metadata.user_id or user extraction when a frontend reliably carries either field |
| Retry budget percent | 20% + floor 3 (Envoy/Finagle precedent) | Tune against `ccr_retry_budget_overflow_total` under real storms |
| Hedge enablement | Off | Enable only with measured interactive tail latency data |

## Non-goals

- No change to the pinned-prefix or GP-rerank contracts beyond what is
  stated (stickiness partitions, never moves the prefix).
- No new upstream providers or protocol work; DeepSeek tool-history
  normalization gaps remain open transformer work, tracked separately —
  Phase 0 makes them non-fatal and Phase 3 makes them rare.
- No multi-instance coordination (single-process tracker state, like today).
- No client-facing API changes beyond error-shape preservation.

## Operational route baseline (2026-10-07)

This section records current routing authority; it is separate from the
proposed failover-v2 phases above.

Main-machine route expansion is source-pinned in
`scripts/ccr_fallback_policy.py`. The live `fallback-policy.json` is derived
state. On `2026-10-07`, after explicit operator authorization and live
qualification, the shared chain ends:

```text
deepseek,deepseek-flash -> minimax,MiniMax-M3.1-Flash-Preview
```

The complete consumer chains are:

```text
main/auth: zai,glm-5.3 -> zai,glm-5.3-flashx -> zai,glm-5.3-flash
           -> deepseek,deepseek-flash
           -> minimax,MiniMax-M3.1-Flash-Preview

worker:    zai,glm-5.3-flashx -> zai,glm-5.3-flash -> zai,glm-5.3
           -> deepseek,deepseek-flash
           -> minimax,MiniMax-M3.1-Flash-Preview

OCR:       openrouter,nvidia/nemotron-3-ultra-550b-a55b:free
           -> zai,glm-5.3-flashx -> zai,glm-5.3-flash -> zai,glm-5.3
           -> deepseek,deepseek-flash
           -> minimax,MiniMax-M3.1-Flash-Preview
```

The disambiguated preview model ID is contractual. A direct upstream request
for `MiniMax-M3.1-Flash` returned HTTP 200 while serving `MiniMax-M3`; the
same test for `MiniMax-M3.1-Flash-Preview` returned HTTP 200 and served the
requested preview model. Both shared providers passed direct credential
preflight before activation. After all four CCR consumers were restarted while
idle, live router logs showed real cascades to MiniMax across main, worker, and
OCR traffic, with MiniMax completing requests on each listener class.

The source guard remains the review boundary: route changes require a reviewed
source change, tests, `ccr-fallback-policy preflight --json`, synchronized
consumers, and idle restarts. The failover-v2 work in this document remains
proposed and does not change that operational contract until its phases are
implemented.

## Work log (append-only)

- 2026-10-07: Document created from the 2026-10-07 incident analysis and a
  three-way research pass (LLM gateway survey, classical failover patterns,
  internal architecture map). No phases implemented yet; the interim 429
  1s-pacing change and the extended fallback chain (azure + minimax tiers)
  are live and documented in `~/.claude-code-router/LOCAL_SETUP.md`.
- 2026-10-07: Corrected the stale operational route note and recorded the
  source-pinned policy guard. Unauthorized Azure and policy-outside MiniMax
  were removed, then the operator explicitly approved MiniMax M3.1 Flash
  Preview as the tier after DeepSeek. Source revision `c9744c6` pins the exact
  model ID and full provider definition. Live credential preflight passed for
  DeepSeek and MiniMax; exact served-model testing rejected the ambiguous
  `MiniMax-M3.1-Flash` alias because it serves MiniMax-M3. All governed
  listeners were restarted at zero active connections, health checks passed,
  and real logs showed MiniMax completions on main, worker, and OCR routes.
- 2026-10-07: Implemented failover-v2 Phases 0 through 3 on branch
  `feat/failover-v2`. Deterministic 400/422 failures are now one-attempt
  rejections except context-window errors; per-route AIMD admission, global
  retry budgets, bounded default hold, EWMA clamps, hold-wait telemetry, and
  optional conversation-provider stickiness are implemented. Full
  `cargo test --all-features --locked`, strict Clippy, and formatting passed.
  Follow-up in the same review round: Phase 4 hedging is implemented but
  disabled by default. It draws from the retry budget and fallback admission
  permits, cancels the loser, and exposes launch/win counters. The operator
  explicitly accepted hedged duplicate spend for these quota-backed tiers;
  default-off remains conservative until production TTFT is measured.
- 2026-10-08: Repaired MiniMax M3/M3.1 routing and malformed-output replay on
  branch `work/minimax-m3-compat`, from clean `origin/main` revision `c3bf76c`.
  The `minimax` transformer now recognizes every `MiniMax-M3.*` ID, emits
  native Anthropic adaptive thinking without the OpenAI-only
  `reasoning_split`, and cleans quote-only or MiniMax-control-marker text from
  assistant history, complete Anthropic responses, and text streaming deltas
  while preserving `tool_use` and tool-result pairing. User content remains
  untouched. Router dispatch canonicalizes MiniMax M3-family requests to the
  contractual `MiniMax-M3.1-Flash-Preview` ID before chain construction and
  protocol overwrite, without changing the source-pinned fallback policy.
  Evidence: 25 focused `minimax` tests, strict Clippy, and the full locked
  all-features suite all passed. Non-claims: mocked tests do not prove the
  live MiniMax endpoint is artifact-free, no billable provider request was
  made, and no listener was restarted or installed.
