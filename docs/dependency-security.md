# Dependency security decisions

This page records temporary dependency-risk decisions that cannot be resolved
by a compatible lockfile update. Narrow OSV exceptions may point here, but they
must expire so the live graph is reviewed again. `cargo audit` and `cargo deny
check` continue to report the graph, and newly actionable vulnerabilities
remain release blockers.

## 2026-10-19 review

### `rustls` and QUIC advisories

Resolved in `Cargo.lock`. `rustls` is pinned to 0.23.46, clearing
RUSTSEC-2026-0346 through RUSTSEC-2026-0349. `quinn-proto` is pinned to
0.11.19, clearing RUSTSEC-2026-0338 through RUSTSEC-2026-0342. The Sindexer
revision remains unchanged.

### `zip` RUSTSEC-2026-0367 through RUSTSEC-2026-0369

Temporarily accepted until 2026-11-19. `zip` 2.4.2 is reached only through
`ndarray-npy` 0.9.1 in `egobox-gp`; `ndarray-npy` uses `ZipArchive` and
`ZipWriter` for NPZ I/O but does not call `ZipArchive::extract` or
`ZipStreamReader::extract`. All three advisories require one of those
extraction paths, so they are not reachable from CCR-Rust or the vendored GP
surrogate. A compatible `ndarray-npy` update is still unavailable: 0.10.0 is
a new semantic-version line and current `egobox-gp` releases, including
0.37.1, continue to require 0.9.1 with `zip` 2.x.

Owner: CCR-Rust maintainers. Follow-up: when `egobox-gp` or `ndarray-npy`
publishes a compatible `zip` 9 graph, update and remove all three OSV
exceptions. The exception expires immediately if CCR or the vendored GP
package begins extracting an archive supplied by a request, model artifact,
or untrusted file.

### `lru` and `paste` recheck

Ratatui Core was updated from 0.1.0 to 0.1.2 and now uses `lru` 0.18.5. The
older affected `lru` 0.16.4 remains reachable through the pinned Sindexer's
Tantivy 0.26.1, which requires exactly that release. The panic-safety
preconditions remain outside CCR's reachable key and unwind behavior, and the
exception is extended only to 2026-11-19.

`egobox-gp` 0.37.1 and Linfa PLS still require the unmaintained `paste`
procedural macro, and no patched `paste` release exists. The build-time-only
risk decision therefore also expires on 2026-11-19.

## 2026-09-17 review

### `rustls` RUSTSEC-2026-0285

Resolved. `Cargo.lock` pins `rustls` 0.23.45 and its required
`rustls-webpki` 0.103.15 update. Rustls previously accepted TLS 1.3 handshake
messages across encryption-level boundaries. The locked 0.23.39 release was
affected; 0.23.45 is the advisory's fixed floor. Reqwest reaches this TLS stack
through Sindexer. The reviewed Sindexer Git revision remains unchanged.

### Local exception enforcement

A release operator installs OSV-Scanner 2.6.0 or newer and runs the local
command in the section above before building or publishing artifacts. The
explicit lockfile input also works in checkouts beneath hidden directories.
No GitHub Actions job performs this check.

GitHub Actions are disabled for this repository. Before submitting changes,
run the local validation commands:

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo audit --file Cargo.lock \
  --ignore RUSTSEC-2026-0367 \
  --ignore RUSTSEC-2026-0368 \
  --ignore RUSTSEC-2026-0369
cargo deny check
osv-scanner scan source --config osv-scanner.toml --lockfile Cargo.lock
```

Historical enforcement note from 2026-09-17: OSV-Scanner 2.6.0 honored
`IgnoredVulns.ignoreUntil`, and a probe with an already-expired `paste`
exception failed as intended. That behavior remains the enforcement contract.
The current expiry dates and reviewed consumers are the 2026-11-19 entries
above; the original 2026-10-01 dates are no longer current.

### Historical remaining-migration note

The September refresh found Ratatui Core 0.1.2 support for `lru` 0.18 while
Tantivy 0.26.2 still required the 0.16 line, and Egobox GP 0.36.3 still
required `paste`. The October recheck above supersedes those version
observations without changing the need to recheck every dependency refresh. Redis 1.7.0 is available, but requires
the separately tested persistence migration described below.

## 2026-08-23 review

### `h2` RUSTSEC-2026-0258

Resolved. `Cargo.lock` pins `h2` 0.4.18, above the advisory's fixed floor of
0.4.16. The dependency is reachable through Hyper, Reqwest, Axum, Sindexer,
and test-only Wiremock paths, so both router clients and servers required the
update.

### `lru` RUSTSEC-2026-0253

Temporarily accepted until the upstream consumers support `lru` 0.18.2 or
later. The locked 0.16.4 release is reached through both `ratatui-core` 0.1.0
and `tantivy` 0.26.1 via Sindexer; both constrain the dependency to the 0.16
line, where no patched release exists.

The unsafe state requires `LruCache::pop()` to unwind while dropping a key and
then requires later cache mutation. The reachable caches use `usize` keys in
Tantivy and `(Rect, Layout)` keys in Ratatui, not application-defined keys with
panicking `Drop` implementations. CCR-Rust does not catch an unwind around
these caches. This makes the advisory preconditions unreachable in the
reviewed graph, but it does not make the affected crate sound.

Owner: CCR-Rust maintainers. Follow-up: recheck Ratatui, Tantivy, and Sindexer
on every dependency refresh and migrate as soon as both inbound paths accept
`lru >= 0.18.2`. If either path begins using a key with custom `Drop` behavior,
the temporary acceptance expires immediately.

### `paste` RUSTSEC-2024-0436

Temporarily accepted as an unmaintained build-time procedural macro. It is
reached through `egobox-gp` 0.36.1 and `linfa-pls` 0.8.1 in the optional GP
feature. The live crates.io API reports 0.36.1 as the newest `egobox-gp`
release, and the advisory provides no patched `paste` version. The macro runs
while compiling the GP graph and adds no runtime request-processing surface.

Owner: CCR-Rust maintainers. Follow-up: replace `paste` with a maintained fork
at the vendored GP boundary or consume an upstream `egobox-gp`/`linfa-pls`
release that removes it. Reassess before modifying the vendored surrogate or
publishing the GP package independently.

### Redis client migration

The lockfile now uses Redis 0.24.1, which clears the Rust never-type fallback
future-incompatibility warning emitted by 0.24.0. Redis remains a direct
dependency used by metrics persistence and administrative cleanup. The
compatible 0.24 line is substantially behind the 1.6.0 release reported by the
live crates.io API, so a major upgrade remains separate from this HTTP/2
security lockfile repair.

Owner: CCR-Rust maintainers. Follow-up: upgrade to the current Redis client in
a dedicated change, exercise snapshot load/write/cleanup against a disposable
Redis server, run all persistence tests, and require a warning-free release
build on both supported targets before merging.
