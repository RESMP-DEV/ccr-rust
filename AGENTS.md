# CCR-Rust — Repository Guide

> `CLAUDE.md` is a symlink to this file. Use this as the canonical local guide.

## Project Scope

CCR-Rust is a multi-protocol LLM router for Claude Code, Codex, OpenAI-compatible clients, and optional MCP aggregation. It handles protocol translation, transformer pipelines, routing, metrics, and streaming.

## Environment and Setup

Before operating clients or credentials, read [Local operation](docs/local-operation.md)
and [Authentication suites](docs/auth-suites.md). Keep CCR an **opt-in launch
option**; do not replace normal Claude/Codex provider settings with global
localhost overrides.

For switching Kimi, Z.AI GLM, or another coding plan, follow
[Switching plans](docs/switching-plans.md) and its tested example config.
Use explicit `provider,model` routes; update Claude's background/subagent
defaults as well as its main model. Kimi's Anthropic endpoint requires an
output limit: keep the documented `maxtoken` transformer for Codex requests
that omit one. Check actual client tool round trips after changing a plan.

On the configured workstation, check `command -v ccr-local claude-ccr codex-ccr`
before using these local helpers. They are not installed by Cargo. Where
`claude-ccr` is still installed, use it, or `codex-ccr`, for routed sessions.
Other machines should follow the client setup guides linked from the
documentation index.

On the maintainer's Mac, `claude-ccr` is retired (October 19, 2026): run native
`claude` there instead, so Claude Code uses the native Anthropic subscription
login rather than a CCR route. The retired helper exits 2 with a routing map.
Keep CCR opt-in for Claude Code on every other machine, and never re-point a
client's global settings at a local listener.

For personal/corporate API plans, use disjoint router configs, credentials,
ports, logs, and persistence namespaces. A `provider,model` pin can still
fall back to another tier; it does not isolate accounts. For native subscription
logins, use separate client homes and verify account/workspace selection.
CCR does not import or refresh those subscription credentials. Never print
keys or copy existing auth/history stores to initialize a new suite.

Before claiming a setup works, verify the actual Git checkout, installed
binary, listener, client version, and route used. Distinguish mocked tests,
synthetic credential-selection probes, and live client tool round trips.
Coordinate service restarts or binary installation with other active worktrees.

Use the Rust-native workflow.

- `cargo check`
- `cargo fmt`
- `cargo clippy`
- `cargo test`
- `cargo build --release`

After code changes, run `cargo install --path . --force` so the installed CLI binary stays in sync with the freshly built one.

For a clean local setup:

- copy `config.example.json` to `~/.claude-code-router/config.json`,
- add API keys via environment-backed placeholders,
- start the router with `ccr-rust start`,
- verify with `ccr-rust status`, `ccr-rust validate`, or `ccr-rust dashboard`.

## Repository Layout

Important files and areas:

- `src/main.rs` — CLI entry point.
- `src/config/` — configuration parsing, pricing, and provider protocol definitions.
- `src/router/` — HTTP handlers, dispatch, request translation, response translation, and streaming.
- `src/gp_router.rs` — request-aware GP reranking and credible-set cost ordering.
- `src/frontend/` — client-format normalization.
- `src/transform/` and `src/transformer/` — transformer implementations and registry plumbing.
- `src/metrics/` — Prometheus metrics and persistence.
- `src/mcp/` — MCP server and tool-catalog handling.
- `src/ratelimit.rs` — provider backoff logic.
- `vendor/gp-routing/` — pinned Apache-2.0 GP surrogate source used by standard builds.
- `tests/` — integration coverage.

## Working Rules

Keep these invariants intact:

- Standard OpenAI- or Anthropic-compatible upstreams should be added through config first; only add code when a provider truly needs normalization or a new protocol.
- If you add a transformer, register it in both registry construction paths.
- Keep frontend detection and serialization aligned when adding client-specific behavior.
- Use environment-backed config placeholders instead of embedding secrets.
- Preserve the existing rate-limit and retry model unless the change is explicitly about routing semantics.
- Treat 429s as tracked failures: stop retries for that tier, continue to later eligible tiers, and synthesize a 429 only when every candidate is rate-limited.
- When direct or search routing pins entries at the front, update `pinned_prefix_len`; GP reranking must never move the pinned prefix.
- When `AppState` gains a field, update every integration-test helper that constructs it directly.
- Keep `vendor/gp-routing/NOTICE` accurate when modifying the vendored encoder or surrogate boundary.
- Keep the optional sindexer Git revision pinned to a reviewed public commit;
  standalone CCR-Rust clones must not require an adjacent repository.

