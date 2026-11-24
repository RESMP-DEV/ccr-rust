# Configuration System

## Approach

CCR-Rust loads runtime configuration from a single JSON file using `serde_json` + `serde` derive macros. The loader performs a multi-stage pipeline:

1. Read the raw JSON file.
2. Parse into a generic `serde_json::Value`.
3. Recursively expand `${VAR}` and `$VAR` references in every string value via `shellexpand::env`, recording failures per JSON pointer.
4. Validate credentials (provider keys, client key) against the recorded expansion failures — unexpanded credential placeholders abort startup unless overridden.
5. Deserialize into typed structs (`ConfigFile`, `Provider`, `RouterConfig`, etc.).
6. Run domain validators: listener port conflicts, provider contracts, model aliases, retry sweeps/budgets, build-feature checks.
7. Wrap into a runtime `Config` that holds an `Arc`'d inner state plus a shared `reqwest::Client` connection pool and presets lookup.

This is a **configuration-as-data** approach (no `.toml`/`.yaml`), not a framework-driven config system. The only external dependency for env expansion is `shellexpand`.

## Key Files

- `src/config/mod.rs` — Loader, env expansion, all validators, `ConfigFile` schema, `Config` runtime wrapper, preset resolution, `from_file` entry point.
- `src/config/types.rs` — Typed schemas: `Provider`, `RouterConfig`, `PersistenceConfig`, `DebugCaptureConfig`, `PresetConfig`, `TransformerEntry`, `ProviderTransformer`, `ModelTransformerConfig`, `TransformerUseList`, and custom deserializers.
- `src/main.rs` — CLI (`clap`) wiring; `--config` / `CCR_CONFIG` flag feeds into `Config::from_file`; subcommands `start`, `validate`, `doctor`, `dashboard`, `clear-stats`, `mcp` consume `Config`.
- `docs/configuration.md` — Authoritative user-facing schema and field documentation.
- `examples/meta.json` — Canonical example config showing providers, router tiers, persistence mode, HOST/PORT.

## File Location Resolution

The config path is resolved in this order (highest priority first):

| Source | Mechanism |
|---|---|
| CLI flag | `--config <path>` |
| Environment | `CCR_CONFIG=<path>` (declared on the clap arg as `env = "CCR_CONFIG"`) |
| Default | `~/.claude-code-router/config.json` |

Other runtime knobs are also exposed as CLI flags or env vars (e.g. `--host`, `-p/--port`, `--max-streams` mapped to `CCR_MAX_STREAMS`, `--redis-url` mapped to `CCR_REDIS_URL`, `--dashboard-host`/`--dashboard-port` mapped to `CCR_DASHBOARD_HOST`/`CCR_DASHBOARD_PORT`).

## Schema & Top-Level Structure

`ConfigFile` maps directly onto the documented JSON shape:

- `Providers`: list of upstream API endpoints (name, api_base_url, api_key, models, pricing, model_pricing, transformer, force_reasoning_effort, force_adaptive_thinking, maxInflight, maxInflightTokens).
- `Router`: default/background/think/webSearch routes, GP routing, tier retries.
- `Presets`: named route+parameter overrides (`route`, optional `max_tokens`, `temperature`).
- `Persistence`: Redis-backed metric persistence settings.
- `DebugCapture`: raw request/response capture settings.
- Top-level scalar fields: `HOST`, `PORT`, `AUTH_HOST`, `AUTH_PORT`, `API_TIMEOUT_MS`, `PROXY_URL`, `POOL_MAX_IDLE_PER_HOST`, `POOL_IDLE_TIMEOUT_MS`, `SSE_BUFFER_SIZE`, `MAX_REQUEST_BODY_BYTES`, `CLIENT_API_KEY`, `BROKER_SOCKET`.

Fields use explicit `#[serde(rename = ...)]` so the JSON uses PascalCase while Rust fields stay snake_case.

## Environment Variable Expansion

- Syntax: `${VAR_NAME}` and bare `$VAR` (alphanumeric/underscore after `$`).
- Performed by `expand_env_references`, which walks the parsed JSON tree recursively and replaces each string value in place.
- Missing variables do **not** abort parsing — they are recorded as `(json_pointer, error)` pairs and logged at warn level. Startup is then gated by credential-specific validators.
- The override `CCR_ALLOW_UNEXPANDED_CREDENTIALS=true|1|yes|on` suppresses the credential gate for local setups where literal placeholders are intentional.

