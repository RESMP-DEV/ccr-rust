# Tool-call failure census and system-prompt trim — 2026-11-26

## Scope

Censused tool-call failures in local Codex agent traces served by GLM-5.x,
mapped the prompt-caused classes to the instruction stack, and applied
corrections plus a size reduction to `~/.agent-core.md` and the Codex tail of
`~/.codex/AGENTS.md`. This is prompt policy work, not a router code change:
nothing under `src/`, `scripts/`, or the live listeners was modified, and no
service was restarted.

## Tooling found

- `session-audit` (`~/session-audit`) already classifies command failures and
  guard rejections and detects retry loops
  (`src/session_audit/detect.py:169` and `:207`), but has no class for
  call-shape failures (schema rejection, JSON truncation, duplicate fields,
  empty patches, wrong tool names).
- `modelctl context --json` reports per-session context composition from local
  agent telemetry; it does not classify tool-call outcomes.
- Gap filled for this task: `build/reports/toolcall-census/census.py`, a
  streaming classifier over the rollout corpus. Rerun:
  `python3 build/reports/toolcall-census/census.py --root /Users/kearm/.codex/sessions --days 30 --as-of 2026-11-26`
  exits 0 in 10.5 s and rewrites `census.json` and `census.md`.

## Census (30 days, 2026-10-27 to 2026-11-26)

421 GLM sessions, 68,596 tool calls, 4,383 failed (6.4%).

| Class | Count | Share |
|---|---:|---:|
| command_failure | 3,709 | 84.6% |
| json_truncation | 192 | 4.4% |
| retry_loop | 176 | 4.0% |
| guard_rejection | 102 | 2.3% |
| wrong_tool | 101 | 2.3% |
| arg_schema_rejection | 94 | 2.1% |
| duplicate_field | 5 | 0.1% |
| empty_patch | 4 | 0.1% |

Authoritative full-corpus counts over `function_call_output` records (not
GLM-filtered, each failure logged once): 772 JSON truncations, 247 `rm -f`
guard rejections, 155 missing-`cmd` schema rejections, 36 `update_plan`
wrong-tool calls.

Model split: glm-5.3 ran 49,583 calls at 7.70% failure, glm-5.4 ran 19,013
calls at 2.97% failure, with zero json_truncation on glm-5.4.

## Root-cause split

- **Not prompt-caused:** JSON truncation. The cut point has a median of 231
  characters (p25 116, p75 502, p95 1842, n=1013), so the prompt's 4 KiB
  payload rule cannot be the cause. It is concentrated in glm-5.3 and
  MiniMax-M3.1-Flash and absent from glm-5.4, which points at upstream
  generation or streaming, not instruction size.
- **Prompt-caused:** 36 calls to `update_plan`, a tool this harness rejects as
  unsupported, driven by an instruction that assumed it exists. 247 `rm -f`
  guard rejections, where the prompt forbade the command without naming a
  usable substitute. 155 calls missing the required `cmd` field. 4 empty
  `apply_patch` payloads.
- **Partly prompt-caused:** 259 `apply_patch` "failed to find expected lines"
  failures, the single largest signature, from hallucinated context lines with
  no stated recovery rule.

## Changes applied

`~/.agent-core.md`, 66,117 to 59,216 bytes (-6,901, -10.4%):

- `tool_call_hygiene` rewritten: measured truncation median, harness-conditional
  patch tool with a one-retry-then-write-file recovery rule, mandatory `cmd`,
  ban on calling a tool named `tool`, and the concrete `rm -f` substitute
  (`pathlib.unlink` / `shutil.rmtree`).
- `prefer_cli_tools` compressed from 7,766 to about 3,500 bytes: the dated
  per-tool catalog is replaced by `ah brief` as the authority, keeping the
  negative-lane claims (Grok/alt-vision, Gmail API, Claude Code vision,
  Cloudflare registrar scope).
- `difficulty_is_fanout_trigger` section removed as a triple duplicate; the
  rule survives in `orchestration_and_autonomy` and in the worker-delegation
  section, and the dangling tag reference was removed.
- Cloud GPU block compressed with every rule retained.
- Worker-delegation Qoder and routing paragraphs tightened; added the live
  correction that `--owned-path` must name a file, not a directory.

`~/.codex/AGENTS.md` Codex tail: `update_plan` instruction made conditional on
the harness exposing it, with a fallback checklist and a no-retry rule.

`~/.agent-core.md.bak` (pre-change copy) is at
`build/reports/prompt-inventory/agent-core.md.bak`.

## Evidence

- `python3 build/reports/toolcall-census/census.py --root
  /Users/kearm/.codex/sessions --days 30 --as-of 2026-11-26` exited 0 in
  10.5 s and reproduced 4,383 failed calls with an identical class table.
- `agent-instructions-sync --sync` exited 0; `agent-instructions-sync` then
  reported all seven parity checks OK (codex core, codex leanctx, zcode core,
  qoder lean, qoder core A, qoder core B, claude imports).
- XML tag balance in `~/.agent-core.md`: 33 opening and 33 closing tags, no
  unbalanced names.
- `python3 scripts/test_worker_delegation_docs.py` passed 22 tests; the repo
  `AGENTS.md` was not modified, so its pinned phrases are intact.
- Live token measurement with an identical one-word probe worker before and
  after: 26,458 input tokens before, 24,933 after (-1,525, -5.8%).
- Full worker reports: `build/reports/toolcall-census/census.md` and
  `build/reports/prompt-inventory/inventory.md`.

## Follow-up

- The router-side transport fix landed after this receipt: PRs #86, #87, and
  #88 merged to `main`, and the merged binary was deployed to all four
  listeners. The root-cause and deployment record is
  `evidence/sse-decoding-root-cause-and-terminal-fix-20261126.md`.
- Post-receipt observation, 2026-11-26 22:55 PST: `~/.agent-core.md` is
  52,995 bytes (mtime 16:49), 6,221 bytes below the 59,216 recorded above, so
  a later edit trimmed it further. `agent-instructions-sync` reports all seven
  parity checks OK at that size. The pre-change backup is still the 66,117-byte
  copy at `build/reports/prompt-inventory/agent-core.md.bak`.

## Non-claims

- The census is observational. It does not prove that any prompt edit reduces
  future failure rates; that needs a post-change window of the same census.
- The token measurement is one probe on one route, not a distribution.
- JSON truncation is not fixed by this work. A router-side repair (retry or
  JSON-repair on unterminated tool arguments) is the mechanism that would
  address it and was not implemented here.
- `ccr-rust/AGENTS.md` still duplicates about 8.2 KB of the core's delegation
  sections. It was left untouched because it carries another agent's
  uncommitted modifications and its phrases are pinned by
  `scripts/test_worker_delegation_docs.py`.
