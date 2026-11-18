# Memory-graph local BFS repair, 2026-11-17

## Scope

This receipt records review of the opt-in `memory-graph` feature and the repair
that replaces its newly proposed external graph crate with an in-repo bounded
breadth-first traversal. It covers tool semantics, feature discoverability,
tests, dependency shape, formatting, lint, compilation, and the full Rust test
suite. No live router listener or production routing configuration was changed.

## Source

- Base/main: `81a994a7e507b43769528659f8f461239c80fa18`
- Reviewed contributor head: `e1dcec2ec6425f6c42e76c30c8749e6eb8d82e68`
- Repair branch: `memory-graph-local-bfs`
- The contributor head remains part of the branch history; the repair removes
  `mattbusel-mem-graph` and retains the feature as `memory-graph = []`.

## Semantic contract

`open_related_nodes` remains feature-gated and follows only relation direction
(`from` -> `to`). Omitted or null depth means one hop. Integral JSON numbers
such as `2.0` count as integers; negative, fractional, non-numeric, and
non-finite values are rejected. Distinct start names are deduplicated, unknown
names contribute nothing, and `depth: 0` returns exactly the `open_nodes`
payload. Relations incident to any returned entity are returned, including
dangling endpoints. The visited set bounds cycles, self-loops, and huge depths
by the entity count.

The local traversal starts once from all distinct valid start names and follows
only relations whose two endpoints are stored entities. This preserves the
previous traversal behavior while avoiding redundant per-start work and
continuing to report dangling relations through the output filter, matching
`open_nodes`. The adjacency index and traversal are O(entities + relations) per
call and remain under the graph read lock; caching would require a
mutation-version contract outside this feature's scope.

## Verification

All passing commands ran in a clean pinned worktree on macOS arm64 with
sccache enabled:

| Check | Result |
| --- | --- |
| `cargo fmt --check` | pass |
| `cargo test --locked --features memory-graph --test test_mcp_daemon test_memory_open_related_nodes` | 3/3 pass, including daemon `tools/call` dispatch |
| `cargo test --locked --test test_mcp_daemon test_initialize_and_tools_list` | 1/1 pass without the feature |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | pass |
| `cargo check --locked --no-default-features` | pass |
| `cargo test --locked --all-features` | pass; 642 unit tests and all integration targets |
| Source search for `mattbusel-mem-graph` / `mem_graph` | no matches in manifest, lock, or memory tool |
| `git diff --check` | pass |

The repository's aggregate `python3 scripts/quality.py` still fails on three
pre-existing baseline defects: one Python formatting violation in
`scripts/test_worker_delegation_docs.py`, the Rust SLOC ratchet, and the SLOC
helper test driven by that ratchet. Exact main produced the same Python format
failure and the same two SLOC errors before this change. The script's Rust
format, Python lint/typecheck, shellcheck, clippy, Rust tests, no-default
feature check, debug binary, and vendored GP checks passed at the repair head.

The repair also removes the one new crate from the dependency graph. Exact-main
and the contributor head both fail current `cargo audit`, `cargo deny`, and
OSV scans for ten newer pre-existing advisories across zstd, webpki, zip, and
related graph nodes; those baseline failures are unchanged by this branch and
are not introduced by the memory-graph feature.

## Review and worker evidence

Three read-only audits covered dependency/security, API/performance, and
feature/test discoverability. They confirmed the current depth parsing and
tool wiring, identified the missing cycle/unknown-start tests, and recommended
an in-repo BFS rather than a new same-day single-maintainer crate. A scoped
write worker produced the local traversal, edge-case tests, and operator docs;
the parent inspected the full diff, regenerated the lockfile, and independently
ran every acceptance command above.

## Non-claims

- The daemon test exercises a loopback JSON-RPC `tools/call` round trip in the
  test harness; no separately installed production daemon was started.
- No production listener was restarted or installed, and no routing policy
  changed.
- The pre-existing Python formatting, SLOC ratchet, and dependency advisory
  failures remain open repository maintenance work.
- The full `python3 scripts/quality.py` aggregate is not a pass at this head
  for the documented baseline reasons above.
