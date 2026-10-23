# GLM-5.3 vs Flash-tier Usage Ratio and Offload Assessment — 2026-10-22

## Scope

Read-only assessment of model-tier distribution across the three governed CCR listeners
(3456 main, 3457 workers, 3458 OCR/review), requested by the operator to find levers that
force more work onto the Flash family (MiniMax M3.1 Flash, GLM-5.3-Flash) and off GLM-5.3,
without dropping interactive quality. No router configuration, source policy, listener
state, or client pin was changed.

## Method and evidence

- Live Prometheus counters per listener (`curl 127.0.0.1:<port>/metrics`), 2026-10-22
  ~18:15–18:30 local, with process-start windows from `ps` (3456 since 13:59, 3457 since
  14:55, 3458 since 10:34 local).
- Seven-day log sweep (2026-10-15T00:00Z → 2026-10-23T01:30Z) delegated to read-only
  `codex-ccr-worker` run `20261022-183045-5fdd6yxh` (MiniMax-served, exit 0, 0 failed
  commands, 472 s). Full report at `/Users/kearm/.cache/ccr-worker-runs/20261022-183045-5fdd6yxh/final.txt`.
- Parent verification of worker counts: `grep -a 'model: glm-5.3 (frontend' service.stdout.log`
  for 2026-10-22 returned 6867 (matches worker Table A-1 exactly); `grep -a 'model: zai,glm-5.3
  (frontend: ClaudeCode)' ocr.stdout.log` for 2026-10-22 returned 1170 (matches Table C-1/C-3).
- Quota probes: `mmx quota show` (MiniMax interval 57% / weekly 55% remaining);
  `modelctl usage --provider zcode` (5-hour prompt window 95% remaining at probe time);
  `ccr-fallback-policy check --json` (no drift, policy sha fc2f0bb7…).
- Policy context: `scripts/ccr_fallback_policy.py` lines 20–90 (2026-10-21 user authorization
  making MiniMax the worker primary because the Z.AI plan keeps exhausting its monthly
  utilization window; observed enforcement is the rolling 5-hour prompt window and
  concurrency caps, monthly counter 4000/4000 resets 2026-10-28).

## Measured ratio

Requested-model counts, full window (120,026 requests across lanes):

| Family | Main (A) | Workers (B) | Review (C) | Total | Share |
|---|---|---|---|---|---|
| GLM-5.3 (all aliases) | 52,451 | 84 | 10,056 | 62,591 | 52.1% |
| MiniMax M3.1 Flash | 67 | 10,934 | 14,149 | 25,150 | 20.9% |
| GLM-5.3-Flash(-x) | 2 | 9,244 | 72 | 9,318 | 7.8% |
| OpenRouter free tiers | 0 | 0 | 21,040 | 21,040 | 17.5% |
| Other (qwen/deepseek/local/strata) | ~2,206 | ~724 | ~1 | ~2,931 | 2.4% |

Post-stabilization view (Oct 21–23, after the 2026-10-21 MiniMax-first policy):

- **Main lane is 98.8% GLM-5.3** (18,032 of 18,259 requests; live 4.5 h counters: zai 2,537
  vs minimax 141 vs deepseek 44 requests; input tokens zai 768 M vs minimax 42 M vs
  deepseek 16 M). The Codex client pin (`~/.codex/config.toml` `model = "glm-5.3"`) plus
  `Router.default = zai,glm-5.3` make this lane structurally GLM-5.3.
- **Workers lane is 100% Flash-family**: post-switch days run MiniMax-primary with
  11–43% spill to GLM-5.3-Flash (Oct 22 attempt-1s: MiniMax 5,073 vs flash 611 = 10.7%;
  live 3.5 h counters: 614 vs 464 = 43% during a burst). Spill is driven by MiniMax
  concurrency 429s (80 rate-limited events in the live window), not quota (57%/55% healthy).
- **Review lane still leaks GLM-5.3**: 10,056 requests in-window pinned `zai,glm-5.3`
  from a ClaudeCode-format frontend (10,054; only 2 from Codex), on a lane whose config
  intentionally offers only glm-5.3-flash on zai. Of those, only ~1,788 succeeded on zai
  (GLM-5.3 burn against design intent); ~2,240 spilled to MiniMax, 883 to OpenRouter,
  and ~5,000 churned retry sweeps (6,723 admission defers labeled zai,glm-5.3; 814 holds
  in the 120 s bucket; 12.6 h cumulative hold time in ~8 h of wall clock).

