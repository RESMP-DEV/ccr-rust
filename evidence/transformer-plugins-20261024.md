# Runtime transformer plugins, 2026-10-24

Implemented by ZCode (GLM-5.3-Flash) on branch `feature/transformer-plugins`
(worktree `.worktrees/transformer-plugins`, base 259b452).

## What was done

Restored the Node.js router's runtime plugin extensibility on the Rust
router: executable request/response transformers registered at startup from
config (`Plugins` section) and a drop-in plugin directory, wired into both
transformer registry construction paths, validated at config load, and
inspectable through a new `ccr-rust plugins [--json]` command.

- `src/config/plugins.rs` — `Plugins` section types (`PluginSpec`,
  `PluginsConfig`, `PluginStages`, `PluginOnError`), validation, defaults
  (timeout 10 s, stages both, on_error passthrough).
- `src/transformer/external.rs` — `ExternalTransformer`: stdin/stdout JSON
  protocol, `CCR_STAGE`/`CCR_PLUGIN_NAME` context, direct spawn (no shell),
  timeout with process-group teardown, bounded pipe drain, passthrough/fail
  error handling.
- `src/transformer/plugin_catalog.rs` — drop-in directory scan, stem
  collision dedupe, config-over-directory and builtin-over-both precedence.
- `src/transformer/mod.rs`, `src/transform/registry.rs` — both registry
  construction paths register plugins (built-ins always win); the factory
  registry key type moved from `&'static str` to `String` for dynamic names.
- `src/config/validate.rs` — new module; the validation gates were split out
  of `config/mod.rs` per the SLOC ratchet (config/mod.rs 1414 → 1172 lines).
- `src/main.rs` — `Plugins` CLI command, `run_server` registers plugins.
- Docs: `docs/plugins.md` (protocol, config, failure semantics,
  limitations), `docs/configuration.md`, `docs/index.md`,
  `examples/plugins/` (strip-cache-control.sh, log-payload.sh),
  `config.example.json`.
- Dependency: `libc = "0.2"` (already locked transitively; lockfile gains
  only the direct-dep edge, no version churn).

## Evidence

- `python3 scripts/quality.py`: 12/12 PASS (format, python checks, sloc
  ratchet, shellcheck, clippy, rust-tests, no-default-features,
  debug-binary, python-helpers, vendored-gp-routing). Receipt:
  `build/reports/quality/`.
- `cargo test --locked --all-features`: 657 lib + integration suites, 0
  failed. New coverage: 14 external.rs, 7 plugin_catalog.rs, 10
  config/plugins.rs, 2 factory-registry plugin tests.
- Timeout teardown proven live: a `sleep 30` plugin with `timeout_ms: 100`
  is killed by process group in 0.11 s (initial implementation blocked on
  orphaned grandchildren holding pipes; fixed with `process_group(0)` +
  group SIGKILL before reap).
- Live CLI: `ccr-rust --config config.example.json plugins` lists 16
  built-ins; a temp config with a drop-in dir lists `strip-cache-control`
  (source directory) and a declared plugin (source config, stages request);
  `validate` exits 1 with
  `Plugins.plugins.broken: command '/nonexistent/plugin' does not exist`
  for a broken declaration.
- `python3 scripts/check_sloc.py`: 42,242/42,242. Total raised for the
  feature; `config/mod.rs` pin lowered after the validate split.

## Non-claims

- No live router round trip through a plugin was run: the production
  listener still runs the previously installed binary, and a real upstream
  round trip needs live credentials. Mocked-upstream integration tests plus
  the CLI-level live runs above are the evidence. Plugin activation on the
  running service happens at the next idle-gated restart, per
  `docs/idle-restart.md`.
- Plugin payload transform latency depends on the external program; no
  performance measurement was taken.
- The `transform/registry.rs` factory path registers plugins, but its live
  consumers were not re-audited beyond compilation and existing tests.
