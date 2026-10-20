# GitHub reconcile and exhausted-lane removal, 2026-10-19

## What was out of date

The primary checkout `/Users/kearm/ccr-rust` was 7 commits behind
`origin/main`, all merged PRs: #58 through #64. Those commits already carried
the recovery tool, the doctor probe, the Qwen token-plan lane, and the
Claude Code native-pathway note. The checkout's untracked copies of those
scripts were therefore pre-merge drafts, and its `src/router/mod.rs` still
carried the pre-#62 `/health` handler.

The live deployment was not behind: `~/.local/share/ccr-routing/ccr_recovery.py`
and both symlinked paths are byte-identical to `origin/main`
(`1cc72348f69db4de`). The recovery tool was already correctly deployed; only
the checkout was stale.

## What was done

- Preserved all 24 dirty and untracked primary-checkout files, with SHA-256
  manifest, to
  `~/.local/share/ccr-recovery/pre-reconcile-20261019/`, then stashed and
  fast-forwarded `main` to `origin/main`. The stash
  (`pre-reconcile-20261019 dirty checkout superseded by origin/main PRs
  #58-#64`) is retained.
- Rebuilt the work as `fix/retry-budget-failover` from `origin/main` so the
  branch carries only new work, not a duplicate of the merged recovery work.
- Found and fixed a split-brain: the live policy body was Qwen-free but the
  upstream source pin would have refused it, because the live body had been
  written by the stale draft, which also had dropped Qwen's `maxtoken`
  transformer and the `deepseek-v4.1-flash-1017` model. Source is now the
  reviewed authority, and the live body is derived from it.

## Utilization and what cannot be optimized

`modelctl usage` tracks only `minimax` among the routed providers. Live read:

- MiniMax: 5-hour window 63 percent remaining, weekly 96 percent remaining.
- Z.AI and Qwen: not tracked by `modelctl`; Z.AI is observed through router
  429s and the 5-hour prompt window.

The user stated Qwen is out for the month. That is consistent with the live
evidence: every Qwen route returned 429 with a reset hint measured in weeks,
while MiniMax succeeded repeatedly on other listeners in the same interval.
An exhausted monthly window is not something routing can tune around; the only
correct response is to stop dispatching to it. Qwen is now registered but
unrouted, so it is skipped rather than retried.

Z.AI is not exhausted. Its refusals are a rolling 5-hour prompt window and
concurrency caps, and it keeps completing GLM calls, so it stays in the chain.

## Policy change

Chain after removal, identical in source, policy body, and all four consumer
configs:

- main/auth: `zai,glm-5.3`, `zai,glm-5.3-flash`, `minimax,MiniMax-M3.1-Flash`
- worker: `zai,glm-5.3-flash`, `minimax,MiniMax-M3.1-Flash`
- OCR: `openrouter,nvidia/nemotron-3-ultra-550b-a55b:free`,
  `zai,glm-5.3-flash`, `minimax,MiniMax-M3.1-Flash`

Qwen, GMI, and DeepSeek are `registered_not_routed`. Preflight against the
reviewed provider set returned HTTP 200 for both routed providers and did not
probe the dormant ones.

Note on the MiniMax ID: the running listeners served
`minimax,MiniMax-M3.1-Flash-Preview` from the draft policy, while upstream's
reviewed source pins `minimax,MiniMax-M3.1-Flash`. The reviewed ID is now
used in source and in the generated configs; the machine instructions state
CCR canonicalizes the legacy preview alias on dispatch, so this is an ID
normalization rather than a model change. Both IDs still carry the required
`minimax` transformer.

## Verification actually run

- `python3 scripts/quality.py` on the branch: all six checks passed.
- Policy tests: 34 passed, including updated contracts for the new chain, the
  empty consumer seat map, and Qwen's registered-but-unrouted status.
- Machine gate: `load_policy` accepts the derived body against the recomputed
  `MACHINE_POLICY_INTEGRITY_SHA256`
  `4f72219c40a00a7d3e304a01cccfe87ffcbe8d5390dbaf84eb34a07d879f2d6b`.
- `ccr-fallback-policy check --json` after sync: no drift, no stale staging.
- Source, deployed module, and symlinked copy all hash to `3fcedb0a84e590ac`.
- Snapshot `20261019T210445-0700`, label `pre-qwen-source-policy-deploy`,
  verified clean, taken before the live policy write.
- Auth (3461) and worker (3457) passed idle gates and restarted on the new
  configs. Main (3456, PID 32148, 3600-second window) and OCR (3458, PID
  32149, 900-second window) have detached idle-gated activations running.

## Pull request

PR #65, `fix/retry-budget-failover`, head `bb3bcca62def512c4dcae38af70bc2014447a6d7`.
Two commits: the retry-budget and provenance fix, then the exhausted-lane
policy removal. Mergeable; review state is tracked in the PR.

## Non-claims

- `modelctl` has no Qwen or Z.AI provider entry, so their utilization is
  inferred from router-side 429 evidence and the user's report, not from a
  usage API.
- Account-scoped cross-listener admission and backoff state still does not
  exist, so MiniMax aggregate capacity across listeners is unmeasured.
- The 5-hour Z.AI window and MiniMax concurrency caps are unchanged by this
  work; only the exhausted Qwen lane was removed.