Attempt-1 "Trying" lines overcount requests under retry sweeps (worker anomaly 5/6) and
mix pre-10-21 policies; the requested-model and live-counter views above are the
authoritative ones.

## Findings

1. The GLM-5.3 burn is concentrated in exactly two places: the interactive main lane (by
   design, quality-critical) and the review-lane stale `zai,glm-5.3` pin (waste, against
   the lane's design).
2. The worker delegation chain is already 100% Flash-family and cannot be improved by
   reordering; its only inefficiency is MiniMax concurrency-429 spill onto GLM-5.3-Flash,
   which still draws the shared Z.AI window.
3. GLM-5.3-Flash is not free: per the 2026-10-21 authorization rationale it draws the same
   Z.AI monthly utilization window as GLM-5.3. The offload targets that actually relieve
   the Z.AI plan are MiniMax (healthy quota) and the OpenRouter free tiers; GLM-5.3-Flash
   relieves concurrency and request-count pressure on the 5-hour window but not the
   monthly window.
4. Codex-side session-level offload is already wired: `~/.codex/ccr-live-model-catalog.json`
   carries `MiniMax-M3.1-Flash`, `minimax,MiniMax-M3.1-Flash`, `glm-5.3-flash`,
   `zai,glm-5.3-flash` aliases over the 3456 listener.
5. ZCode main sessions run GLM-5.3 directly on the same plan family outside CCR; ZCode
   subagents already run GLM-5.3-Flash natively, so that side is already offloaded.

## Levers, in priority order (none applied in this assessment)

1. **Eliminate the review-lane `zai,glm-5.3` pin** — pure win, no quality exposure:
   reviewer traffic was designed for the free/flash lane. Either repin the client
   (the sender is an Anthropic-format client, frontend-detected as ClaudeCode, with a
   ~1,721-tool catalog and 525-message system prompt; native `~/.claude/settings.json`
   verified clean) or add a consumer-scoped canonicalization in
   `scripts/ccr_fallback_policy.py` that rewrites `zai,glm-5.3` pins to `zai,glm-5.3-flash`
   for `ocr-reviewers.json`, mirroring the existing legacy-`-Preview` alias
   canonicalization precedent. Source-policy change: tests + preflight + idle-gated restart.
2. **Run secondary/parallel Codex sessions on Flash aliases** (`codex -m MiniMax-M3.1-Flash`
   for maximum Z.AI relief; `-m glm-5.3-flash` when same-provider Responses fidelity
   matters). Zero policy change; primary interactive session stays on glm-5.3.
3. **Cap MiniMax inflight on the workers lane** via an explicit `maxInflight` on the
   minimax provider in the source policy (validated range 1–64, AIMD-pinned per
   `src/admission.rs`), tuned to the observed concurrency limit, so 429 churn becomes
   local admission holds and worker volume stops spilling to GLM-5.3-Flash. Requires
   measuring the real MiniMax concurrency ceiling from the 429 bodies first.
4. **Increase delegation volume** — already repo policy; every delegated piece is parent
   GLM-5.3 burn avoided at zero quality cost, since the parent keeps synthesis.
5. **Gated pilot only if 1–4 are insufficient**: request-class/turn-depth aware routing on
   the main lane via `src/gp_router.rs` (`request_class`, `dependency_count`,
   `credible_set_cost_order` exist but are inactive under `strictTierOrder: true`),
   following Option 2 of `evidence/prefill-cascade-routing-assessment-20261021.md`.
   Keep Turn-0/architectural turns on glm-5.3; route bounded continuation and
   compaction-class turns to glm-5.3-flash. Same-provider handoff avoids the
   thinking-signature hazard but forfeits cross-model prompt-cache locality; needs
   measured `cache_read` economics before enabling.

Quality guardrails: main-lane primary stays `zai,glm-5.3` (interactive synthesis is the
quality-critical surface and the anchor of the cascade design); flash/minimax tiers absorb
bounded continuation, review, and bulk-session work; do not re-add FlashX (plan trial
expired 2026-10-09) or Qwen (monthly window exhausted 2026-10-19); every policy change
goes through source change + tests + `ccr-fallback-policy preflight` + `ccr-wait-idle`
gated restart.

## Non-claims

- No routing, configuration, or client-pin change was made or validated live; levers are
  proposals with evidence, not deployed outcomes.
- The review-lane client identity behind the `zai,glm-5.3` pin was not conclusively
  identified; native Claude settings, shell profiles, and `~/.agents` configs were
  checked clean, and identification is the first follow-up.
- Attempt-1 tier counts include retry-sweep inflation (documented by the worker); only
  requested-model counts and live counters are quoted as authoritative.
- MiniMax's true concurrency ceiling was not measured (429 bodies not yet parsed); the
  `maxInflight` recommendation lacks its numeric value until that probe runs.
- ZCode-plan quota entries reflect the probe moment only; no window-exhaustion forecast
  is claimed.
- Token-volume shares for the main lane come from live counters (4.5 h window), not the
  7-day log sweep; logs do not record per-model token totals.

---

# Implementation and Rollout — 2026-10-22 (same day, operator-directed)

The operator approved the assessment's levers with refinements: remove the review-lane
pin waste; Z.AI Flash is cheaper-but-not-free (still draws the shared window); the
interactive Codex lane stays on GLM-5.3; tune the delegation lane for effectiveness.

## Changes

1. **ocr-poller repin** (`RESMP-DEV/ocr-poller` master, commits `3b9a2e2` + `38b2f3e`):
   the `glm-5-3-flashx` profile — the default reviewer profile, deliberately pinned to
   `zai,glm-5.3` on 2026-10-17 after FlashX expired — now pins `zai,glm-5.3-flash`,
   the model the OCR lane's zai provider is designed to serve. Label updated; profile
   key, state, and run-id namespace unchanged. The poller re-executes per run, so the
   next review run picks it up without a daemon restart.
2. **Token-aware admission** (ccr-rust `feature/token-aware-admission`, commits
   `61f52e1` + `b1aa3bb`, PR [#77](https://github.com/RESMP-DEV/ccr-rust/pull/77)):
   new per-provider `maxInflightTokens` budget; a request defers to the next eligible
   tier when its pre-request estimate plus the route's reserved in-flight tokens would
   exceed the budget — no upstream attempt, no AIMD cut cascade. Count AIMD unchanged;
   token budget static; `ccr_tier_inflight_tokens` gauge added; estimate shares the
   audit accounting (invariant-tested). SLOC ratchet re-pinned at the new measured
   baseline. The Rust implementation was drafted by `codex-ccr-worker` run
   `20261022-191230-6zbp8vjp` under a fixed design contract in the
   `.worktrees/token-admission` worktree; the parent reviewed every hunk and
   independently re-ran all gates.
3. **Governed policy**: `MACHINE_SHARED_PROVIDERS.minimax.maxInflightTokens = 250000`
   (integrity pin `bfe649cf…` updated in the same change), chosen from the measured
  collision bounds: ~154K + ~150K in flight collided; solo ~158K served. Budget admits
   one large worker context plus a medium one.

## Verification evidence

- Gates: `cargo fmt --check` 0; `cargo clippy --all-targets -- -D warnings` 0;
  `cargo test --locked` exit 0 (613 lib tests, all integration suites, 0 failures —
  parent-run outside the worker sandbox); `python3 scripts/quality.py` 12/12 PASS;
  policy unittests 40/40; `ccr-fallback-policy check` clean before sync and `[] /
  []` (no drift, no restart required) after all restarts.
- Probe (workers lane, tiny requests): bursts of 4/6/8 concurrent all served on
  MiniMax with zero 429s — rules out request-count concurrency as the limit.
- Log analysis: minutes with 429s average 12.6 minimax dispatches vs 12.7 without —
  rules out requests-per-minute; combined with the 154K-token audits preceding 429
  bursts, the limit is token-volume shaped.
- Deployment: `cargo install --path . --force` from the feature branch; policy script
  deployed to `~/.local/share/ccr-routing/`; `sync` wrote `maxInflightTokens: 250000`
  into all four consumer configs; idle-gated restarts on 3457 (idle 0.5 s), 3456
  (idle 126.7 s), 3461 (idle 0.5 s); 3458 restarted itself onto the new binary at
  19:47 via its idle-restart daemon.
- Live token-gate validation: request A (140K tokens, MiniMax-pinned) admitted with
  `ccr_tier_inflight_tokens{minimax}=140015`; concurrent request B (140K) deferred at
  the gate (280,030 > 250,000) and served by `glm-5.3-flash`; minimax request counter
  moved only for A; zero MiniMax 429s.
- Post-restart sanity: main lane 200 on `glm-5.3` (interactive tier unchanged);
  workers lane 200 on MiniMax.

## Non-claims (implementation phase)

- The ocr-poller repin takes effect on the reviewer fleet's next poll cycle; no
  post-repin `zai,glm-5.3-flash` OCR request had been observed at receipt time
  (the four in-flight Strata PR 20 review runs started before the edit).
- The 250K budget is a first value bounded by two observed points (~158K solo OK,
  ~304K collided), not a measured MiniMax limit; it should be revisited against the
  new `ccr_tier_inflight_tokens` gauge and residual 429 rate after a few days.
- Token admission is per-listener; concurrent large prefills on two listeners can
  still combine past MiniMax's real limit (no cross-process budget).
- PR #77 is open and unreviewed at receipt time; the deployment preceded merge as an
  operator-directed operational change, with the PR as the codification path.
- Worker timeouts (58/476 runs, 12% over 7 days) were not addressed; separate concern.
- The `.worktrees/token-admission` worktree is retained until PR #77 merges.

---

# DeepSeek Utilization Diagnosis — 2026-10-23

Operator question during the Z.AI window exhaustion: whether DeepSeek had any
utilization. Read-only diagnosis; no state changed.

## Findings

1. **DeepSeek is utilized and funded.** Since the listener restarts (~2026-10-22
   19:40 local): main lane 285 successful requests / 112.0M input tokens; review
   lane 38 requests / 2.3M tokens. Balance endpoint: `total_balance $6.35,
   is_available true` (all topped-up). The workers lane shows zero DeepSeek since
   restart because MiniMax is primary there with quota remaining — expected, not a
   fault. Historical `code=402` rejections on the workers lane cluster entirely in
   the 2026-10-08/09 PAYG-exhaustion era (446 in the sampled tail); none recent.
2. **~56% of main-lane DeepSeek attempts fail one deterministic 400**: `The
   "content[].thinking" in the thinking mode must be passed back to the API.
   (invalid_request_error)`. Counters since restart: 356 provider_rejected vs 285
   successes. This is the cross-provider thinking-block hazard from the
   2026-10-21 prefill-cascade assessment, live: GLM-served sessions carry thinking
   blocks in assistant history; when the request spills mid-session to DeepSeek's
   Anthropic endpoint, the normalized history does not satisfy DeepSeek's
   thinking-mode pass-back requirement and the tier is deterministically rejected.
   Rejections skew to long histories (>=100K tokens: 155 rejected vs 388 OK;
   <100K: 17 rejected), consistent with thinking-block accumulation rather than
   size limits.
3. **What "out of tokens" was**: the Z.AI 5-hour prompt window read 0.0%
   (reset 2026-10-23 18:18 PDT; monthly window resets 2026-10-28). MiniMax was
   NOT exhausted (40% interval / 31% weekly at probe).
4. **Adjacent degradation**: the local crawl-back tiers failed 100% of attempts
   on the main lane since restart (strata-b550 2,669 and local27 2,803
   request_failed, zero successes) — the B550 tunnel and local27 server were not
   serving, so requests that exhausted MiniMax + DeepSeek died outright.

## Proposed fix (not applied)

A DeepSeek-tier request normalization (transformer or provider config) that
strips `thinking` blocks from replayed assistant history and disables thinking
mode for that tier, mirroring the existing minimax transformer precedent. This
would recover the ~56% wasted attempt volume during Z.AI window walls. Recorded
for a future reviewed change; not implemented in this diagnosis.

## Non-claims

- No DeepSeek request-shape change was tested live; the proposed fix is inferred
  from the error taxonomy, not validated.
- Pricing/burn rate of the $6.35 balance was not projected.
- The crawl-back tier outage (strata-b550/local27) was observed but not
  investigated further here.
