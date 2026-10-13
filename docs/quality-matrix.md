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
| Formatting | `cargo fmt --check` |
| Lint | `cargo clippy --all-features --all-targets --locked -- -D warnings` |
| Rust unit and mocked integration tests | `cargo test --all-features --locked` |
| Portable feature boundary | `cargo check --no-default-features --locked` |
| Python policy/runtime helpers | `python3 -m unittest discover` |
| Vendored GP surrogate | `cargo test --manifest-path vendor/gp-routing/Cargo.toml --locked` |

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
