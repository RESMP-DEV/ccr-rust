# Branch homologation and Qoder lane probe, 2026-10-25

Agent: ZCode (GLM-5.3-Flash parent), session-initiated maintenance. Branch
deletions were justified per-branch before execution: 11 branches by direct
ancestor-of-origin/main proof, 8 by tip preservation on an origin branch.

## Qoder sub-agent delegation probe

Fresh, offline, read-only `codex-ccr-worker` brief against this checkout,
quick profile, `--output build/probes/qoder-20261025/answer.txt`.

- Receipt `build/probes/qoder-20261025/answer.txt.status.json`: `outcome:
  completed`, `engine: qoder`, `served_route: qoder,dfmodel`,
  `model_identity: DeepSeek-V4.1-Flash-1017`, `qoder_model_usage_keys:
  ["dfmodel"]`, request ID `414185fe-d48b-4050-a870-29296b530f41`, credits 0,
  elapsed 7.386 s, `read_only: true`, `network_allowed: false`. The
  MiniMax-first chain was listed as fallback and unused.
- Parent acceptance grep confirmed all three reported facts: primary struct
  `RateLimitTracker` (src/ratelimit.rs:45), first/last `pub fn` `new` (:50) /
  `restore_rate_limit_backoff_counter` (:242), 9 `pub fn` declarations.
- Verdict: the Qoder primary worker lane works end-to-end today; a clean
  result bypassed the Responses listener as designed. Non-claims: Qoder
  zeroes token counts, so reasoning depth at `max` remains unmeasured; the
  probe exercises the read-only surface only.

## Branch/worktree homologation

Start state: 23 local branches plus a detached-HEAD primary at 259b452
(10 behind origin/main), 20 registered non-primary worktrees plus 1 prunable
registration, 53 `refs/remotes/pull/*` refs, local `main` 17 behind.

Classification evidence: `git branch --merged/--no-merged origin/main`,
`git cherry origin/main <branch>` per unmerged branch, and
`git branch -r --contains <tip>` for preservation. `arbiter scan --check`
ran before destructive steps; its single error was the five live zcode
sessions sharing this primary checkout (this session family), not a blocker.

Deleted (19 branches): 11 merged into origin/main
(chore/sloc-rerpin-79, docs/deepseek-fix-receipt, feat/shared-fallback-policy,
feat/tool-call-efficiency, feature/deepseek-thinking,
feature/token-aware-admission, fix/deepseek-space-placeholder,
fix/starved-429-escalation, fix/zai-quota-cordon,
repair/tool-telemetry-review, shadow-repaired; the last two needed `-D`
because their upstreams are gone despite being merged), and 8 whose tips are
preserved on origin refs (docs/minimax-operator-guide and work/minimax-m3-compat
in sync with their origin branches; feat/failover-v2 and the three
repair/pr48-* names on the commit 163cfb6 inside origin/feat/failover-v2
history; test/four-tier-fallback in sync; work/codex-anthropic-continuation on
origin/work/codex-anthropic-continuation).

Kept (4 + main): feat/local27-final-fallback, feat/test-quality-idle-restart,
feature/qwen27b-nvfp4-fallback, feature/transformer-plugins. Each tip is
reachable from no origin branch and not patch-equivalent to origin/main, so
deleting any of them would lose the only copy. `main` was fast-forwarded
c04d637 -> 3c0c53a (origin/main tip).

Worktrees: removed 16 and pruned 1 broken registration
(`~/.cache/ccr-rust-repair.mbGhN7`); kept the three worktrees backing the
kept branches. Four removed worktrees carried uncommitted source edits that
exist nowhere else; their diffs were preserved first to
`build/worktree-rescue-20261025/{deepseek-thinking(41 lines, base 9bcf507),
pr48-admission-retry(400), pr48-router-hedging(372),
pr48-stickiness-routing(212) (base 163cfb6)}.patch`, then removal used
`git worktree remove --force`. All four `~/.cache` ccr worktrees had heads
contained in origin/main and were clean.

Refs: all 53 `refs/remotes/pull/N` remote-tracking refs deleted via
`git update-ref --stdin`; GitHub retains PR head refs server-side, so the
local mirrors were stale duplicates.

End state: refs/heads = main (3c0c53a) + the 4 kept branches; worktrees =
primary + 3 kept lines; 0 pull refs; origin untouched (no remote branch was
deleted).

## Non-claims

- Rescue patches capture uncommitted diffs, not full trees; bases 9bcf507 and
  163cfb6 remain recoverable from origin branches.
- The four kept branches are unqualified work lines; nothing here verifies
  they build or are still wanted. Deleting them later is a separate decision.
- The primary checkout's detached HEAD and 10 dirty paths were left exactly
  as found; that in-flight work is unaffected.
- No CCR listener, routing policy, or installed binary was touched.
