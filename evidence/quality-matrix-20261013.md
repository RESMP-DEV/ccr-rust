# Offline quality matrix, 2026-10-13

## Claim

The repository now has a deterministic offline quality dispatcher, property and
SSE boundary tests, opt-in coverage and mutation modes, and a locked vendored
GP-routing test boundary.

Implementation commit: `ddf4eb2` on `feat/quality-matrix`, based on `df4c0017`.

## Evidence

All commands ran in
`/Users/kearm/ccr-rust/.worktrees/pr-quality-matrix` on the exact split tree.

- `python3 scripts/quality.py` — exit 0. Formatting, strict Clippy,
  all-features Rust tests, the no-default-features boundary, Python helper
  tests, and vendored GP tests all passed.
- `python3 scripts/quality.py --coverage` — exit 0. LLVM source coverage was
  78.78% lines (21,793/27,662), 77.16% functions (2,010/2,605), and 78.73%
  regions (34,331/43,606), above the 75% line floor.
- `python3 -m unittest discover -s scripts -p 'test_quality_matrix.py' -v` —
  exit 0, 6 tests passed.
- `ruff check` and `ruff format --check` on the dispatcher tests and script —
  exit 0.
- No `default_*.profraw` files remained at the worktree root after coverage.

The ignored machine receipt is written to
`build/reports/quality/receipt.json`. This tracked file is the durable receipt.

## Review repair round

After PR review, the dispatcher received focused hardening:

- `--list --coverage` no longer creates the coverage report directory.
- A new coverage run removes a stale HTML index before starting, so a failed
  later run cannot reuse old percentages in its receipt.
- Coverage totals are parsed from the Function/Line/Region header labels, not
  by assuming positional order.
- A requested but unavailable or malformed coverage summary is recorded with a
  `coverage_summary_error` rather than silently omitted.
- `--coverage --mutation` composes, while `--fast` remains exclusive with
  `--coverage` and `--mutation`; a mutation-only diagnostic extends the default
  matrix.
- Every check start failure is recorded in the log and result and the remaining
  checks still run.
- Git metadata failures no longer prevent the receipt from being written.
- The receipt agent is configurable through `QUALITY_AGENT` and defaults to
  `Codex`.
- Property generation now explicitly constructs multiline SSE data.
- The backoff property uses a ceiling at or above the base so its
  non-decreasing assertion is meaningful, with a separate test pinning the
  ceiling-below-base behavior.

The repaired tree passed the same default matrix and coverage gate. Coverage
was unchanged at 78.78% lines, 77.16% functions, and 78.73% regions. Focused
regressions were 14 Python tests and 6 Rust property/boundary tests, all green.
No LLVM profiler artifacts remained at the worktree root.
The final incremental CLI edge rejected `--fast --mutation`, bringing the
focused Python regression count to 15; all 15 passed.

Review suggestions for moving the vendor lockfile out of the tree or adding a
manifest MSRV field were not applied: the lockfile is deliberately tracked and
documented, while the dependency floor is already recorded in the changelog.
The reviewer claim that profiles only reside in the coverage target directory
was contradicted by the observed root artifacts removed by the original run.

## Non-claims

- The matrix is offline and makes no live provider request.
- Mutation testing is diagnostic, not a routine merge gate.
- A full `src/sse.rs` mutation diagnostic previously produced 49 mutants:
  22 caught, 14 missed, 11 timed out, and 2 were unviable. Survivors show room
  for stronger tests; they do not invalidate the passing deterministic matrix.
- This change does not alter listener restart behavior.
