# CCR-Rust Repair Handoff — 2026-08-23

## Objective

Bring CCR-Rust back to a secure, reproducible, working local installation and
make the AlphaHENG submodule point at the repaired upstream revision. Preserve
the existing dirty standalone checkout and do the repair from a clean worktree.

## Repository safety

The standalone checkout was intentionally left untouched except for this
handoff file. At audit time it was on `agent/meta-muse-responses`, 34 commits
ahead and 41 behind its remote tracking branch, with these pre-existing changes:

```text
 M REVIEW.md
?? .rust-indexer/
```

Do not discard, overwrite, stash, or fold those items into the repair. Fetch
`main` explicitly and use a separate clean worktree.

The authoritative remote is `https://github.com/RESMP-DEV/ccr-rust.git`.
Remote `main` was:

```text
eaee84b419678868c720f61213421dfce5841e05
Merge pull request #30 from RESMP-DEV/feat/jina-web-tools
tree bf3cba8f26f4e66fbb69309419fbed6231eb2763
```

The installed Cargo package records its source as the temporary checkout
`/Users/kearm/.cache/alphaheng/pr-repairs/ccr-jina.apZ3RX` at commit
`9b743c0aa450156d9723c88e7ed909d5d3094c68`. That commit has the same Git tree
as the audited `main`, but installing from a temporary repair directory is poor
provenance. Reinstall from the final authoritative checkout after the fix.

## Verified working behavior

The installed binary is `/Users/kearm/.cargo/bin/ccr-rust`, version `1.3.0`,
ARM64 release, built with default features under Rust 1.97.0.

The following were verified locally without making provider calls:

- `ccr-rust validate` accepted `examples/config.minimal.json`.
- The router started on loopback, `/health` returned `ok`, `/v1/models`
  returned the configured route, and SIGINT shutdown was clean.
- The MCP daemon enforced bearer authentication: unauthenticated health was
  HTTP 401 and authenticated health returned `ok`.
- MCP `initialize` negotiated protocol `2024-11-05`.
- MCP `tools/list` returned 18 tools, including the native sindexer tools.
- `cargo fmt --check` passed against the tree on current upstream `main`.
- `cargo test --all-features --locked` passed all 560 tests; one doc test was
  ignored and there were no failures.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` passed.

## Required fixes

### 1. Fix the vulnerable lockfile

Both `cargo audit` and `cargo deny check` fail on current `main`:

```text
RUSTSEC-2026-0258: h2 0.4.13, unbounded empty DATA frames
fixed in h2 >= 0.4.16
```

`cargo update --dry-run` resolves `h2` to `0.4.18` within the current version
constraints. Apply at least the targeted update, then rebuild and rerun every
gate:

```bash
cargo update -p h2 --precise 0.4.18
```

The installed binary predates the advisory and was installed from a source
lockfile containing `h2 0.4.13`, so it must be rebuilt after the lockfile fix.

### 2. Triage the remaining RustSec and compatibility warnings

These do not currently fail `cargo audit`, but they must be resolved or given a
specific documented policy decision:

- `RUSTSEC-2026-0253`: `lru 0.16.4`, potential use-after-free from missing panic
  safety. It is pulled through both `ratatui-core` and `tantivy`/`sindexer`.
- `RUSTSEC-2024-0436`: `paste 1.0.15` is unmaintained. It is pulled through
  `egobox-gp` and the vendored `gp-routing` package.
- `redis 0.24.0` emits a Rust future-incompatibility warning. It is a direct
  dependency. The compatible lock update only reaches `0.24.1`; current Redis
  releases are substantially newer, so plan and test an intentional migration.

Use `cargo tree -i <crate>@<version>` to prove every inbound dependency path.
Do not merely add audit ignores without a written risk justification and an
owner/follow-up.

### 3. Refresh the stale dependency lock deliberately

`cargo update --dry-run` reported 186 compatible package changes. Do not land a
blind bulk lockfile rewrite. Separate the security-critical update from broader
dependency refreshes when that keeps review and rollback clear. Pay particular
attention to the optional `dashboard`, `gp`, and `sindexer` feature graphs.

### 4. Repair AlphaHENG's submodule pointer

`/Users/kearm/AlphaHENG/contrib/ccr-rust` is clean but pinned to:

```text
f568d0f1493670921c0a958d54c9dee81c9abe63
```

`.gitmodules` declares branch `main`, while upstream `main` is `eaee84b...`.
The two trees differ across eight files, with upstream containing 864 insertions
and 82 deletions relative to the pinned submodule. After the CCR-Rust repair is
merged, advance the AlphaHENG submodule to the final repaired commit and run the
parent repository's applicable validation. Do not point AlphaHENG at an
unmerged feature branch or temporary checkout.

### 5. Restore or intentionally disable local routing

The machine is more hooked up than expected:

- `~/.claude/settings.json` currently sets Claude base URLs to
  `http://127.0.0.1:3456`.
