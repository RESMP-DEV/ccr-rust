# Preview/stale model-ID audit — 2026-10-19

## Scope

User asked whether the MiniMax flip to non-preview `MiniMax-M3.1-Flash` is
visible and whether any other place still uses a `preview` tag. Read-only
audit across live runtime state, the governed policy, the Codex model catalog,
the agent instructions, and the repository source.

## Verified live state

- MiniMax `/models` now lists `MiniMax-M3.1-Flash` and no `-Preview` entry.
- All four listeners (3456/3457/3458/3461) advertise
  `minimax,MiniMax-M3.1-Flash`.
- Direct probes: `MiniMax-M3.1-Flash`→`MiniMax-M3.1-Flash`,
  `MiniMax-M3.1-Flash-Preview`→`MiniMax-M3.1-Flash-Preview` (still answers,
  unlisted), `MiniMax-M3`→`MiniMax-M3`.
- A legacy `minimax,MiniMax-M3.1-Flash-Preview` route through CCR is
  canonicalized on the wire to `MiniMax-M3.1-Flash`.

## Classification of every `preview` hit

Legitimate vendor IDs (correct, left unchanged):
- `config.example.json`: `tencent/hy3-preview:free`, `qwen/qwen3.6-max-preview`.
- `examples/*` and `docs/gemini-integration.md`: `gemini-3.1-pro-preview`.
- `docs/configuration.md`: generic reasoning-preview prose.

Our stale MiniMax suffix:
- Repository source at merged `origin/main` (9b43860): NONE. `dispatch.rs` and
  `config/types.rs` both resolve to `MINIMAX_M3_1_FLASH`
  (`"MiniMax-M3.1-Flash"`). Remaining `-Preview` strings are only test-input
  literals proving canonicalization of the legacy alias, plus a comment.
- Deployed policy `~/.local/share/ccr-routing/ccr_fallback_policy.py`: pins
  `MiniMax-M3.1-Flash` (non-preview). Runtime authoritative and correct.
- `~/.claude-code-router/*.json`: no `-Preview` pin (derived from policy).
- Codex catalog `~/.codex/ccr-live-model-catalog.json`: still carries
  `MiniMax-M3.1-Flash-Preview` and `minimax,MiniMax-M3.1-Flash-Preview`
  slug entries. `ccr-catalog-sync --check` reports ok/no changes, and no
  toml/config references the slug, so they are inert display metadata, not an
  active route.

## Landmine (action required before committing recovery work)

The primary checkout is dirty at OLD main `bfb525e`,
which predates the MiniMax fix. Its working-tree
`scripts/ccr_fallback_policy.py` still pins `MiniMax-M3.1-Flash-Preview` in
both `MACHINE_ROUTE_CHAIN` and the provider `models` list. That file must be
rebased onto current `origin/main` (or the pin re-applied as non-preview)
BEFORE the separate recovery/catalog work is committed, or committing it would
regress the merged MiniMax canonicalization in the governed policy.

## Watch item

`-Preview` is now an unlisted-but-live deployment. If MiniMax later stops
serving it, the only exposure is a client pinned to the literal `-Preview`
slug; CCR's canonicalization already protects governed routes. The Codex
catalog preview slugs should be dropped at the next `ccr-catalog-sync` run that
regenerates them from live routes.

## Non-claims

Does not change any route, catalog, or config. Does not assert MiniMax will
keep `-Preview` alive or when it will be removed.
