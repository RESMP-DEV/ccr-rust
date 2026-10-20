# Bounded worker delegation default — 2026-10-20

## Scope

Made bounded GLM-first delegation a testable parent-agent default, corrected
the stale active worker-chain summary, documented the launch/brief contract,
and added a read-only adoption summary. No router policy, provider credential,
listener configuration, or concurrency ceiling was changed here. The canonical
launcher was repaired through RESMP-DEV/ccr-worker-launcher#5 and #6, merged,
installed, and made the live `codex-ccr-worker` entrypoint.

## Changes

- Canonical global instructions (`~/AGENTS.md`) now tell parent agents to
  decompose nontrivial implementation, review, research, and diagnostic work
  into independently reportable worker orders before interactive exploration.
  The default trigger is broad search/audit, multiple files, modules, or
  repositories, isolated implementation/test diagnosis, or roughly five
  parent tool calls/five minutes. The parent exception list covers dialogue,
  decomposition/synthesis, tiny brief-preparation lookups, irreversible or
  credential-facing work, live-service coordination, parent-only tools, and
  final acceptance.
- Corrected the global active chain summary to the governed source/live state:
  GLM-5.3 Flash first, then MiniMax M3.1 Flash. The prior global paragraph
  still described three now-unrouted Qwen tiers as active.
- `agent-instructions-sync --sync` propagated the core block to Codex and
  ZCode. A pre-change copy is in
  `~/.cache/alphaheng/instruction-backups/delegation-default-20261020/`.
- `AGENTS.md` and `docs/worker-inspection.md` now carry the same repository
  triage rule, a compact XML worker-brief contract, and the foreground-session
  pattern for independent workers. `CHANGELOG.md` records the operator-workflow
  change.
- Added `scripts/ccr_worker_usage.py`, installed the local
  `ccr-worker-usage` wrapper, and cataloged it in the toolbelt. It reports
  listener active/tier request metrics plus local receipt outcomes, timeout
  profiles, permission choices, requested routes, and elapsed-time totals.
- RESMP-DEV/ccr-worker-launcher#5 (merge `c9ae7a3`) adds the canonical
  quick/review/deep timeout profiles and receipt fields, caps
  `--max-workers` at 12, validates output paths before work, repairs two
  unreachable tests, and replaces the README placeholder with the launch
  contract.
- RESMP-DEV/ccr-worker-launcher#6 (merge `a39dc9b`) preserves timeout-salvage
  evidence and an explicit non-claim. The deployment receipt was then committed
  as `eaa84b9`. `~/.local/bin/codex-ccr-worker` is now a machine wrapper over
  the canonical installed `ccr-worker`, selecting the governed GLM worker
  consumer and machine policy adapter; the old divergent Python copy remains
  only as a rollback artifact.

## Live evidence

- `ccr-fallback-policy check --json` exit 0: no drift, no restart required,
  and `glm-workers.json` advertises `zai,glm-5.3-flash` followed by
  `minimax,MiniMax-M3.1-Flash`.
- Source and both deployed execution copies of `ccr_fallback_policy.py` had
  the same SHA-256:
  `3fcedb0a84e590ac4c921d960199185e0593c8bc1e353fdf9d02fc53a03ebde2`.
- `GET http://127.0.0.1:3457/health` returned `ok`. During this task the
  worker listener served the delegated calls from the GLM tier with MiniMax
  as final fallback; the in-memory sample included 5 active GLM worker
  attempts and an AIMD limit of 45.
- Two bounded read-only GLM workers audited instruction friction and launcher
  ergonomics. The instruction audit completed in run
  `20261020-071943-yb2bkw51` (thread
  `01a159ae-b393-7302-8b67-749cbbcecd72`, requested route
  `zai,glm-5.3-flash`, 192.221 seconds, 6 successful commands, exit 0). Its
  private audit output is `build/reports/delegation-audit/instructions.txt`
  (SHA-256
  `56a8c975135be6d37301bd3e1b55ceface904dbbb0601ecd408b9dd4c7f883a4`). The launcher-ergonomics
  audit completed in run `20261020-071951-fitwg3e2` (thread
  `01a159ae-d2ca-7512-b9bb-07577eb4a212`, requested route
  `zai,glm-5.3-flash`, 699.298 seconds, 13 observed commands including one
  expected read-only sandbox failure, exit 0). Its durable output is
  `/Users/kearm/ccr-worker-launcher/build/reports/delegation-ergonomics.txt`
  (private ignored-build output; SHA-256
  `c638597ab29fb6e75003af43149d486b5f130ae1602f2b32ec757c72a4d3baca`).