- `~/.codex/config.toml` currently selects `model_provider =
  "claude-code-router"`, whose base URL is `http://127.0.0.1:3456/v1`.
- `~/.claude-code-router/global-profile-takeover.json` lists active Claude Code
  and Codex profiles.

But the runtime is not operational:

- no CCR-Rust process is running;
- ports 3456 and 3457 have no listener;
- `~/.claude-code-router/config.json` is absent;
- `.claude-code-router.pid` is stale;
- no launchd service or startup hook was found.

The latest backup,
`~/.claude-code-router/config.json.pre-native-ae.bak`, parses as a valid
10-provider/14-tier configuration, but validation warned that
`WAFER_API_KEY` was not available in the audit environment. Never copy secrets
into this repository or commit a live configuration.

Choose and complete one coherent state:

1. Restore an environment-backed private config, arrange a reliable local
   startup mechanism, and prove Claude/Codex traffic through mocked or explicitly
   authorized upstreams; or
2. remove/disable the active Claude and Codex router selections so stopped CCR
   cannot break those clients.

Back up user configuration before editing it. Do not make real provider calls
or incur charges merely to prove local routing.

### 6. Restore CI evidence

Current upstream `main` has no GitHub check runs or commit statuses even though
`.github/workflows/ci.yml` declares a push trigger for `main`. The visible main
CI runs are older failures from 2026-08-01. Determine why the 2026-08-08 merge
did not produce checks and ensure the repaired commit has a real CI result.

## Suggested clean-worktree start

```bash
cd /Users/kearm/ccr-rust
git fetch origin refs/heads/main:refs/remotes/origin/main
repair_worktree="$(mktemp -d /Users/kearm/.cache/ccr-rust-repair.XXXXXX)"
git worktree add --detach "$repair_worktree" origin/main
cd "$repair_worktree"
```

Create a normal repair branch in that worktree before committing. Preserve the
dirty standalone checkout exactly as found.

## Acceptance gates

All of these must pass on the final locked graph and final source revision:

```bash
cargo fmt --check
cargo test --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo audit
cargo deny check
/Users/kearm/AlphaHENG/scripts/audit-dependencies.sh .
cargo build --release --all-features --locked
```

Also complete these runtime checks without a billable provider call:

- validate a representative environment-backed config;
- start the router on loopback and verify `/health` plus `/v1/models`;
- start the authenticated MCP daemon;
- prove unauthorized health is 401;
- prove authenticated health, `initialize`, and `tools/list`;
- confirm sindexer tools appear when built with default/all features;
- shut both processes down cleanly and verify no listeners remain.

After the final source and lockfile are accepted:

```bash
cargo install --path . --force --locked
ccr-rust version
```

Record the final source commit, Git tree, `Cargo.lock` hash, installed binary
SHA-256, test counts, audit outputs, runtime receipts, and the updated AlphaHENG
submodule commit. Update `CHANGELOG.md` for the dependency/security and operator
workflow changes.

