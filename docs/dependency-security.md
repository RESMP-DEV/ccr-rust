# Dependency security decisions

This page records temporary dependency-risk decisions that cannot be resolved
by a compatible lockfile update. Narrow OSV exceptions may point here, but they
must expire so the live graph is reviewed again. `cargo audit` and `cargo deny
check` continue to report the graph, and newly actionable vulnerabilities
remain release blockers.

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