## Validation Pipeline

Validators run in `Config::from_file` after env expansion but before deserialization into typed structs:

1. `validate_provider_credentials` — rejects configs where any `api_key` or `extra_headers` value failed env expansion, unless overridden.
2. `validate_provider_extra_headers` — validates each header name via `reqwest::header::HeaderName::from_bytes` and each value via `HeaderValue::parse`.
3. `validate_client_credentials` — same check for `CLIENT_API_KEY`.
4. Deserialization into `ConfigFile`.
5. `validate_client_listener` — enforces that when `CLIENT_API_KEY` is set, `AUTH_HOST` is non-empty and `AUTH_PORT` is present (the normal HOST/PORT listener remains unauthenticated).
6. `validate_provider_contracts` — per-provider validation (documented elsewhere).
7. `validate_model_aliases`, `validate_retry_sweeps`, `validate_retry_budget` — router-domain checks.
8. `Config::validate_build_support` — feature-gated checks (e.g. `gpRouting.enabled=true` requires the `gp` Cargo feature; `kplsDim` must be between 1 and `FEATURE_DIM`).

There is also a dedicated `ccr-rust validate` subcommand that calls `validate_config` (which invokes the same `Config::from_file` pipeline) without starting the server.

## Presets

Named route shortcuts declared under `Presets` map a string key to a `PresetConfig` containing `route` (a `provider,model` pair) plus optional `max_tokens` and `temperature` overrides. They are stored in a `HashMap<String, PresetConfig>` and accessible via `Config::get_preset` / `preset_names`.

## Transformer Configuration

Transformers are configured per-provider via a nested `transformer` object mirroring the Node.js config:

- `use`: ordered chain of `TransformerEntry`s (either a name string or `["name", options] tuple).
- `<model-name>` keys: model-specific `use` overrides.

Custom deserializers in `types.rs` handle the dual form of `TransformerEntry` and the mixed-key `ProviderTransformer` (where `"use"` is special and other keys are model overrides). Helper methods `is_sole_transformer` and `should_bypass` mirror the Node.js bypass/passthrough semantics.

## Runtime Access

After loading, callers receive a cloneable `Config` (wrapped in `Arc<ConfigInner>`) exposing accessors like `providers()`, `router()`, `http_client()`, `sse_buffer_size()`, `client_api_key()`, `broker_socket()`, `max_request_body_bytes()`, `persistence()`, `debug_capture()`, `auth_host()`, `auth_port()`, and `get_preset()`.

## Constraints & Rules

- Config file must be valid JSON parseable by `serde_json` — enforced by `from_str` in `from_file`.
- Unexpanded credential placeholders in `api_key`, provider `extra_headers`, or `CLIENT_API_KEY` abort startup with a diagnostic listing the offending provider/header, unless `CCR_ALLOW_UNEXPANDED_CREDENTIALS` is truthy.
- When `CLIENT_API_KEY` is set, `AUTH_HOST` must be non-empty and `AUTH_PORT` must be provided — enforced by `validate_client_listener`.
- Provider `extra_headers` keys must be valid HTTP header names and values must be valid `HeaderValue`s — enforced during load.
- `force_reasoning_effort` values must be one of `none|minimal|low|medium|high|xhigh|max` — validated via the `REASONING_EFFORT_VALUES` constant.
- `maxInflight` per provider must be 1–64; `maxInflightTokens` must be 1000–4000000 — documented in `docs/configuration.md` and enforced in provider contract validation.
- `MAX_REQUEST_BODY_BYTES` defaults to 64 MiB (`DEFAULT_MAX_REQUEST_BODY_BYTES`) and is clamped to a hard ceiling of 1 GiB (`HARD_MAX_REQUEST_BODY_BYTES`).
- `Router.gpRouting.enabled=true` requires the `gp` Cargo feature; if enabled, `kplsDim` must be between 1 and `gp_routing::features::FEATURE_DIM` — enforced by `validate_build_support`.
- The broker socket resolves via precedence: config field `BROKER_SOCKET` > environment variable `CCR_BROKER_SOCKET` — implemented in `Config::broker_socket`.

## What Is Not Present

- No `.env` file parser — CCR-Rust reads from the process environment only; users must source their own `.env` before launching.
- No YAML/TOML support — the format is fixed to JSON.
- No secret manager integration (Vault, AWS Secrets Manager, etc.) — secrets enter only via environment variables or the JSON file itself.
- No hot-reload — the config is loaded once at startup.