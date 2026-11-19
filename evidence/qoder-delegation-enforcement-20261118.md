# Qoder delegation enforcement, 2026-11-18

## Claim

Recent under-use was not a Qoder launcher failure. The launcher was already
reaching Qoder for most eligible noninteractive workers. The remaining leaks
were interactive-parent policy and tooling: parents could classify discovery,
verification, and evidence collection as parent work, and Codex exposed native
`create_thread` child tasks that stayed on GLM-5.3 instead of invoking
`codex-ccr-worker`. This change closes both paths and makes the remaining
launcher outcomes visible.

## Measured diagnosis

The initial three-day receipt snapshot showed 43 launcher runs: 39 completed,
29 with a Qoder attempt, 10 `network_authorized` bypasses, one
`write_resume_unsupported`, and three legacy/not-recorded receipts. At that
point the apparent problem was therefore not “Qoder unavailable”; most eligible
offline runs reached it.

The new metadata-only parent audit examined the subsequent three-day Codex
rollout window. It selected 12 `codex-tui` rollouts: seven root parents and
five native children. One root parent issued five `create_thread` calls and
zero `codex-ccr-worker` launches. Its three initial child rollouts were each
over 1.1 MB, recorded `model=glm-5.3` and provider `claude-code-router`, and
therefore bypassed Qoder entirely. Across native-child rollouts the audit
observed 172 tool calls. Only one root parent had no launcher-worker launch.

The clearest example was
`~/.codex/sessions/2026/11/17/rollout-2026-11-17T20-42-27-01a1ecf6-9d50-7d62-a642-5298c512cf6f.jsonl`.
It wrote a ledger but used `create_thread` for three audits and two conflict
reviews instead of `codex-ccr-worker`. This proved that the old ledger alone
did not pin the execution lane.

After the repairs, a three-day receipt sample contained 48 Qoder attempts:
47 served by `engine: qoder` and one fallback, satisfying
`attempted = served + fell_back`. Eleven runs still had deliberate
`network_authorized` bypasses; those remain shell-capable/network worker work,
not parent work.

## Changes

### Interactive policy and Codex tool surface

- `~/AGENTS.md` now makes path/symbol/entry-point discovery a bounded worker
  brief, closes the parent-only list, requires a fresh ledger on every
  actionable turn, and bans native `create_thread`/fork delegation.
- `agent-instructions-sync --sync` propagated the policy to Codex, ZCode, and
  Qoder instruction surfaces; the follow-up parity check passed.
- `/Users/kearm/.codex/config.toml` and its synced copy deliberately install a
  disabled `[mcp_servers.codex_tui]` namespace guard. This forces Codex to the
  six non-delegation dynamic tools while retaining shell and
  `codex-ccr-worker`. `codex mcp list` reports `codex_tui ... disabled`.
- The repository guides and enforcement tests mirror the closed contract and
  explicitly require `--no-qoder-primary` only for command-bearing worker
  briefs that Qoder cannot execute.

### Launcher reliability

`RESMP-DEV/ccr-worker-launcher` commit `14ea248` was pushed and installed:

- A resumed `modelUsage` envelope containing `dfmodel` plus a stale key is
  accepted; stale-only envelopes remain `wrong_model`.
- A read-only Qoder launch `OSError` becomes a recorded failed attempt and
  falls back to the governed CCR worker chain.
- A slot/cooldown timeout before launch records
  `slot_or_cooldown_timeout` without claiming a Qoder attempt.

Source and installed `qoder_lane.py` and `worker.py` had matching SHA-256
hashes after installation. Fresh installed-wrapper and resume probes both
completed on `qoder,dfmodel`; the resume recalled the exact prior string.

### ccr-rust observability

- `ccr-worker-usage --parent-sessions` now reports root/native-child session
  counts, launcher and `create_thread` launches, tool/shell totals, ledger
  messages, and per-session `native_child` state without storing or printing
  prompt text.
- Receipt aggregation now reports Qoder `served`, `fell_back`, actual
  `outcome:<value>` counters, bypass reasons, `eligible_not_attempted`, and
  legacy `not_recorded`.
- `ccr-rust workers list/show` expose engine, served route, model identity,
  Qoder request/bypass state, and flattened attempt outcome in JSON and a
  compact human `Qoder:` line.

## Worker and parent evidence

Qoder served the following bounded lanes, among others:

- `20261118-165615-r5f0h0ek` and `20261118-165623-69w6fhdr`: usage and gate
  audits.
- `20261118-165632-hwjgc3hn`: instruction-surface audit.
- `20261118-170301-yh4zcrpb`: launcher patch map.
- `20261118-170342-2sapbr__`: delegation-contract write.
- `20261118-170616-0qoe507p`: launcher reliability write.
- `20261118-170813-dsqjljyb`: usage telemetry write.
- `20261118-171105-m2l0zxg5`: Codex source/config audit.
- `20261118-171816-nhbs0_1a`: real-receipt classification repair.
- `20261118-172056-iv20_o2r`: root/native-child reporting polish.
- `20261118-172432-ohicjvte`: worker-inspector field write.
- `20261118-173420-2wc4e9o4`: focused Ruff repair.

The parent independently inspected each final receipt/diff, replayed the named
tests, performed the cross-repo clean port, and wrote this consolidated
receipt. Worker prose is evidence, not the acceptance record.

## Validation

Launcher repository, commit `14ea248`:

- `uv run --frozen --no-sync pytest -q`: 59 passed.
- `uv run --frozen --no-sync ruff check .`: passed.
- `git diff --check`: passed.
- Installed-wrapper fresh Qoder probe: completed, exit 0,
  `served_route: qoder,dfmodel`.
- Installed-wrapper resume probe: completed, exit 0, recalled prior context,
  `served_route: qoder,dfmodel`.

Clean ccr-rust port:

- `cargo fmt --check`: passed.
- `cargo test --locked --lib workers::`: 41 passed.
- `cargo test --locked --test integration_worker_inspector`: 4 passed.
- `cargo clippy --all-targets --locked -- -D warnings`: passed.
- `cargo test --all-features --locked`: passed.
- `ruff check` and `ruff format --check` on the three changed Python files:
  passed.
- `python3 scripts/test_worker_delegation_docs.py`: 15 passed in the clean
  port.
- `PYTHONPATH=scripts python3 -m unittest scripts.test_ccr_worker_usage`:
  14 passed.
- `python3 scripts/quality.py`: format, lint, typecheck, shellcheck, clippy,
  Rust tests, no-default-features, debug binary, and vendored GP routing all
  passed. The run exited 1 only at the pre-existing SLOC ratchet and the
  helper suite’s same SLOC assertion.

SLOC control: untouched `origin/main` at `c38d5f1` reports 42,235/40,969 and
already fails both total and `router/mod.rs` ceilings. The clean task port
reports 42,398/40,969, a 163-line task increase. No unrelated ratchet re-pin
was mixed into this change.

## Non-claims

- Parent-session counts prove local tool-call metadata, not delegation quality,
  task correctness, or served upstream routing.
- `network_authorized` runs intentionally do not attempt Qoder; they are worker
  lanes, and this change does not grant Qoder shell or network access.
- The disabled `codex_tui` guard affects newly started Codex sessions. The
  already-running parent session may retain its original tool advertisement;
  the live `codex mcp list` probe and Codex source test prove the configured
  new-session path, not a post-guard interactive `create_thread` attempt.
- The SLOC ratchet remains broken on `origin/main` before this change. This
  receipt does not claim the repository-wide SLOC gate is green.
