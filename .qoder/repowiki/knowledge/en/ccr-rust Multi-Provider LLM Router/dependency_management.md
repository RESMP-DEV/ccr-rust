# Dependency Management

## System Overview

CCR-Rust is a Rust project managed entirely through Cargo. Dependencies are declared in `Cargo.toml`, pinned by `Cargo.lock`, and constrained by two security policy files — `deny.toml` (cargo-deny) and `osv-scanner.toml` (OSV-Scanner). There is no Python/Node dependency graph; auxiliary scripts under `scripts/` are executed directly with the system Python/Rust toolchain and have no manifest.

## Sources of Truth

| File | Role |
|---|---|
| `Cargo.toml` | Declares direct dependencies, optional features (`dashboard`, `gp`, `sindexer`, `telemetry`), dev-dependencies, and release profile. |
| `Cargo.lock` | Full transitive-graph lockfile used by all build/test commands (`--locked` flags throughout the repo). |
| `vendor/gp-routing/Cargo.lock` | Lockfile for the vendored GP surrogate (referenced from `Cargo.toml` as `path = "vendor/gp-routing"`). |
| `deny.toml` | cargo-deny policy: allowed registries, banned crates, license allowlist, advisory ignores. |
| `osv-scanner.toml` | Time-limited vulnerability exceptions with `ignoreUntil` dates. |
| `docs/dependency-security.md` | Human-readable record of reviewed temporary risk decisions and their expiry dates. |
| `.cargo/mutants.toml` | Configuration for cargo-mutants (test harness, not a dependency source). |

## Allowed Sources

`deny.toml` restricts sources to:
- Crates.io only: `allow-registry = ["https://github.com/rust-lang/crates.io-index"]`
- One private Git repository: `allow-git = ["https://github.com/RESMP-DEV/rust_sindexer.git"]`
- All other registries and Git sources are denied (`unknown-registry = "deny"`, `unknown-git = "deny"`).

The Sindexer crate is pulled from that private GitHub repo at a fixed revision:
```
sindexer = { version = "0.1.0", git = "https://github.com/RESMP-DEV/rust_sindexer.git",
             rev = "d87cf842c08605e98a9d3df48306640f0e567ca7", default-features = false, optional = true }
```
The vendored `gp-routing` package is referenced by path, not registry or Git.

## Versioning Strategy

Versions are mixed between caret ranges and exact pins:
- Caret ranges are used for most dependencies (e.g. `anyhow = "1.0"`, `axum = "0.7"`, `tokio = "1.35"`).
- Exact pins are applied selectively to address known CVEs or incompatibilities, with inline comments citing the advisory:
  - `openssl = "=0.10.80"` — multiple RUSTSEC entries listed inline.
  - `rand = "=0.8.6"` — GHSA-cq8v-f236-94qc.
  - `ratatui = "=0.30.0"` — GHSA-rhfx-m35p-ff5j.
  - OpenTelemetry stack pinned to matching versions: `opentelemetry = "=0.32.0"`, `opentelemetry_sdk = "=0.32.1"`, `opentelemetry-otlp = "=0.32.0"`, `tracing-opentelemetry = "=0.33.0"`, `reqwest-otel = { package = "reqwest", version = "=0.13.5" }`.
- The lockfile is treated as immutable in CI-like flows: validation commands consistently use `--locked`.

## Vendoring

Only one vendored component exists: `vendor/gp-routing`. It is an optional feature-gated dependency (`gp = ["dep:gp-routing"]`) whose own `Cargo.lock` is checked separately (`cargo deny --manifest-path vendor/gp-routing/Cargo.toml check`). This keeps the optional GP-surrogate path isolated from the main graph.

## Security Gates

Two tools enforce policy, each with a distinct role:

1. **cargo-deny** (`deny.toml`)
   - Denies unknown registries and Git sources.
   - Bans multiple versions of the same crate (warn level) and wildcards (deny).
   - Bans known-malicious crates (TeamPCP time utilities, crypto wallet stealers, dependency-confusion examples).
   - License allowlist: MIT, MIT-0, Apache-2.0, AGPL-3.0-or-later, BSD-2-Clause, BSD-3-Clause, CDLA-Permissive-2.0, ISC, MPL-2.0, Unicode-3.0, Unicode-DFS-2016, Zlib, OpenSSL, BSL-1.0.
   - Advisory ignores are hard-coded here because cargo-deny does not read `osv-scanner.toml`.

2. **OSV-Scanner** (`osv-scanner.toml`)
   - Holds dated exceptions with `ignoreUntil = 2026-11-19` for RUSTSEC-2026-0253 (lru/paste), RUSTSEC-2026-0367/0368/0369 (zip via ndarray-npy).
   - Expiry is enforced by OSV-Scanner itself; expired entries fail the scan.

Both files must stay in sync. `deny.toml` documents this explicitly: temporary exceptions mirror `osv-scanner.toml` and `docs/dependency-security.md`, and removing an expiry date requires manual cleanup in both places.

## Conventions and Constraints

- **Direct dependencies are declared in `Cargo.toml`; transitive ones live in `Cargo.lock`.** No `vendor/` directory holds third-party Rust crates except the optional `gp-routing` surrogate.
- **Private sources are whitelisted explicitly.** Only crates.io and `https://github.com/RESMP-DEV/rust_sindexer.git` are allowed; everything else is rejected by cargo-deny.
- **Known-vulnerable pins carry inline RUSTSEC/GHSA citations.** New exact pins should follow the same convention.
- **Temporary risk decisions expire by date.** Exceptions in `osv-scanner.toml` use `ignoreUntil`; `docs/dependency-security.md` records the review date, consumer analysis, and owner/follow-up. A matching entry in `deny.toml` must be removed manually when the date passes.
- **Feature-gated optional dependencies** (`gp`, `sindexer`, `telemetry`, `dashboard`) let consumers opt out of non-core graphs (e.g. the vendored GP surrogate and OpenTelemetry transport).
- **Validation command set** (from `docs/dependency-security.md`) enforces formatting, clippy, tests, `cargo audit`, `cargo deny`, and OSV-Scanner scans against both the main and vendored lockfiles before any merge/release.
- **No private registry tokens or GOPRIVATE-style configuration exist** — authentication for the single allowed Git source would be handled by the host's Git credential store, not by Cargo config in-tree.