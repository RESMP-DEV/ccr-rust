# Retry-budget failover repair and terminal-429 provenance, 2026-10-19

## What was wrong

Two independent defects produced the client-visible
`exceeded retry limit, last status: 429 Too Many Requests` even though a
healthy fallback tier was available.

1. **The process-wide retry budget was charged for the first attempt on every
   fallback tier.** `src/router/mod.rs` acquired a retry permit whenever
   `total_attempts > 0`, which is true for the first attempt on the second
   tier and beyond. When concurrent traffic saturated the budget, the router
   executed `break 'tier_pass` and abandoned the whole cascade before a later
   tier was attempted. The sweep's failure mix still said "rate limited", so
   the client received a synthesized 429. Live evidence: at
   `2026-10-20T01:56:11.931Z` the main listener emitted a Qwen pre-request
   token audit and then terminated without a `Trying qwen...` line, while
   MiniMax succeeded on another request 1.4 seconds later.
2. **The terminal 429 named only the last rate-limited tier.** The response
   body said "All candidate backend tiers are currently rate limited" with no
   indication of which candidates were exhausted or why, so an operator could
   not tell an exhausted quota from a suppressed failover.

A third factor amplified both: the Qwen token-plan lane was in every governed
chain while its monthly utilization window was exhausted, so each request paid
a guaranteed-refusal round trip before reaching a tier that worked.

## What changed

- `src/router/mod.rs`: the retry budget is now charged only for a retry of a
  tier that already attempted (`attempt > 0`). Budget exhaustion stops
  retrying that tier, lets the remaining tiers make their normal first
  attempt, and then stops additional retry sweeps. Tail-latency hedges stay
  budget-controlled because they deliberately add a parallel upstream
  attempt.
- `src/router/mod.rs`: the synthesized terminal 429 now carries an
  `error.details` array with each rate-limited full `provider,model` route
  and its retry hint, plus an `x-ccr-rate-limited-tiers` header.
  `x-ccr-tier` keeps dispatch's configured `tier_name` display-name
  contract.
- `docs/configuration.md`, `docs/failover_architecture.md`, `CHANGELOG.md`:
  the retry-budget contract and the terminal-response shape are documented.
- The Qwen lane was removed from every governed chain earlier the same day
  (receipt: `evidence/qwen-removal-20261019.md`).

## Regressions added

All in `tests/test_failover_controls.rs`, against local mock upstreams only. The focused file now contains 12 tests.

- `exhausted_retry_budget_still_fails_over_to_the_next_tier`: a pre-saturated
  budget still reaches a healthy second tier, and the request succeeds.
- `exhausted_retry_budget_completes_the_cascade_without_extra_sweeps`: after
  exhaustion the cascade completes once across both tiers with no extra
  sweeps, so the exemption cannot become unbounded amplification.
- `terminal_rate_limit_response_reports_tier_provenance`: the terminal 429
  names both rate-limited routes in the body and the header, `x-ccr-tier`
  carries the last rate-limited route's configured display name, and a
  second request issued while both routes are in local backoff reports the
  same routes with non-null retry hints without any further upstream hits.
- `terminal_rate_limit_keeps_the_configured_tier_display_name`: a provider
  with a custom `tier_name` keeps that value in `x-ccr-tier`, which a
  provider-prefix implementation would break.
- `all_admission_deferred_requests_re_sweep_until_permit_release`: when one
  request holds the only admission permit, a second request stays held and
  succeeds after the permit releases. This disproves the review worker's
  concern that a pure all-deferred sweep terminates immediately.

## Verification actually run

- `cargo test --all-features --locked --test test_failover_controls`: 12 passed.
- `cargo test --all-features --locked --test integration_responses
  test_responses_stream_preserves_exhausted_tier_rate_limit`: passed. The full
  suite caught a compatibility break in the legacy `x-ccr-tier` header, which
  was fixed by keeping the provider name in that header.
