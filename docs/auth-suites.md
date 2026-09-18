# Personal and corporate authentication suites

Separate plans are possible, but CCR does not have a built-in account vault or
`login`/`suite` command. Use a separate configuration, credential store, and
launch target for each account. Choose the suite before starting a session.

| Account type | Supported approach | What CCR does |
| --- | --- | --- |
| Provider API keys, including coding-plan API endpoints | One CCR process/config/port per suite | Sends that suite's configured key to its supported upstream API |
| Claude subscription or corporate Claude login | Separate Claude config and credential directories; sign in with Claude | No browser login, subscription discovery, or OAuth refresh |
| ChatGPT personal or corporate workspace login for Codex | Separate Codex homes; sign in with Codex | No ChatGPT login, workspace selection, or OAuth refresh |

Native subscription sessions use the client's native provider. A subscription
login is not an API key and is not automatically available to a CCR custom
provider. A suite can contain both native client homes and CCR API credentials,
but those remain separate authentication paths and may have separate billing.

## Why one shared fallback list is insufficient

In CCR, an explicit `provider,model` route already present in `Router.tiers`
is moved to the front **with the remaining fallback candidates retained**.
For example, `Router.tiers: ["personal,model", "corporate,model"]` can send
a personal request to corporate when personal returns a 429. A direct route
outside that list normally becomes a single candidate, but `ignoreDirect`,
search routing, and future config changes make model selection unsuitable as
an account boundary.

For accounts that must not mix, run two processes with disjoint provider lists.
Keep all routes in each process—default, tiers, think, background, web search,
and presets—within that suite. Missing credentials or exhausted capacity should
produce a failure for that suite, not selection of another account.

If you deliberately combine accounts in one process, give every provider a
unique `name` and every account a distinct `tier_name` (or omit `tier_name` to
use the provider name). Lookup selects providers by name; tier names also key
rate-limit tracking and metrics. Shared tiers permit cross-account fallback.

## API-key suite layout

```text
~/.claude-code-router/suites/
  personal/
    config.json
    credentials.env
    logs/
  corporate/
    config.json
    credentials.env
    logs/
```

Use mode `0700` directories and mode `0600` credential files. Keep them outside
Git. Store secrets with an editor or your secret manager; do not put real keys
in shell history, client flags, issue comments, or documentation. Each
`credentials.env` supplies the same variable name with a **different value**:

```bash
CCR_SUITE_API_KEY='replace-with-this-suites-key'
```

For example, `personal/config.json` can contain:

```json
{
  "Providers": [{
    "name": "zai-personal",
    "api_base_url": "https://api.z.ai/api/anthropic/v1",
    "api_key": "${CCR_SUITE_API_KEY}",
    "protocol": "anthropic",
    "auth_header": "authorization",
    "models": ["glm-5.3"],
    "tier_name": "zai-personal"
  }],
  "Router": {
    "default": "zai-personal,glm-5.3",
    "tiers": ["zai-personal,glm-5.3"],
    "ignoreDirect": false
  },
  "Persistence": {"mode": "none"},
  "DebugCapture": {"enabled": false},
  "HOST": "127.0.0.1",
  "PORT": 3458
}
```

This is an endpoint/model example, not an entitlement check. Use the endpoint,
model, protocol, and headers exposed by your plan. For corporate, create an
independent config with corporate names and credentials and port `3459`.
An OpenAI-compatible provider uses `protocol: "openai"`; a Responses upstream
uses `"responses"`. See [provider configuration](configuration.md).

### Start one API suite

Create and populate the directories and files above first. Run this for
personal in its own terminal; use `corporate` and `3459` in another terminal:

```bash
suite_dir="$HOME/.claude-code-router/suites/personal"
suite_port=3458
env -i PATH="$PATH" HOME="$HOME" CCR_SUITE_DIR="$suite_dir" CCR_SUITE_PORT="$suite_port" \
  /bin/bash --noprofile --norc -c '
    set -eu
    umask 077
    cd "$CCR_SUITE_DIR"
    set -a
    . ./credentials.env
    set +a
    : "${CCR_SUITE_API_KEY:?Missing suite API key}"
    ccr-rust --config ./config.json validate
    exec ccr-rust --config ./config.json start --host 127.0.0.1 --port "$CCR_SUITE_PORT"
  '
```

`credentials.env` is trusted shell code: source only the private file you
created. The clean launch environment prevents unrelated provider secrets from
being inherited. The explicit missing-key check matters because CCR currently
warns and retains raw config text if environment expansion fails. Repeat the
check for every credential placeholder if you add providers or extra headers.
Do not treat `validate` alone as proof of secret availability.

Use `Ctrl-C` to stop each foreground instance. For services, create distinct
launchd labels or systemd units, ports, config paths, credential environments,
and log paths. If enabling Redis persistence, give suites distinct
`Persistence.redis_prefix` values; otherwise their persisted metrics and EWMA
state can mix. Keep capture directories and MCP credentials separate too.

