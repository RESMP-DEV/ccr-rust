# Quality standard establishment and routing hardening, 2026-10-19

## Standard now enforced by `scripts/quality.py`

The default offline matrix is no longer the old six-check matrix. It now owns
12 boundaries and fails closed with a per-run receipt:

1. Rust formatting.
2. Ruff formatting over `benchmarks`, `scripts`, and
   `tests/provider_validation`.
3. Ruff lint over the same executable Python trees.
4. `ty` type check over `scripts`.
5. Rust file-growth ratchet.
6. ShellCheck over the tracked shell entry points.
7. Strict Clippy, all features and all targets.
8. All-features Rust unit and mocked integration tests.
9. No-default-features build.
10. Checkout debug binary build.
11. Python helper tests, explicitly pointed at the checkout binary.
12. Vendored GP routing tests.

Dependency advisories are a release gate with commands requiring advisory
network access: `cargo audit`, `cargo deny check`, and OSV Scanner. Dated
exceptions live in `osv-scanner.toml`; expired exceptions are failures.

The canonical quality contract is `docs/quality-matrix.md`. `CLAUDE.md`
remains a symlink to the canonical `AGENTS.md`.

## Independent findings and disposition

Two read-only reviews ran against clean main. They confirmed the prior
matrix was incomplete and found code defects the existing suite missed.

Accepted blocking defects, all fixed with focused tests:

- OpenAI/Anthropic streams could end early without a protocol terminal and
  still record admission/rate-limit success. A provider disconnect could
  therefore grow AIMD admission and clear route backoff. OpenAI now requires
  `[DONE]` or `finish_reason`; Anthropic requires `message_stop`.
- A hostile `x-ratelimit-reset` could overflow `Instant` and panic dispatch.
  It now uses checked arithmetic and ignores an impossible horizon.
- Active-stream gauges were decremented manually on multiple paths. An RAII
  guard now owns the counter lifetime across the three streaming producers.

Accepted lower-severity findings, all fixed:

- Admission deferrals counted as upstream attempts.
- Unknown endpoint suffix matching could suppress appending a required path.
- Invalid configured extra headers were silently dropped at dispatch.
- A malformed unterminated SSE frame could buffer unbounded until idle
  timeout. It now fails closed at 1 MiB.
- The Python runtime test could use or skip ambient CLI state. It now builds
  and uses `target/debug/ccr-rust`.
- A recommended follower example omitted MiniMax's required transformer and
  used a legacy ID.
- Two shell, three type, and numerous Python lint/format defects existed.
- A tracked GitHub Actions workflow, dead backup, stale root configs,
  contradictory SLOC guard, and orphan test module were repository-hygiene
  defects.

Non-blocking risks intentionally left as separate scope: duplicate streaming
pumps remain structurally similar; unknown-but-valid direct routes can still
create per-route admission entries; and no general module-splitting refactor
was performed in this pass. The new ratchet prevents growth while those
follow-ups happen.

## Commands actually run

- Expanded default matrix, final result: all 12 checks passed.
- Coverage mode: all 12 checks passed, including LLVM line coverage.
- `cargo build --release --locked --all-features`: passed. Binary SHA-256
  `86a48d1577ac2650e18ca16855c0e7f22c2d5e3d848e837d212a80e5f63d07b9`.
- `cargo audit` passed with the three documented, dated `zip` extraction
  exceptions; remaining `lru` and `paste` warnings are accepted under OSV's
  dated exception policy.
- `cargo deny --workspace --all-features check bans licenses sources`: passed.
- OSV Scanner: no issues with reviewed dated exceptions.
- `ccr-rust --config examples/config.followers.json validate`: passed with
  the corrected MiniMax contract.
- Receipt: `build/reports/quality/receipt.json`; coverage report:
  `build/reports/coverage/html/index.html`.

## Non-claims

- Coverage mode proves exercised lines only, not absence of defects in
  untested protocols.
- `cargo deny` warnings for duplicate dependency versions remain nonfatal.
- The SLOC ceiling is a ratchet at the hardened baseline, not a claim that
  every legacy monolith is now well factored.
- OSV and RustSec exceptions are reviewed technical-risk decisions with
  expiry dates, not statements that the affected crates are sound.
- This receipt does not include live provider calls; quality checks are
  deterministic and offline by design.
