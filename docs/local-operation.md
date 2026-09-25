# Local operation and agent use

Use CCR as an **opt-in launch option**. Keep ordinary `claude` and `codex`
commands on their existing providers; select CCR explicitly for a session.
For separate personal and corporate accounts, read [Authentication suites](auth-suites.md)
before adding credentials or fallback routes.
See [Switching plans](switching-plans.md) for Kimi/GLM commands and the
configuration needed for Codex tool calls through Kimi.

## Existing workstation setup

These helpers were installed on the maintainer's Mac on September 17, 2026.
They are local scripts, **not commands installed by `cargo install`**. Check
availability before using them:

```bash
command -v ccr-rust ccr-local claude-ccr codex-ccr
ccr-local status
ccr-local validate
```

| Command | Purpose |
| --- | --- |
| `claude-ccr` | Start the local service if needed, launch Claude Code through CCR |
| `codex-ccr` | Start the local service if needed, launch Codex with the `ccr` profile |
| `ccr-local start` / `stop` / `restart` | Control the launchd service |
| `ccr-local status` / `validate` | Check the listener or load and validate its configuration |

The configured listener is `http://127.0.0.1:3456`. The original setup routes
Claude Code to Z.AI GLM-5.3 and Codex to Azure GPT-6 Astra. Treat these as a dated
setup record; inspect the active config and live route counters before claiming
which backend a session used.

| Local file | Responsibility |
| --- | --- |
| `~/.claude-code-router/config.json` | Providers and routing; `${ENV_VAR}` credential placeholders |
| `~/.claude-code-router/runtime-credentials.json` | Private credential values, mode `0600`; never print or commit |
| `~/.claude-code-router/serve.py` | Load the allowed credentials and execute CCR |
| `~/Library/LaunchAgents/com.kearm.ccr-rust.plist` | Start at login and restart after exit |
| `~/.claude-code-router/logs/` | Service logs; inspect locally, redact before sharing |
| `~/.local/bin/{ccr-local,claude-ccr,codex-ccr}` | Service and opt-in client launchers |
| `~/.codex/ccr.config.toml` | CCR provider profile for the installed Codex fork |
| `~/.claude-code-router/LOCAL_SETUP.md` | Machine-specific setup and acceptance receipt |

The service loads `runtime-credentials.json`, not older `.env` files. Changes
to a file do not change a running process until it is restarted. Coordinate a
restart with other active work. The local Codex fork's `--ignore-user-config`
also skips named profiles; do not combine it with `--profile ccr`.

## Set up another machine

The local helpers above are optional. The portable path is:

1. Build and install from the intended checkout:

   ```bash
   git status --short --branch
   git rev-parse HEAD
   cargo build --release --locked
   cargo install --path . --force --locked
   command -v ccr-rust
   ```

   Add `--all-features` to both Cargo commands when you need the optional
   features. Compare the built and installed binary hashes after installation
   (`shasum -a 256` on macOS, `sha256sum` on Linux).

2. Follow [Configuration](configuration.md) to create a minimal config outside
   the repository, retaining only providers you actually use. Back up an
   existing config before changing it. Load its credential variables into the
   **router process**. CCR expands placeholders at startup but does not load
   `.env` automatically. See [suite startup](auth-suites.md#start-one-api-suite)
   for a guarded launch example.

3. Validate and start on loopback, in a separate terminal:

   ```bash
   ccr-rust --config "$HOME/.claude-code-router/config.json" validate
   ccr-rust --config "$HOME/.claude-code-router/config.json" start --host 127.0.0.1 --port 3456
   ```

   Use explicit `--host` and `--port` when running multiple instances. A passing
   `validate` checks configuration structure; it does not authenticate to the
   upstream or prove that every environment variable expanded.

4. Use the command-scoped instructions for [Claude Code](claude_code_setup.md)
   or [Codex](codex_setup.md). For persistent service management, see
   [Deployment](deployment.md); preserve the same config, environment, binary,
   and port when moving from a foreground process to a service.

## Prove the selected path

```bash
curl --fail --silent http://127.0.0.1:3456/health
curl --fail --silent http://127.0.0.1:3456/v1/models
curl --fail --silent http://127.0.0.1:3456/v1/frontend-metrics
curl --fail --silent http://127.0.0.1:3456/v1/usage
```

Health proves reachability; `/v1/models` lists configured routes, not live
provider entitlement. Compare frontend and per-tier counters before and after
a client request. To verify tools, create a temporary file containing a random
marker and ask the client to **read the file with a tool** and return its exact
contents. Inspect the client trace for the tool call and result. Do not include
the marker in the prompt. A text completion alone does not test tool use.

Use a disposable directory without private project content. External MCPs and
project instructions can change the test, so record whether they were enabled.
The optional CCR MCP daemon is not needed for Claude's built-in `Read` tool or
Codex's shell tool.

## Recorded acceptance, September 17, 2026

At source commit `ee0fec7c8091cbef85d38de392c7ea18e2f5e514`, the all-feature
release build and installed binary matched SHA-256
`98a21d177989b5f61b578a5efb1cebc2a4008a3e9972e05c65a6bdc94345a7cf`.
Actual Claude Code **2.1.276** and the local Codex development build **0.0.0**
each completed a file-reading tool round trip through CCR and a live upstream:
Claude used `Read` through Z.AI; Codex used its shell through Azure. Exact random
file contents, client traces, and CCR counters agreed. Each used two upstream
requests; both exited successfully. External MCPs and repository instructions
were isolated for this test.

The private local receipt is `~/.cache/ccr-live-20260917/acceptance.json`.
The earlier 560-test repository run used mocked upstreams. These are distinct
forms of evidence; neither guarantees every future client version or long
interactive session. Recheck the installed hash and runtime after another
checkout installs a binary. This record is not a claim about the binary
currently installed on every machine.