- `python3 scripts/quality.py`: all six checks passed (format, strict Clippy,
  all-features Rust tests, no-default-features check, Python helper tests,
  vendored GP routing). Receipt: `build/reports/quality/receipt.json`,
  `aggregate_passed: true`, source revision `bfb525ef37b3f6feb6f06b98b2ccf13a2b019d3a`
  with a dirty tree.
- Independent read-only review worker: `build/reports/reviews/retry-budget-provenance.md`.
  It found no blocking issue and raised the sweep-amplification concern that
  the second regression above now covers.
- Configured review services on PR #65. Kilo Code Review reported no issues
  and recommended merge. CodeRabbit raised three minor findings, all accepted
  and fixed: the 503 description overstated its contents, this receipt's test
  count and `x-ccr-tier` claim did not match the test, and the terminal
  response had replaced the configured `tier_name` display name with the
  provider prefix, which would break any provider with a custom name. The
  display-name regression is now pinned by a dedicated test.

## Final quality receipt

The final run completed at `2026-10-20T03:28:26.706402+00:00` with `aggregate_passed: true` after the all-admission regression was added.

## Deployment

- Pre-change snapshot `20261019T201543-0700`, label `pre-retry-budget-fix`,
  42 files, verify clean.
- `cargo install --path . --force` completed; `~/.cargo/bin/ccr-rust` and
  `target/release/ccr-rust` share SHA-256
  `c0af23abd0dab103f66f19697d6eb73d52d4121033367a47eca5e1ee19c104fa`.
- Auth listener (3461) passed its idle gate and restarted on the new binary.
  `/health?details=1` reports
  `["zai,glm-5.3","zai,glm-5.3-flash","minimax,MiniMax-M3.1-Flash-Preview"]`.
- Main (3456), worker (3457), and OCR (3458) had live requests, so each has a
  detached idle-gated activation running rather than an interrupted turn.
  Worker uses a 3700-second window to outlast its running deep worker task.
  Logs: `~/.claude-code-router/logs/retry-fix-activate-{main,worker,ocr}.log`.
  Current activator PIDs: main `64494` (7200-second window), worker `51338`
  (3700-second window), OCR `51339` (900-second window). A detached monitor
  appends completion evidence to
  `~/.claude-code-router/logs/retry-fix-activation-receipt.log`.

## Relationship to the published tree

This work was originally developed in `/Users/kearm/ccr-rust`, which was 7
commits behind `origin/main`. Those commits already contained the recovery
tool, the doctor probe, and the Qwen token-plan lane (PRs #58 through #64),
so the local untracked `scripts/ccr_recovery.py` was a pre-merge draft and the
locally deployed `ccr-recovery` helper is in fact byte-identical to
`origin/main`. The branch `fix/retry-budget-failover` was therefore rebuilt
from `origin/main` and carries only the retry-budget and provenance change,
without the stale health-handler regression the local checkout would have
introduced.

## Open findings not fixed here

- **Admission-denied tiers are absent from terminal-429 provenance.** A tier
  refused by `AdmissionTracker::acquire` is not recorded as rate-limited, so
  the details array can under-report candidates. Diagnostic completeness only,
  not a routing regression.
- **A reviewer concern about purely admission-deferred sweeps was tested and
  disproved.** `execute_attempt` returns `None` without changing the shared
  failure flags, but the caller marks the attempt retryable before continuing;
  the new all-deferred regression holds the second request open and succeeds
  after the first permit releases.
- **The stale local recovery draft must not be deployed.** Running this
  branch's test suite against the live module fails 15 of 39 tests, because
  the live helper verifies symlinked local-bin targets by content while the
  local draft does not. The live helper is the correct one; the stale local
  copy should be reconciled from `origin/main` rather than deployed.
- **Account-scoped cross-listener admission and backoff state does not
  exist.** Each listener still learns MiniMax limits independently while
  sharing one credential, so the aggregate ceiling that produces upstream
  429s is still unmeasured. The Qwen cordon and the retry-budget repair
  reduce how often a request reaches an exhausted tier, but they do not
  change the shared-account concurrency picture.