- `ccr-worker-usage --days 3 --json` exit 0 reported 101 local worker runs:
  77 completed, 12 timed out, 9 cancelled, and 3 failed; 95 were read-only,
  47 allowed network, and 81 requested the current GLM Flash route. The
  concurrent listener sample had 2 active requests with 158 Z.AI and 104
  MiniMax request counters. Those counters are in-memory generation-local
  values, not lifetime totals.
- `ah brief` catalogs both `codex-ccr-worker` and `ccr-worker-usage`.
  `ah doctor --probe` reported `ok` for both tools (56 ms and 49 ms).
- After canonical deployment, live read-only run
  `20261020-074605-4zplhh53` completed in 4.539 seconds with exit 0 and final
  output `CANONICAL_LAUNCHER_OK`. Its receipt records
  `model/requested_route=zai,glm-5.3-flash`, the governed GLM→MiniMax fallback
  list, read-only/offline permissions, `timeout_profile=quick`,
  `timeout_seconds=120`, and `timeout_explicit=true`.
- CodeRabbit reviewed PR #67 with two actionable findings: the guide still
  showed the known background-launch pattern, and one malformed receipt could
  abort the whole usage summary. Both were fixed. Its low-risk redirect-bypass
  nit was also closed by using a no-redirect opener for the loopback metrics
  request. The first post-fix full matrix failed `ty` on an incompatible
  override signature; the parameter names were corrected and the full matrix
  passed again.
- Independent bounded review run `20261020-075417-yab2ly8r` (thread
  `01a159ce-5816-7473-a0c9-ddf5633d458c`, requested GLM Flash route,
  566.401 seconds, exit 0) found no
  blocking defects and independently found the userinfo hostname bypass. Its
  non-blocking robustness findings for invalid run dates, scientific-notation
  counters, and missing receipt directories were also fixed before merge. The
  private report is
  `build/reports/delegation-audit/pr67-independent.txt` (SHA-256
  `a3cd6194b29e40a0a577749c5e5a9244fd5203e28939bd40bf79c72e25c1bb27`).
- Historical launcher receipts showed the pattern this change targets: bursts
  of delegation followed by idle days, including 63 runs on 2026-10-19 and no
  completed-status run dated 2026-10-20 before this audit.

## Validation

- `git diff --check` — exit 0.
- `agent-instructions-sync --sync` — exit 0, Codex and ZCode core parity OK.
- `ruff format --check scripts`, `ruff check scripts`, and `ty check scripts`
  — exit 0.
- `python3 -m unittest discover -s scripts -p 'test_*.py'` — 128 tests, OK.
- `python3 scripts/quality.py --fast` — exit 0; property invariants passed in
  0.717 seconds.
- Final `python3 scripts/quality.py` — exit 0; all 12 default checks passed
  (format, Python format/lint/type, SLOC ratchet, ShellCheck, Clippy, Rust
  tests, no-default-features, debug build, 128 Python helper tests, and
  vendored GP tests).
- Post-review `python3 scripts/quality.py` — after one repaired `ty` failure,
  a fresh run passed all 12 checks.
- Launcher validation on merged main `a39dc9b`: `uv run ruff check .` clean
  and `uv run pytest -q` with 18 tests passed. PR #5's pre-merge branch also
  passed 17 tests.
- `sindexer update /Users/kearm/ccr-rust` was attempted after the edits but
  stopped after repeated embedding failures because `127.0.0.1:1236` has no
  listener; `sindexer-doctor` also produced no output before interruption.
  Direct source inspection and grep were used instead. The indexer stack was
  not reconfigured.

## Launcher audit result

The launcher-ergonomics worker found that the canonical source lagged the
installed machine launcher and that two tests were after `unittest.main()`
and unreachable. PR #5 repaired test reachability, profile/receipt semantics,
the 12-slot ceiling, output-path validation, and documentation. PR #6 ported
timeout-salvage evidence. CodeRabbit reviewed #5 successfully; its #6 attempt
was rate-limited until the next included window, so #6 was merged with the
local deterministic gate and no unresolved review threads. No branch protection
was available on this private repository.

## Non-claims

- This does not prove future model obedience to the instruction; it changes
  the persistent instruction and repository guidance. A later trace audit is
  needed to measure new-turn delegation frequency.
- The fast property gate is not the full quality matrix and does not run all
  Rust/Python integration tests.
- Queue-wait accounting remains unported in the canonical launcher.
- Timeout salvage was not induced in a live run; its coverage is deterministic
  unit testing.
- Worker receipts still report requested routes; they do not independently
  prove the upstream served route. Router metrics/logs remain the routing
  evidence.
