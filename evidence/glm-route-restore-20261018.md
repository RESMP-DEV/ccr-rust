# GLM route restore after cordon premise disproof — 2026-10-18

## Scope

Restored the GLM-5.3-Flash and GLM-5.3 tiers (chain head and second tier)
plus their consumer primaries, reversing the same-day quota cordon
(commit a0fe126). Executed on explicit user direction after the cordon's
stated premise was disproved with live evidence. Also added a derived
`ccr-primary` alias per consumer so stable client pins track the primary
tier across future route changes without client restarts.

## Why the cordon was reversed

The cordon removed every GLM route because `modelctl usage --provider
zcode --json` reported the monthly tool quota (`TIME_LIMIT:5:1`) at
4000/4000, 0% remaining, reset 2026-10-28 20:26:14-07:00, and reasoned
that routing to an exhausted plan "spends a refusal on every call". That
causality is wrong for completions:

- Router log (service.stdout.log): between the 01:32 cordon deploy (which
  already reported 4000/4000) and the 03:04 main-listener activation, the
  still-old chain served thousands of successful `zai` completions,
  including six in the final 25 seconds before cutover. The observed
  failures were 5-hour-window/concurrency 429s that cascaded to MiniMax
  exactly as the chain is designed to do.
- The ZCode desktop app kept serving GLM-5.3 on the same plan throughout,
  including during this repair.
- Conclusion: the monthly counter gates the plan's tool usage; completions
  are gated by the rolling 5-hour prompt window (13-18% at the time) and
  concurrency caps. A refusal, when it happens, cascades to the MiniMax
  final tier, so no request pays a guaranteed failure.

Secondary defect fixed by the same change: the cordon's hard removal of the
`zai` provider broke every client explicitly pinned to `zai,glm-5.3`
(single-tier direct routing has no fallback; 1,353 terminal 503s). Clients
are now pinned to the stable `ccr-primary` alias instead, which rides the
chain and follows router-side route changes with no client restart.

## Evidence

- Source change: `scripts/ccr_fallback_policy.py` — chain restored to
  (`zai,glm-5.3-flash`, `zai,glm-5.3`, `minimax,MiniMax-M3.1-Flash-Preview`);
  consumer primaries restored (config.json/config-auth.json `zai,glm-5.3`,
  glm-workers.json `zai,glm-5.3-flash`); `zai` provider re-registered as a
  shared provider with `${CCR_ZAI_API_KEY}` (present in
  `runtime-credentials.json`); `derive()` now sets
  `modelAliases["ccr-primary"] = primary` for every consumer.
- `uv run --no-project python -m unittest discover -s scripts -p
  'test_ccr_fallback_*.py'` — 29 tests, OK (chain shape, zai registration,
  alias expectations updated; cordon tests replaced by restore tests).
- `uv run --no-project ruff check scripts/ccr_fallback_policy.py
  scripts/test_ccr_fallback_policy.py` — clean.
- Deployed the new module to `~/.local/share/ccr-routing/` and
  `~/.claude-code-router/`; sha256 309e1db7752298b9… identical across
  source and both execution copies. `fallback-policy.json` rewritten from
  the new constants; `load_policy` gate passes.
- `ccr-fallback-policy preflight --json` — `zai ok (authenticated on
  models)`, `minimax ok`, `registered_not_routed: [deepseek, gmi]`, ok=true.
- `ccr-fallback-policy check --json` — drift on all four consumers with the
  expected tier orders; `sync --json` applied them.
- Idle-gated restarts: 3456 via `ccr-local restart` (idle after 0.5s);
  3457 and 3459/3461 restarted. The first 3458 (OCR) respawn wedged inside
  dyld (`ExternallyViewableState::generateAtlas` / `PropertyList::encode`,
  confirmed by sample(1)), never reaching policy code; the wedged pid was
  killed and the launchd respawn came up healthy. All four listeners serve
  the restored catalogs (3456 default `zai,glm-5.3`; 3457 flash-first; OCR
  keeps its OpenRouter primary; auth health 200).
- Live verification: `/v1/responses` with `model=ccr-primary` on 3456 →
  "Direct routing: zai,glm-5.3 moved to front", "Success on zai after
  2.54s", served model `glm-5.3`. Fresh `codex exec` (read-only sandbox) →
  "Incoming request for model: ccr-primary (frontend: Codex)", "Success on
  zai after 5.81s", reply delivered.
- Claude's operator alias `claude-glm-5.3` (dropped by the cordon sync)
  restored into config.json and preserved by derivation.

## Non-claims

- Preflight proves credential authentication only, not completion quality.
- Whether Z.AI eventually hard-enforces the monthly counter for completions
  later in the window is unknown; if it does, affected requests cascade to
  MiniMax by design. The 5-hour window and concurrency caps remain the
  observed completion limits.
- Worker (3457) and OCR (3458) chains are restored and catalog-verified but
  no worker job or OCR review ran during this repair.
- The OCR dyld wedge is characterized by one sample; root cause was not
  investigated further after the clean respawn.

## Amendment: interactive no-drop scoping (same day)

User direction: Z.AI draws no distinction between model sessions and tool
use for metering (tool calls are model-initiated), the plan plainly has
capacity while in use, and interactive Codex traffic must never be dropped
onto another provider. Implemented `MACHINE_CONSUMER_ROUTES =
{"config.json": ("zai,glm-5.3",)}`: the main listener derives a
single-tier chain (GLM-5.3 only), so a rate-limited GLM holds and re-sweeps,
surfacing 429 to the client, instead of cascading to GLM-5.3-Flash or
MiniMax. Workers, auth, and OCR keep the shared chain.

- Tests: 31 passing (scope derivation, scope-excludes-primary error, gate
  tamper on the scope, machine constants); ruff clean.
- Deployed both execution copies, gate passed, preflight ok (zai + minimax),
  `check`/`sync` changed only `config.json`.
- Activation: the idle-gated restart aborted because that process generation
  served HTTP 200 with an empty `/metrics` body (see anomaly below); with
  idleness verified from the request log (last completed request 11:07:39Z),
  a direct `launchctl kickstart -k` activated `Tier order: ["zai,glm-5.3"]`.
- Verification: 3456 catalog advertises only `zai,glm-5.3`, `glm-5.3`,
  `claude-glm-5.3`, and `ccr-primary`; live `ccr-primary` request logged
  "Success on zai after 1.26s".

Anomaly, not fully investigated: one process generation (started 11:07:41Z
by a SIGTERM whose sender was not identified) served an empty `/metrics`
body, breaking `ccr-wait-idle`; the next generation serves 4,480 bytes and
the idle gate works again.

## Amendment 2: scoping reverted to the plain chain (same day)

The no-drop scoping in Amendment 1 over-corrected the user's intent: "never
accidentally dropped onto MiniMax" referred to the cordon-style removal of
GLM, not the absence of fallback. Standard Codex should ride the ordinary
chain: GLM-5.3, then GLM-5.3-Flash, then MiniMax. `MACHINE_CONSUMER_ROUTES`,
its gate pin, the derive filter, and their tests were removed. 29 tests
pass, ruff clean; preflight ok (zai + minimax); sync changed only
config.json; an idle-gated restart activated `Tier order: ["zai,glm-5.3",
"zai,glm-5.3-flash", "minimax,MiniMax-M3.1-Flash-Preview"]`; a live
`ccr-primary` request was served by glm-5.3 ("Success on zai after 2.78s").
The idle gate worked normally on this restart (metrics healthy).
