# Qoder primary worker lane integration, 2026-10-24

Agent: Codex (parent). CCR-Rust owns routing documentation and acceptance;
the behavioral implementation is in the canonical `ccr-worker-launcher`
repository and its installed uv tool. Implementation commit `15b96f0`; pushed
main receipt head `96d0e25` in `RESMP-DEV/ccr-worker-launcher`.

## Decision

Qoder is a launcher-level agent lane, not a `Router.tiers` provider. CCR's
HTTP provider model requires OpenAI, Anthropic, or Responses upstream
semantics, while the supported Qoder surface is Qoder CLI/Agent SDK. Adding a
fake HTTP provider would misrepresent an agent harness as a model API.

The installed launcher now tries Qoder first for fresh, offline, read-only
worker briefs, bounded to 20 attempts per local day. It requests display model
`DeepSeek-Flash`, internal key `dfmodel`, catalog identity
`DeepSeek-V4.1-Flash-1017`, and `--reasoning-effort max`. Only `Read`,
`Glob`, and `Grep` are enabled and strict MCP is configured with no servers.
A clean JSON result must have `subtype: success` and exactly one
`modelUsage` key, `dfmodel`. Non-timeout failures fall back to the unchanged
MiniMax-first CCR chain; write, network, resume, disabled, and exhausted
budget tasks use that chain directly.

## Live proof

- Direct Qoder CLI probe returned `QODER_V41_FLASH_MAX_OK`. Its manifest and
  request log record `DeepSeek-Flash`, `--reasoning-effort max`, model key
  `dfmodel`, and HTTP 200.
- The live Qoder dynamic catalog maps `dfmodel`/`DeepSeek-Flash` to
  `DeepSeek-V4.1-Flash-1017`; its catalog state also reports 1M max input and
  supported `max` thinking.
- Source launcher receipt:
  `/Users/kearm/.cache/qoder-worker-live/status.json` completed in 8.299s with
  `engine: qoder`, `served_route: qoder,dfmodel`, request ID
  `0c5ff30b-1baa-417a-924c-09a19cff311e`, and zero credits.
- Installed `codex-ccr-worker` receipt:
  `/Users/kearm/.cache/qoder-worker-installed/status.json` completed in
  5.885s with request ID `59b8db6e-4724-4824-9367-1511534a3983`, zero credits,
  and budget attempt 2 of 20.
- Opt-out receipt `/Users/kearm/.cache/qoder-worker-optout/status.json`
  completed through `minimax,MiniMax-M3.1-Flash`, proving the normal CCR chain
  remains intact.
- Budget-gate receipt `/Users/kearm/.cache/qoder-worker-budget0/status.json`
  ran with `CCR_QODER_WORKER_BUDGET=0`, completed through
  `minimax,MiniMax-M3.1-Flash`, and left the daily Qoder counters at 2/2/0/0.

Launcher tests: `uv run ruff check src tests` exited 0 and `uv run pytest`
exited 0 with 26 tests passed. Full implementation details and hashes are in
`/Users/kearm/ccr-worker-launcher/evidence/qoder-primary-workers-20261024.md`.
CCR-Rust documentation coverage passed `python3
scripts/test_worker_delegation_docs.py` (15 tests) and the full offline gate
`python3 scripts/quality.py` (12 PASS checks). `ccr-fallback-policy check
--json` reported zero drift, zero restarts, and the unchanged worker chain
MiniMax → GLM Flash → DeepSeek → Meta → Strata → local27; listener 3457
reported health `ok` and advertised those same tiers.

## Deployment update, 2026-11-13

The canonical launcher branch
`RESMP-DEV/ccr-worker-launcher@bc9be700a9ef164c4aad1b83a9f5451574812cee`
(`docs/qoder-guidance-surfaces`) was installed with `uv tool install --force`.
Source and installed SHA-256 values matched for `qoder_lane.py`,
`qoder_write.py`, and `worker.py`; the user-facing `codex-ccr-worker` wrapper
execs that installation. No CCR listener or `Router.tiers` change occurred.

The lane now requests `Qwen4-Max-Preview` / `qmodel_latest` and accepts fresh
offline read-only briefs, offline read-only resumes using Qoder's persisted
session ID, and offline scoped writes with `--write-workspace` plus exact
relative `--owned-path` declarations. Scoped writes execute in a disposable git
worktree with Read/Glob/Grep/Write/Edit, no Bash, no network, and no session
persistence. The launcher rejects deletions, symlinks, and out-of-scope changes
before applying anything. Network and undeclared-write work still bypass Qoder.

Live evidence:

- Fresh read-only audit run `20261113-211619-ecuyvqrd` completed with
  `engine=qoder`, `served_route=qoder,qmodel_latest`, and request
  `7dcf0db8-2ae6-470d-89fd-c929a19d68d4`.
- Direct persisted-session resume request
  `396d11ee-18e0-48f2-879a-0ee97c810b97` exited 0 and returned
  `QODER_PERSISTED_RESUME_OK`; the old session also reported both legacy and
  current usage keys, confirming why exact-key validation must reject it.
- End-to-end wrapper scoped-write run `20261113-212801-1mehm0uo` exited 0 at
  budget attempt 20/20 with `engine=qoder`,
  `served_route=qoder,qmodel_latest`, request
  `45978086-c185-43a7-b51b-5cab78ac1e60`, and only `owned.txt` applied.
  Parent verification confirmed `outside.txt` and all other paths were
  untouched and the isolated worktree was removed.

Launcher validation at implementation head: `uv run pytest` passed 48 tests
plus 2 subtests, `uv run ruff check .` passed, and `git diff --check` passed.
CCR-Rust consumer guidance and `scripts/test_worker_delegation_docs.py` were
updated from the stale DeepSeek identity to Qwen4, scoped writes, and
read-only resume eligibility.

## Non-claims

- Qoder zeroes token counts; reasoning depth at `max` is requested and
  catalog-supported but not measurable from the result envelope.
- The full V4.1 identity is established by the live catalog mapping plus the
  observed `dfmodel` result key, not by a full identity echo in the response.
- No CCR listener restart or shared-policy route change occurred. Launcher
  network delegation remains on the governed CCR chain by design.
- The wrapper scoped-write proof covers one disposable regular-file repository.
  Submodules, ignored-file preservation, symlinked paths, concurrent workspace
  mutation, and isolation-limit failures are covered by unit tests and fallback
  behavior, not by this live wrapper run.
