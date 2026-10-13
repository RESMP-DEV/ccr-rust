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

## Non-claims

- The matrix is offline and makes no live provider request.
- Mutation testing is diagnostic, not a routine merge gate.
- A full `src/sse.rs` mutation diagnostic previously produced 49 mutants:
  22 caught, 14 missed, 11 timed out, and 2 were unviable. Survivors show room
  for stronger tests; they do not invalidate the passing deterministic matrix.
- This change does not alter listener restart behavior.
