# Deterministic worker brief contract — 2026-10-21

## Scope

Clarified the ccr-rust worker-delegation contract so the parent orchestrator
must construct extremely scoped, mechanically executable work orders and
every worker executes exactly one order without judging whether it should be
a worker or how to decompose work. Documentation and its documentation test
only.

## Changes

- `AGENTS.md`: added the `Deterministic brief contract` section after
  `Delegation-first triage`. Every brief must be mechanically executable and
  state one bounded outcome, exact owned files or surface, exact acceptance
  commands, required report evidence, and an explicit stop condition. A brief
  that leaves scope, file discovery, or cross-piece tradeoffs to the worker
  is split further or retained by the parent; ambiguity is never delegated.
  Workers execute exactly one brief, do not decide whether they are workers,
  do not re-plan the parent task, do not decompose their assignment, and stop
  to request a fresh parent split when a brief is genuinely under-specified.
- `docs/worker-inspection.md`: added the same contract verbatim under
  `Brief contract` and tied the compact lowercase-XML brief template to it.
- `scripts/test_worker_delegation_docs.py`: added assertions for the contract
  phrases and the no-worker-decomposition rule in both guides, plus a
  cross-document equality test for the two contract paragraphs.
- `CHANGELOG.md`: recorded the operator-workflow clarification during parent
  integration.

## Validation

Parent integrated-tree validation, 2026-10-21:

- `python3 scripts/test_worker_delegation_docs.py` — exit 0; 9 tests, OK.
- `uv run --no-project python -m unittest discover -s scripts -p
  'test_ccr_fallback_*.py'` — exit 0; 36 tests, OK.
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 python3 scripts/quality.py` —
  exit 0; all 12 matrix checks passed, including Python helpers, formatting,
  lint, typecheck, Rust tests, and the vendored GP gate.

- `python3 scripts/test_worker_delegation_docs.py` — exit 0; 9 tests, OK
  (4 pre-existing, 5 new).
- `ruff format --check scripts/test_worker_delegation_docs.py` — exit 0.
- `ruff check scripts/test_worker_delegation_docs.py` — exit 0.
- `ty check scripts/test_worker_delegation_docs.py` — exit 0.
- Mutation checks against a scratch copy of the tree (repository untouched):
  removing the no-worker-decomposition clause from `AGENTS.md`, replacing the
  stop-condition phrase in the worker guide, and weakening the
  mechanical-executability clause in the worker guide each failed the suite
  with exit 1, so the new assertions bite.
- `ruff check scripts` — exit 0. `ty check scripts` — exit 0.
  `ruff format --check scripts` — exit 0 (the concurrent fallback-policy
  worker repaired its in-flight placeholder during this task).
- `python3 -m unittest discover -s scripts -p 'test_*.py'` — worker-sandbox
  exit 1 from 61 environmental permission errors while the routing worker was
  concurrently in flight. The focused delegation suite passed then and is
  rerun by the parent on the integrated tree below.
- `git diff --check` — exit 0.
- `sindexer update /Users/kearm/ccr-rust` — exited 0 with an index error:
  the Milvus endpoint `127.0.0.1:19530` refused the connection. The stack
  was left unconfigured per the no-reindex rule; no durable index update
  occurred.
- Parent `sindexer update /Users/kearm/ccr-rust` was interrupted after the
  local embeddings endpoint at `127.0.0.1:1236` repeatedly failed.
  `sindexer-doctor` reported `status: broken`, Milvus healthy, the embed
  agent process present, and `embed 1236: false`; the stack was left
  unreconfigured.

## Non-claims

- Documentation change only: no launcher code, router code, fallback policy,
  or live behavior was changed. No listener or worker process was started,
  restarted, or probed.
- Green documentation tests do not prove any model obeys the contract; a
  later trace audit is needed to measure parent brief quality and worker
  behavior against it.
- Worker-time failures caused solely by filesystem or socket sandbox
  restrictions are not treated as behavior results; the parent reruns the
  integrated checks outside that sandbox.
