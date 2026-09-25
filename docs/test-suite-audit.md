# Test-suite duplication audit

## Scope

The September 24, 2026 audit covered the full all-features suite, not only the
new worker inspector. At worker-inspection commit `77ee5eb`, 649 tests passed
across 21 Cargo test binaries, with one pre-existing ignored doctest. Every
executed test name occurred once. This rules out duplicated execution in that
run, not semantic overlap between differently named tests.

## Findings and disposition

| Finding | Disposition |
| --- | --- |
| Identical fixtures in the two Codex streaming targets | Share the app builder, request body, request execution, SSE parser, and localhost-bind guard; parameterize the config's upstream protocol. |
| Eleven integration targets define `build_app` | Consolidate the identical stream pair first. Other route sets and state customization need individual comparison, not blanket replacement. |
| Legacy and canonical OpenAI-to-Anthropic response tests overlap | Retain both: `transform::openai` and `transform::openai_to_anthropic` are distinct public implementations. Removing the legacy module would change the API, not just remove redundant tests. |
| `tests/mocks/claude_code_client.rs` is not wired into `tests/lib.rs` | Treat as unused scaffolding, not duplicated runtime coverage; leave it outside this cleanup. |
| Frontend detector tests resemble integration detection tests | Retain both: detector predicates and dispatcher precedence are different contracts. |

Both protocol-specific upstream servers and all seven streaming tests retain
their assertions. Support modules contain no tests, so importing them into two
targets does not multiply execution. Routing precedence, 429/tier cascade,
reasoning controls, tool conversion, cancellation, and protocol-specific
streaming controls remain separate regression cases.

Review also exposed an existing whitespace bug in the copied SSE fixture
parser. The shared parser now strips only the one optional space after `data:`,
preserving additional spaces and tabs. Table-driven checks inside the existing
OpenAI stream test cover those boundaries, empty fields, and multiline CRLF
frames without adding a new test target or duplicating test execution.

## Validation workflow

Use focused tests while editing:

```sh
cargo test --locked --test integration_codex_stream --test integration_codex_openai_stream
```

Run `cargo test --all-features --locked` once as the broader regression gate.
This test-only cleanup starts from main at `b1d268d` (610 tests); the 649-test
audit snapshot additionally contains the separate worker-inspection changes.
There is no claim that sharing fixtures reduces runtime test count or produces
a measured speedup. The benefit is maintaining each identical fixture once
without deleting distinct coverage.