## Common Change Patterns

For new providers:

- use config-only wiring when the upstream speaks a supported protocol,
- add a transformer only when request or response normalization is required,
- and add a new protocol variant only when the upstream cannot fit the current transport model.

For new CLI commands:

- add the command enum variant,
- hook it into the top-level dispatch,
- and keep command behavior consistent with the existing subcommand style.

## Worker inspection

### Delegation-first triage

On a new task, enumerate its pieces before touching the work, reading only
what the briefs need: owned paths, entry points, and acceptance commands.
Send every piece that can be stated as a bounded `codex-ccr-worker` order with
its own scope, owned files or surface, acceptance check, and finish line; a
one-piece task is still delegated. Keep only live conversation, cross-piece
judgment, credentials or other irreversible actions, live-service
coordination, tools the worker infrastructure cannot reach, and final
integration with the parent. Size, file count, elapsed time, and effort
already spent are not the criterion.

State the split before launching: which pieces are workers, which remain with
the parent, and under which condition. Launch the workers before doing parent
work, send independent pieces concurrently, and do not ask permission to
launch a worker the user's task already authorizes. Do not collapse several
delegable pieces into one broader interactive pass because they share a theme.

### Deterministic brief contract

Every brief is mechanically executable and states one bounded outcome, the
exact owned files or surface, the exact acceptance command or commands, the
report evidence the worker must produce, and an explicit stop condition. A
brief that leaves the worker to choose a scope, locate the right files, or
weigh cross-piece tradeoffs is not a brief: the parent splits it further or
keeps it. Ambiguity is never delegated.

The worker executes exactly one brief. It does not decide whether it is a
worker, does not re-plan the parent task, and does not decompose its own
assignment. When a brief is genuinely under-specified, the worker stops and
asks the parent for a fresh split, naming the missing piece, instead of
guessing or widening scope.

<worker_inspection>
Use `ccr-rust workers list --limit 5` and `ccr-rust workers show RUN --json`
to inspect native `codex-ccr-worker` artifacts without loading full JSONL
transcripts, then summarize adoption with `ccr-worker-usage --days 7`
(source: `scripts/ccr_worker_usage.py`). Keep the launcher's verbose
stdout/stderr redirected to private files. Select an exact run ID or receipt
path; `latest` can change when other workers start. For progress, use
`workers events RUN --after CURSOR --limit 10 --json` and retain
`next_cursor`. Fetch individual evidence with `workers detail RUN
EVENT_CURSOR --max-chars 2000`. Completion and final reports are claims, not
verified success; inspect failed commands and verify the resulting diff and
tests. See `docs/worker-inspection.md` for limits and exit semantics.
</worker_inspection>

## Validation Expectations

Run at least the relevant unit or integration tests for the area you changed. For routing, translation, or config work, `python3 scripts/quality.py` is the default offline gate; `cargo test --all-features --locked` is the focused minimum while iterating. Changes under `vendor/gp-routing/` are included by the dispatcher and have their own tracked lockfile.

Integration tests use mocked upstreams, so prefer updating those over inventing ad hoc manual checks. Use `scripts/quality.py --coverage` for measured source coverage and `--mutation` only as an explicit, potentially long diagnostic.

Installing a newly built CLI does not itself require restarting a live listener. Coordinate activation with `scripts/ccr_wait_idle.py` (or the installed wrappers' default idle-gated `restart`) so an active LLM turn is not interrupted. Use `restart-now` only for an explicit emergency.

## Pitfalls

Watch out for these recurring mistakes:

- forgetting the post-build `cargo install --path . --force`,
- registering a transformer in only one registry path,
- relying on the wrong transformer order,
- and changing streaming behavior without checking both streaming and non-streaming call paths.

## File Growth Policy

This repo enforces a SLOC ratchet. Split files when they accumulate multiple independent concerns, and prefer clear module boundaries over one giant orchestration file.

## Change log expectations

Update `CHANGELOG.md` when a change affects routing behavior, protocol support, operator workflow, validation posture, or dependency shape. For upstream or sync-heavy work, summarize the effect in plain language rather than copying commit logs.