Point each client at the selected port using [Claude Code](claude_code_setup.md)
or [Codex](codex_setup.md). For example:

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:3458 \
ANTHROPIC_AUTH_TOKEN=ccr-local ANTHROPIC_API_KEY=ccr-local \
ANTHROPIC_MODEL=zai-personal,glm-5.3 \
ANTHROPIC_DEFAULT_OPUS_MODEL=zai-personal,glm-5.3 \
ANTHROPIC_DEFAULT_SONNET_MODEL=zai-personal,glm-5.3 \
ANTHROPIC_DEFAULT_HAIKU_MODEL=zai-personal,glm-5.3 claude
```

Separate ports prevent accidental routing across disjoint configs. They are
**not an access-control boundary**: CCR's HTTP API does not authenticate local
clients, and the dummy client token does not protect a suite. Keep listeners
on loopback. For isolation from other users/processes, use separate OS accounts
or an authenticated, access-controlled deployment. A single OS user can read
both of its own credential stores.

## Native Claude and Codex login suites

Use fresh directories rather than copying an existing home containing tokens,
history, MCP credentials, or provider overrides. These shell functions select
the home only for the launched command; they do not change default launches.

```bash
claude_suite() {
  local suite="${1:?Choose personal or corporate}"
  shift
  case "$suite" in personal|corporate) ;; *) return 2 ;; esac
  local suite_dir="$HOME/.config/coding-suites/$suite/claude"
  (umask 077; mkdir -p "$suite_dir") || return
  env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_BASE_URL \
    -u CLAUDE_CODE_OAUTH_TOKEN -u CLAUDE_CODE_USE_BEDROCK \
    -u CLAUDE_CODE_USE_VERTEX -u CLAUDE_CODE_USE_FOUNDRY \
    -u CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR -u CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR \
    -u ANTHROPIC_CUSTOM_HEADERS \
    -u ANTHROPIC_MODEL -u ANTHROPIC_DEFAULT_OPUS_MODEL \
    -u ANTHROPIC_DEFAULT_SONNET_MODEL -u ANTHROPIC_DEFAULT_HAIKU_MODEL \
    CLAUDE_CONFIG_DIR="$suite_dir" CLAUDE_SECURESTORAGE_CONFIG_DIR="$suite_dir" \
    claude "$@"
}

codex_suite() {
  local suite="${1:?Choose personal or corporate}"
  shift
  case "$suite" in personal|corporate) ;; *) return 2 ;; esac
  local suite_dir="$HOME/.config/coding-suites/$suite/codex"
  (umask 077; mkdir -p "$suite_dir") || return
  env -u OPENAI_API_KEY -u OPENAI_BASE_URL CODEX_HOME="$suite_dir" codex "$@"
}
```

For the installed Claude Code 2.1.276, `CLAUDE_CONFIG_DIR` selects config/history;
credential storage follows it unless `CLAUDE_SECURESTORAGE_CONFIG_DIR` overrides
it. Setting both explicitly prevents an inherited secure-storage override from
rejoining the accounts. On macOS, this build also namespaces its Keychain
service by a hash of the credential directory. This is version-specific;
verify a fresh suite is logged out before signing in.

Codex uses `CODEX_HOME` for config, history, and `auth.json`; supported Keychain
storage derives its account key from the canonical home path. Use distinct
real directories, not symlinks to the same home. A named model/provider
**profile alone does not create a separate login store**. The local 0.0.0
development build uses file storage; protect that directory and do not assume
credentials are in Keychain.

From a neutral directory, verify the new homes, then sign in interactively:

```bash
claude_suite personal auth status
claude_suite corporate auth status
codex_suite personal login status
codex_suite corporate login status

claude_suite personal auth login
claude_suite corporate auth login
codex_suite personal login
codex_suite corporate login
```

A logged-out status may exit nonzero; that is expected before login. Choose
the correct account and organization/workspace in each login flow; use separate
browser profiles if the browser keeps selecting the previous account. The
directory name does not enforce workspace membership. Confirm the selected
account/workspace in each client after login, then launch with
`claude_suite corporate` or `codex_suite personal`, for example. Keep corporate
managed policies in effect; these homes do not override organization policy.

These functions clear common provider overrides. A host-managed launcher or
project settings can supply additional configuration, so recheck the effective
provider and account in the actual project before sending work.

If using CCR from one of these client homes, create a CCR provider profile in
that home and select it explicitly. The API key still belongs in that suite's
**router** environment. That session uses the API plan, not the native
subscription login stored alongside it.

## Verification and limits

On September 17, 2026, probes against the CCR release built from `ee0fec7`
used synthetic credentials and a local HTTP upstream:

| Probe | Observed result |
| --- | --- |
| Two independent configs/processes, one key each | Each upstream received only its expected key |
| Personal request naming corporate's absent route | HTTP 503; zero upstream calls |
| Personal upstream returns 429 | Personal returned 429; corporate still returned 200 |
| Same accounts in one shared tier list, personal pinned | Personal 429 fell back to corporate 200 |
| Codex 0.0.0, two homes with synthetic API-key logins | Separate `auth.json` values; logout from personal left corporate intact |
| Claude 2.1.276, two fresh homes | Neither inherited the existing workstation login |
| Claude synthetic OAuth credential files in those homes | Each was recognized; removing personal credentials left corporate recognized |

The Claude synthetic test checked local credential selection, not token validity
or a real Keychain login. No second personal/corporate subscription was signed
in or charged, and no corporate entitlement was verified. The existing live
Claude/Codex tool-loop test is recorded in [Local operation](local-operation.md).

For real acceptance, repeat the tests with each authorized account: confirm
identity/workspace, send a harmless request, verify the expected CCR tier or
native provider, and check usage in the correct provider account. Test each
suite independently with the other stopped. Keep receipt data free of keys,
OAuth tokens, private prompts, and account identifiers.
