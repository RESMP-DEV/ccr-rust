## Build Approach

The project is a Rust binary (`ccr-rust`) built entirely through **Cargo**. There are no Makefiles, shell build scripts, or external build orchestrators at the repository root. The build surface consists of:

- `Cargo.toml` — package metadata, dependencies, features, and release profile.
- `build.rs` — compile-time script that captures the current Git branch into the `GIT_BRANCH` environment variable (used by the dashboard and `/health` endpoint).
- `Dockerfile` — two-stage Docker image using `rust:1.88-bookworm` for the builder stage and `debian:bookworm-slim` for the runtime.
- `deny.toml` + `osv-scanner.toml` — supply-chain security policy for `cargo-deny` and GitHub OSV Scanner.
- `ruff.toml` — Python linting configuration for auxiliary scripts under `scripts/`, `examples/`, and `benchmarks/`.
- `.cargo/mutants.toml` — configuration for `cargo-mutate` fuzzing.

No CI pipeline files were found under `.github/workflows/`; the repository snapshot does not include an active CI configuration in this tree.

## Key Files

| File | Role |
|---|---|
| `Cargo.toml` | Declares package version `1.3.0`, edition 2021, optional features (`dashboard`, `gp`, `sindexer`, `telemetry`), dev-dependencies (`criterion`, `proptest`, `wiremock`), and a single Criterion benchmark `benches/concurrent_streams.rs`. |
| `build.rs` | Runs `git branch --show-current` at build time and emits `GIT_BRANCH` via `cargo:rustc-env=...`; reruns when `.git/HEAD` changes. |
| `Dockerfile` | Builds release binary then copies it into a non-root user `ccr` container; exposes port 3456 with a healthcheck against `http://localhost:3456/health`. |
| `deny.toml` | Policy file for `cargo deny check`: denies unknown registries/git sources except crates.io and one RESMP-DEV git repo, allows only a whitelist of licenses, bans specific malicious crates, and sets `multiple-versions = "warn"` plus `wildcards = "deny"`. |
| `osv-scanner.toml` | Time-bounded OSV Scanner ignore list (`ignoreUntil = 2026-11-19`) mirroring temporary exceptions declared in `deny.toml`; kept in step per the comment in `deny.toml`. |
| `ruff.toml` | Ruff configuration used to lint the Python tooling in `scripts/` and `examples/smoke-test.sh` helper scripts. |
| `.cargo/mutants.toml` | Configuration for mutation testing via `cargo-mutants`. |

## Architecture & Conventions

### Feature-flagged compilation
Optional functionality is gated behind Cargo features defined in `Cargo.toml`:
- `default = ["dashboard", "gp", "sindexer"]`
- `dashboard` pulls in `reqwest/blocking`
- `gp` pulls in the vendored `vendor/gp-routing` crate
- `sindexer` pulls in a git-pinned `rust_sindexer` dependency
- `telemetry` pulls in the full OpenTelemetry stack (`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry`, `reqwest-otel`, `http-body`)

This lets consumers opt out of heavy subsystems (e.g. telemetry) at compile time.

### Release profile
`[profile.release]` enables `opt-level = 3`, `lto = true`, and `codegen-units = 1` — producing a single large, fully LTO-optimized binary suitable for deployment.

### Vendored dependencies
The Dockerfile copies a `vendor/` directory and the build relies on it (the `gp-routing` and `sindexer` dependencies are referenced via `path = "vendor/gp-routing"` and a pinned git revision). This pins transitive dependency trees outside of `Cargo.lock` for reproducible builds.

### Compile-time version info
The `build.rs` script embeds the Git branch name at compile time so the runtime dashboard can display it. Consumers should use `option_env!("GIT_BRANCH")` (as done in `src/dashboard.rs`) rather than hardcoding it.

### Dependency security model
Two complementary tools enforce supply-chain policy:
1. `cargo deny` (via `deny.toml`) — enforced at build time; disallows unknown registries/git sources, bans known-malicious crates, restricts licenses to a curated allowlist.
2. GitHub's OSV Scanner (via `osv-scanner.toml`) — scans for vulnerabilities with expiry dates.

Temporary advisory ignores are duplicated across both files and must be removed together when their `ignoreUntil` date passes; the `deny.toml` comments explicitly state this coordination requirement.

## Conventions & Constraints

- **Version numbering** is a plain semver string in `Cargo.toml` (`version = "1.3.0"`). No version bump script was found.
- **Feature gates**: all optional subsystems are enabled exclusively through Cargo features; there are no `#[cfg(feature = ...)]` blocks scattered without corresponding feature declarations.
- **Dependency pinning**: several critical dependencies are pinned to exact versions in `Cargo.toml` (e.g. `openssl = "=0.10.80"`, `rand = "=0.8.6"`, `ratatui = "=0.30.0"`, OpenTelemetry crates pinned to `=0.32.x`), typically with CVE rationale comments next to them.
- **Allowed registries**: `deny.toml` enforces `unknown-registry = "deny"` and `unknown-git = "deny"`; only `crates.io` and `https://github.com/RESMP-DEV/rust_sindexer.git` are whitelisted. Any new source requires editing `deny.toml`.
- **License policy**: only the explicit allowlist in `deny.toml` is permitted; adding a dependency with an unlisted license will fail `cargo deny check`.
- **Multiple versions warning**: `multiple-versions = "warn"` means duplicate versions of the same crate are flagged but do not fail the build; `wildcards = "deny"` forbids wildcard version specifiers.
- **Docker build reproducibility**: the builder stage copies `Cargo.toml` and `Cargo.lock` before source code to leverage Docker layer caching, and uses a pinned base image `rust:1.88-bookworm`.
- **Container entrypoint**: the runtime image defaults to `ccr-rust start --host 0.0.0.0` and expects port 3456; the healthcheck targets `/health`.
- **Python tooling linting**: auxiliary scripts are linted with Ruff via `ruff.toml` (no Cargo equivalent); this is separate from the Rust build.
- **Benchmark harness**: benchmarks are declared as standalone binaries in `Cargo.toml` (`[[bench]] harness = false`), running `benches/concurrent_streams.rs` directly with the Criterion harness.