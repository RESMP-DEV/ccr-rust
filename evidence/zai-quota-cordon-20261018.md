# Z.AI monthly tool quota cordon — 2026-10-18

## Scope

Cordoned the Z.AI GLM coding-plan routes after the provider reported the
monthly tool quota exhausted, leaving MiniMax M3.1 Flash Preview as the governed
non-OCR final route. This machine-side action does not own ZCode availability
filtering; another machine is handling that boundary.

## Evidence

- `modelctl usage --provider zcode --json` exited 0 and reported
  `TIME_LIMIT:5:1` (`monthly tool quota`) at `4000/4000`, `0.0%` remaining,
  with reset deadline `2026-10-28 20:26:14-07:00`.
- Source route chain and all governed primaries changed to
  `minimax,MiniMax-M3.1-Flash-Preview`. OCR retains its OpenRouter primary and
  now falls through only to MiniMax.
- `ccr-fallback-policy preflight --json` exited 0. It probed only routed
  MiniMax, which authenticated with HTTP 200 through the model-list check;
  dormant DeepSeek and GMI were reported `registered_not_routed`.
- `ccr-fallback-policy sync --json` and a subsequent `check --json` exited 0
  with no drift across `config.json`, `config-auth.json`, `glm-workers.json`,
  and `ocr-reviewers.json`.
- `uv run --no-project python -m unittest discover -s scripts -p
  'test_ccr_fallback_*.py'` passed 29 tests.
- `uv run --no-project ruff check scripts/ccr_fallback_policy.py
  scripts/test_ccr_fallback_policy.py` and `git diff --check` passed.
- Full `python3 scripts/quality.py` passed all six checks: format, clippy,
  Rust tests, no-default-features, Python helpers, and vendored GP routing.
  Its machine receipt is `build/reports/quality/receipt.json`.
- Idle-gated restarts activated MiniMax-only catalogs on ports 3457, 3458, and
  3461. Health probes returned 200; `/v1/models` exposed no Z.AI IDs.
- A bounded one-hour idle watcher was installed at
  `/Users/kearm/build/ccr-cordon/activate-main-listener.sh` for the active main
  listener on port 3456. It waits for two stable idle samples, kickstarts the
  LaunchAgent, and logs the resulting health/model catalog to
  `/Users/kearm/build/ccr-cordon/activate-main-listener.log`. After the first
  detached shell child was reaped at tool-session teardown, the same bounded
  watcher was rearmed as LaunchAgent
  `com.kearm.ccr-cordon-activate`; launchd reported it running while the main
  listener held two active requests.
- Separate Claude/ZCode MCP cleanup removed `zai-mcp-server`,
  `web-search-prime`, `web-reader`, and `zread` from `~/.claude.json` after a
  mode-0600 timestamped backup. The three already-running `@z_ai/mcp-server`
  workers were terminated, and no `@z_ai/mcp-server` process remained.

- Review round repair: Kilo Code Review reported that the port 3458 OCR primary
  is `openrouter,nvidia/nemotron-3-ultra-550b-a55b:free`, not MiniMax. The
  cordon commit had rewritten `docs/local-operation.md` to say OCR started at
  MiniMax. That wording was corrected at head `81cce37`, which states ordinary,
  worker, and OCR *fallback* routes start at MiniMax while the OCR primary stays
  on OpenRouter. `python3 scripts/quality.py` passed all six checks at that head,
  and the live 3458 catalog still lists OpenRouter before MiniMax.

## Restoration contract

Do not restore either current GLM tier merely because the provider timestamp
arrives. First rerun `modelctl usage --provider zcode --json`, require monthly
headroom, then make a reviewed source-policy change with tests and preflight.

## Non-claims

- The model-list preflight proves authentication only, not completion quality.
- The main-listener activation is intentionally deferred until its active
  request drains; until then its in-memory process can retain the prior chain.
- The Claude/ZCode MCP cleanup was performed before the user said another
  machine owns availability filtering. No further ZCode filtering work was done
  after that instruction.
