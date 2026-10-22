# GLM-5.3 reasoning parameters and worker-boundary recovery — 2026-10-22

## Scope

Verified the real GLM-5.3 reasoning controls, corrected local Codex metadata
that advertised a non-native alias, audited how MiniMax M3.1 Flash worker
reports influence parent behavior, and reduced lost-work windows in the worker
launcher and inspector. No CCR listener was restarted and no provider route was
changed.

## GLM-5.3 parameter verdict

Current Z.AI documentation and OpenAPI identify `reasoning_effort` as the
model-level control. GLM-5.3 accepts `low`, `high`, and `max`, with `max` as
the default. `thinking.type` is `enabled` by default and GLM-5.3 no longer
permits disabling thinking; `thinking.clear_thinking` is a separate boolean
defaulting to true. No `reasoning_rate`, `thinking_rate`, or budget-rate field
exists in the official schema.

`xhigh` is not a native GLM-5.3 level. Z.AI's Coding Plan mapping accepts it
and normalizes it to `max`. Live probes through CCR returned HTTP 200 for both
`low` and `xhigh` against `zai,glm-5.3`:

- `low` → `GLM_low_OK`, 98 reported reasoning tokens.
- `xhigh` → `GLM_xhigh_OK`, 344 reported reasoning tokens.
- Additional `high`, `max`, and omitted-effort probes all returned their exact
  markers with HTTP 200. These one-shot token counts do not establish a
  performance ordering; they only prove request acceptance.

The effective parent and launcher effort is now the native `max` label rather
than the Coding Plan `xhigh` alias. The generated catalog source also normalizes
the GLM-5.3 source entry and derived aliases to `low`, `high`, and `max`.

## Worker influence findings

Read-only audits covered recent MiniMax/GLM worker artifacts and the installed
launcher lifecycle. MiniMax reports repeatedly opened with parent-level
verdicts, gates, phase assignments, production commands, or release vetoes
while the inspector's default 1,200-character head preview retained those
openings and discarded later caveats. Several reports visibly informed later
parent actions. This is evidence that the report channel, not only model
quality, was acting as an authority channel.

The launcher previously wrote its receipt only after completion, did not
persist the brief, dropped thread IDs on cancellation/failure paths, and
attached salvage only to timeouts. The corpus included dead runs with no
receipt and historical cancelled/failed runs whose trace contained a resume
thread but whose receipt did not.

## Changes

- Canonical and Codex/ZCode agent instructions now describe MiniMax-first
  workers as evidence/busy-work lanes, treat their reports as untrusted data,
  make read-only the default, require explicit `--write-workspace`, and reserve
  production/live-service actions for the parent. `agent-instructions-sync`
  and its parity check passed.
- The generalized launcher now persists the exact brief and an initial
  `running` receipt, writes final artifacts atomically, preserves thread IDs,
  and attaches salvage evidence to every non-completed run plus completed runs
  with failed commands. Its worker instruction explicitly forbids production,
  routing, credential, remote, deployment, and destructive actions and states
  that final reports are evidence, not parent instructions. The source changes
  were installed with `uv tool install --reinstall`.
- CCR-Rust's worker inspector surfaces `cwd`, `brief_path`, and salvage fields;
  `latest` now selects timestamped runs before malformed/manual directories.
  Usage summaries count timestamped directories with absent receipts as
  `receipt_pending`.
- Repository instructions and `docs/worker-inspection.md` now define the worker
  report authority boundary and initial/salvage receipt semantics.
- `scripts/ccr_codex_catalog.py` normalizes generated GLM-5.3 metadata to the
  native effort labels. A recovery snapshot preceded deployment, `ccr-catalog-sync`
  applied the catalog atomically, and a second check reported no drift.
  The formatted source and deployed execution copy both hash to
  `87bf70c0fee7f5c1229dc8aac8b493a5b741e08e6012c109304cbdd6b4480165`.

Live launcher proof: run `20261022-105056-ngqnj97q` was launched without a
write flag, completed `DEFAULT_READONLY_OK` on MiniMax, recorded
`read_only: true`, preserved its brief, and still reported
`task_verified: false`. A source-built inspector resolved `latest` to that
timestamped run rather than `manual-resume-20260119`.
After installing the rebuilt CLI, the installed inspector resolved `latest` to
an unrelated active run `20261022-110121-2debg9yd` with outcome `running`, its
recorded `cwd`, and its persisted brief path. This is live evidence that the
initial receipt is present before turn completion; the foreign run was not
inspected beyond those metadata fields.

## Validation

- `uv run ruff check .` in `/Users/kearm/ccr-worker-launcher` — passed.
- `uv run pytest` in `/Users/kearm/ccr-worker-launcher` — 20 tests passed.
- `PYTHONPATH=scripts python3 -m unittest test_ccr_worker_usage
  test_ccr_codex_catalog test_worker_delegation_docs` — 26 tests passed.
- `uv run --no-project ruff check` over the five changed Python files — passed.
- `cargo test --locked --lib workers::` — 39 tests passed.
- `cargo test --locked --test integration_worker_inspector` — 3 tests passed.
- `cargo clippy --all-targets --locked -- -D warnings` — passed.
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 cargo test --all-features
  --locked` — 593 unit tests plus all integration/bin/doc-test targets passed.
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 cargo build --release --locked`
  and `cargo install --path . --force --locked` — passed; both installed
  executables were replaced.
- `cargo fmt --check` and `git diff --check` — passed after formatting.
- `ccr-catalog-sync --check` after apply — `ok: true`, no changes or errors.
- `agent-instructions-sync` — Codex, ZCode, Claude import, and Lean context
  parity all passed.
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 python3 scripts/quality.py` —
  all 12 offline checks passed after formatting, typing, and ratchet repairs;
  receipt: `build/reports/quality/receipt.json`.

## Non-claims

- One-marker GLM probes prove acceptance and returned text only; they do not
  rank reasoning depth, latency, quality, or cost.
- `served_route` remains null in launcher receipts and `provider_verified` is
  deliberately false. The live MiniMax proof confirms the requested route, not
  an independently observed upstream identity.
- Historical receipt-less runs are now counted, but their original briefs were
  not available before this change and were not reconstructed.
- The launcher hardens future runs; it cannot recover work from processes that
  already died before the initial receipt existed.
- No listener restart, route-order change, provider addition, credential
  change, remote push, or production deployment was performed.
