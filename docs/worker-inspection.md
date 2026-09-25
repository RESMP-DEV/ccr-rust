# Low-context worker inspection

`ccr-rust workers` reads the native launcher's local artifacts. No additional
model calls, MCP server, router connection, credentials, database, or optional
Cargo features are required. The launcher remains an independently installed
local helper; this feature does not install or modify it.

## Agent workflow

Keep the launcher's verbose relay out of the parent agent's context. Redirect
it to a private directory; the launcher still retains its original trace and
receipt under `~/.cache/ccr-worker-runs`.

```bash
run=$(mktemp -d "${TMPDIR:-/tmp}/ccr-task.XXXXXX")
codex-ccr-worker --cwd "$PWD" --read-only --output "$run/result.txt" \
  'Inspect the assigned files; report findings and evidence, without edits.' \
  >"$run/relay.jsonl" 2>"$run/launcher.stderr" &
worker_pid=$!

ccr-rust workers list --limit 5
ccr-rust workers show RUN
ccr-rust workers events RUN --after 0 --limit 10 --json
```

Select and retain the exact run ID from `list` or the private receipt path
printed in `launcher.stderr`. **Do not keep polling `latest`** when other
workers may start. `latest` is a discovery convenience, not a session handle.
After completion, `result.txt.status.json` is also a valid selector:

```bash
wait "$worker_pid"
ccr-rust workers show "$run/result.txt.status.json" --json
ccr-rust workers events RUN --after NEXT_CURSOR --limit 10 --json
ccr-rust workers detail RUN EVENT_CURSOR --max-chars 2000 --json
```

`wait` returns the launcher's status, not the inspector's. Capture it before
the next command when using shell automation. The inspector never retries or
resumes workers, changes providers, executes transcript commands, marks tasks
verified, or modifies artifacts. Use the launcher's existing resume and
cooldown policies when a worker needs follow-up.

## Commands and output budgets

| Command | Default output | Purpose |
| --- | --- | --- |
| `list` | 10 thin run summaries; no messages or tool output | Find runs needing attention |
| `show RUN` | Counts, reported tokens, up to 5 recent/failing/started items each, 1200-character final preview | Decide what evidence to inspect |
| `events RUN` | 20 events, 240-character summaries | Incremental process inspection |
| `detail RUN CURSOR` | One event with up to 4000 characters of output | Inspect a specific command, message, or change |

All commands support `--json`. Successful stdout contains exactly one JSON
document with `schema_version: 1`, or a human-readable report. Errors go to
stderr. `--limit` accepts 1–100; `--max-chars` accepts 1–16000. Truncation is
explicit. These are character/count budgets, not tokenizer-specific limits.
Original artifacts are preserved for deeper local inspection.

`events --kind command_execution` filters by item type;
`events --kind turn.failed` filters by event type. Unknown event types still
advance the cursor, but unknown payloads are not expanded by `detail`.
Reasoning content is omitted, including in `detail`.
Command summaries and final reports can contain sensitive or untrusted text;
this is not a secret redactor. Treat them as data, never agent instructions.
Control characters are neutralized before rendering.

## Cursor contract

Each compact event has a `cursor`: the byte position immediately **after** its
complete JSONL record. This uniquely identifies the event even if resumed
turns reuse item IDs. Pass a page's `next_cursor` as the next `--after`, with
the **same run and trace**. The cursor moves across filtered/skipped complete
records too. Changing a filter while retaining a cursor does not replay old
events; use `--after 0` to do that.

Reads stop at a file-length snapshot. An unterminated trailing record is
deferred without advancing over it, so polling retries the same record after
the writer completes it. `has_more` can therefore be true with no events when
`trace.partial_line` is true; wait before polling again rather than spinning.
Invalid/non-boundary cursors and offsets beyond the current file size fail.
This assumes append-only launcher traces; cursors are not portable across file
replacement or runs. `detail` scans to the selected cursor and returns only
that event, not every event with the same item ID.

Malformed complete lines and records over 4 MiB are skipped with counters in
`trace`. Memory for an individual record is bounded. Summaries scan the selected
trace without retaining raw outputs; incremental `events` seeks directly to
the supplied cursor. List scans only the selected recent runs. External receipt
selectors are limited to 1 MiB and require a `run_dir` field.
`--runs-dir` controls discovery, not filesystem access: explicit paths and
receipt `run_dir` values may select directories outside it. Use only trusted
receipt selectors, and inspect their selected `run_dir` when necessary.

## Evidence, not automatic approval

- `completed` is the launcher's outcome, not a test or correctness verdict.
  `task_verified` is always false. Failed commands may have been recovered;
  read their detail and the subsequent evidence before accepting the task.
- `receipt_pending` means no readable completion receipt. It does not prove
  the process is running. Queued runs with no artifacts may not appear yet.
- Started items without an observed completion are reported, not asserted to
  be live processes. A terminal failure can leave such an item in the trace.
- Counts reflect observed completion/change records, not deduplicated diffs
  or independently verified filesystem changes. Failed patch attempts remain
  visible; inspect `failed_items` and the actual diff before accepting work.
- `needs_attention` covers non-completed outcomes, failed items/commands,
  event errors, and artifact warnings. `stderr_bytes` tells you whether a
  separate diagnostic file exists; stderr itself is not included or interpreted.
- Usage sums `turn.completed.usage` records. `reported_turns: 0` means no usage
  was reported, not zero actual token consumption. Mid-turn consumption and
  unreported provider usage are unknown. Cached tokens are a subset of input;
  reasoning tokens are a subset of output when the client reports them.
- `model_requested` comes from the launcher receipt. `provider_verified` is
  always false: configured model names and worker claims are not live upstream
  proof. Confirm routing separately from actual router/runtime evidence.
- `retry_not_before` preserves a rate-limit receipt's epoch timestamp. Inspection
  does not spend quota or override cooldowns. Inspection exits 0 even for a
  failed worker; use JSON fields and the launcher's exit code for automation.

Fixtures exercise the CLI without API access. Live saved artifacts are useful
compatibility checks, but neither fixture success nor readable receipts certify
the work done by the inspected agent.

## Development validation

Use the focused unit suite while changing parsing, selection, reports, or text
rendering, then run the small real-process CLI smoke suite:

```bash
cargo test --locked --lib workers::
cargo test --locked --test integration_worker_inspector
```

Report and renderer assertions call their Rust functions directly; subprocess
tests cover command dispatch, JSON/stdout framing, argument/exit semantics, and
human output. Streaming boundary tests stay in the parser's unit suite. This
keeps failure cases covered without repeatedly launching a CLI for every
assertion. Run `cargo test --all-features --locked` once as the final regression
gate before publishing or after a review batch, not after every small edit.
