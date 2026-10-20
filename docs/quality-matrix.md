# Quality matrix

## Status

This document is the operational contract for the repository quality
dispatcher. The default test matrix is deterministic and offline: it does not
call live model providers. Coverage and mutation modes are explicit opt-ins
because they are substantially slower and require additional developer tools.

## Quality matrix

Run the default matrix from the repository root:

```bash
python3 scripts/quality.py
```

The default owns these boundaries:

| Boundary | Command owner |
| --- | --- |
| Rust formatting | `cargo fmt --check` |
| Python formatting | `ruff format --check` over `benchmarks`, `scripts`, `tests/provider_validation` |
| Python lint | `ruff check` over the same paths |
| Python type check | `ty check scripts` |
| Shell lint | `shellcheck` over the tracked shell entry points |
| Rust file-growth ratchet | `python3 scripts/check_sloc.py --json` |
| Rust lint | `cargo clippy --all-features --all-targets --locked -- -D warnings` |
| Rust unit and mocked integration tests | `cargo test --all-features --locked` |
| Portable feature boundary | `cargo check --no-default-features --locked` |
| Checkout debug binary | `cargo build --all-features --locked` |
| Python policy/runtime helpers | `CCR_TEST_BINARY=$PWD/target/debug/ccr-rust python3 -m unittest discover` |
| Vendored GP surrogate | `cargo test --manifest-path vendor/gp-routing/Cargo.toml --locked` |

The Rust ratchet caps total nonblank `src/**/*.rs` lines at the measured
merged-main baseline, caps every file at 2,500 nonblank lines, and gives the
five existing largest hotspot files individual ceilings. Lower a ceiling when
a refactor shrinks the measured baseline; do not raise one to admit new code.

`ruff.toml` is the single source of truth for the Python rules, so a new
helper inherits the same standard instead of drifting. The rule set is pinned
explicitly rather than inherited from Ruff's defaults, so a default change
cannot silently weaken the boundary. Each per-file exception names the
deliberate pattern it permits and why. The benchmark and
provider-validation directories are included because they are executable code
shipped with the repository, not scratch files. `ty` is scoped to `scripts`,
which is the only Python tree that runs in CI-equivalent local validation and
has no third-party import requirements. Python runtime tests explicitly use
the checkout-built debug binary, never an ambient installed CLI, and fail if
that binary is absent. The benchmark and provider-validation trees import optional `aiohttp`, `starlette`, `uvicorn`, `openai`, and
`anthropic` packages that are not installed in the validation environment, and
their type check is therefore not part of the default matrix.

Every invocation unlinks and then writes
`build/reports/quality/receipt.json`. The receipt records source revision and
dirty state, each command, exit code, elapsed time, and log path. An
interrupted run cannot leave a stale PASS. A failing command still writes a
failure receipt and does not abort later checks, so one defect does not hide
another.

Use `--fast` while iterating on property invariants. Use `--coverage` to
replace the normal Rust test invocation with LLVM source coverage; the current
enforced line-coverage floor is 75%. Use `--mutation` after the default matrix
passes; mutation testing is a diagnostic, not a routine merge gate. `--list`
only prints the selected checks.

The vendored GP package has its own tracked lockfile. Do not remove it: its
tests choose dev-dependency versions independently from the root CCR lockfile.

## Dependency security

Dependency advisories are a release gate, not part of the default matrix,
because the check needs network access to advisory databases. Run them before
a release:

```bash
cargo audit --file Cargo.lock \
  --ignore RUSTSEC-2026-0367 \
  --ignore RUSTSEC-2026-0368 \
  --ignore RUSTSEC-2026-0369
cargo deny check
cargo deny --manifest-path vendor/gp-routing/Cargo.toml check
osv-scanner scan source --config osv-scanner.toml --lockfile Cargo.lock
osv-scanner scan source --config osv-scanner.toml --lockfile vendor/gp-routing/Cargo.lock
```

The vendored GP surrogate has its own tracked lockfile and its own dependency
graph, so it is scanned and denied separately. `deny.toml` and
`osv-scanner.toml` carry the same dated exceptions because cargo-deny does not
read the OSV Scanner configuration.

Every exception in `osv-scanner.toml` carries an expiry date, and
`docs/dependency-security.md` records the reason, owner, and follow-up for
each accepted advisory. An expired exception is a finding, not a waiver.
