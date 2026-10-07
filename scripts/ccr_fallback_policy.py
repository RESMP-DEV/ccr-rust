#!/usr/bin/env python3
"""Derive registered CCR configurations from one shared fallback policy."""

import argparse
import copy
import hashlib
import json
import os
import re
import shutil
import ssl
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path.home() / ".claude-code-router"
ENV_REF = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")
# The live policy JSON is operator-editable state, not an approval authority.
# Main-machine routes and listener ownership are pinned here so an agent can
# repair credentials or consumer drift, but cannot expand routing by editing
# JSON. Route changes require a source change, tests, and live qualification.
MACHINE_ROOT = ROOT
MACHINE_ROUTE_CHAIN: tuple[str, ...] = (
    "zai,glm-5.3-flashx",
    "zai,glm-5.3-flash",
    "zai,glm-5.3",
    "deepseek,deepseek-flash",
    # User-approved 2026-10-07 final tier. Do not shorten this id: the alias
    # "MiniMax-M3.1-Flash" is accepted upstream but serves MiniMax-M3.
    "minimax,MiniMax-M3.1-Flash-Preview",
)
MACHINE_CONSUMERS: dict[str, str] = {
    "config.json": "zai,glm-5.3",
    "config-auth.json": "zai,glm-5.3",
    "glm-workers.json": "zai,glm-5.3-flashx",
    # OCR keeps its distinct local primary; shared fallback remains governed.
    "ocr-reviewers.json": "openrouter,nvidia/nemotron-3-ultra-550b-a55b:free",
}
MACHINE_SHARED_PROVIDERS: dict[str, dict[str, Any]] = {
    "deepseek": {
        "name": "deepseek",
        "api_base_url": "https://api.deepseek.com/anthropic/v1",
        "api_key": "${CCR_DEEPSEEK_API_KEY}",
        "protocol": "anthropic",
        "models": ["deepseek-flash"],
    },
    "minimax": {
        "name": "minimax",
        "api_base_url": "https://api.minimax.io/anthropic/v1",
        "api_key": "${CCR_MINIMAX_API_KEY}",
        "protocol": "anthropic",
        "auth_header": "authorization",
        "models": ["MiniMax-M3.1-Flash-Preview"],
    },
}
# os.umask is process-global and not atomic, so a set/restore dance inside each
# write leaves a window where an importing thread creates files mode 0666.
# Reading it once at import confines that window to interpreter startup.
UMASK = os.umask(0)
os.umask(UMASK)


def read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise TypeError(f"Expected an object: {path.name}")
    return value


def load_policy(root: Path = ROOT) -> dict[str, Any]:
    policy = read_json(root / "fallback-policy.json")
    if policy.get("version") != 1:
        raise ValueError("Unsupported fallback policy version")
    routes = policy.get("routes")
    if (
        not isinstance(routes, list)
        or not routes
        or any(not isinstance(r, str) or r.count(",") != 1 for r in routes)
        or len(set(routes)) != len(routes)
    ):
        raise ValueError("Policy routes must be a nonempty unique provider,model list")
    consumers = policy.get("consumers")
    if not isinstance(consumers, dict) or not consumers:
        raise ValueError("Policy requires registered consumers")
    reserved = {"fallback-policy.json", "runtime-credentials.json"}
    for name, settings in consumers.items():
        # Metacharacters are rejected because consumer names are interpolated
        # into literal staging-file scans; a pattern like "wor[k].json" would
        # match unrelated dot-files instead of that consumer's staging files.
        if (
            Path(name).name != name
            or not name.endswith(".json")
            or name in reserved
            or set(name) & set("*?[]")
        ):
            raise ValueError(
                "Consumer names must be plain JSON basenames without glob"
                " metacharacters or policy-file collisions"
            )
        if not isinstance(settings, dict) or not isinstance(
            settings.get("primary"), str
        ):
            raise TypeError(f"Missing primary route for {name}")
        extras = settings.get("additional_routes", [])
        if not isinstance(extras, list) or any(not isinstance(r, str) for r in extras):
            raise ValueError(f"Invalid additional routes for {name}")
    if root.resolve() == MACHINE_ROOT.resolve():
        if tuple(routes) != MACHINE_ROUTE_CHAIN:
            raise ValueError(
                "Machine fallback routes do not match the approved source policy:"
                f" expected {list(MACHINE_ROUTE_CHAIN)}"
            )
        if set(consumers) != set(MACHINE_CONSUMERS):
            missing = sorted(set(MACHINE_CONSUMERS) - set(consumers))
            extra = sorted(set(consumers) - set(MACHINE_CONSUMERS))
            raise ValueError(
                "Machine listener registrations do not match the approved source"
                f" policy: missing={missing}, extra={extra}"
            )
        for name, expected_primary in MACHINE_CONSUMERS.items():
            if consumers[name]["primary"] != expected_primary:
                raise ValueError(
                    f"Consumer {name} primary must remain {expected_primary}"
                )
            if consumers[name].get("additional_routes", []) != []:
                raise ValueError(
                    f"Consumer {name} cannot add routes outside the approved chain"
                )
        shared = {
            provider["name"]: provider
            for provider in policy.get("providers", [])
            if isinstance(provider, dict) and isinstance(provider.get("name"), str)
        }
        if shared != MACHINE_SHARED_PROVIDERS:
            raise ValueError(
                "Machine shared provider definitions do not match the approved"
                f" source policy: expected {sorted(MACHINE_SHARED_PROVIDERS)}"
            )
    return policy


def derive(
    config: dict[str, Any], policy: dict[str, Any], consumer: str
) -> dict[str, Any]:
    """Keep the primary first, shared final fallback last, and existing client settings."""
    settings = policy["consumers"][consumer]
    chain = policy["routes"]
    primary = settings["primary"]
    if primary == chain[-1] and len(chain) > 1:
        raise ValueError("The final fallback cannot also be a consumer primary")
    candidates = [
        primary,
        *chain[:-1],
        *settings.get("additional_routes", []),
        chain[-1],
    ]
    tiers = list(dict.fromkeys(candidates))
    if tiers[-1] != chain[-1]:
        raise ValueError("Additional routes cannot move the final fallback")
    result = copy.deepcopy(config)
    # A silent last-wins overwrite would hide a misconfigured duplicate entry,
    # so collisions in either list are rejected like duplicate routes are.
    providers: dict[str, dict[str, Any]] = {}
    for provider in result["Providers"]:
        if provider["name"] in providers:
            raise ValueError(
                f"Consumer {consumer} defines provider {provider['name']} more than once"
            )
        providers[provider["name"]] = provider
    shared: set[str] = set()
    for provider in policy.get("providers", []):
        if not isinstance(provider, dict) or not isinstance(provider.get("name"), str):
            raise TypeError("Invalid shared provider definition")
        key = provider.get("api_key", "")
        if not isinstance(key, str) or ENV_REF.fullmatch(key) is None:
            raise ValueError(
                "Shared providers require environment credential references"
            )
        if provider["name"] in shared:
            raise ValueError(
                f"Policy defines shared provider {provider['name']} more than once"
            )
        shared.add(provider["name"])
        providers[provider["name"]] = copy.deepcopy(provider)
    for route in tiers:
        parts = route.split(",", 1)
        models = providers.get(parts[0], {}).get("models") if len(parts) == 2 else None
        if len(parts) != 2 or not isinstance(models, list) or parts[1] not in models:
            raise ValueError(f"Consumer {consumer} has an unavailable route: {route}")
    required_providers = {route.split(",", 1)[0] for route in tiers}
    result["Providers"] = [
        provider
        for provider in providers.values()
        if provider["name"] in required_providers
    ]
    router = result["Router"]
    aliases = router.get("modelAliases")
    if isinstance(aliases, dict):

        def alias_route(value: Any) -> str | None:
            if isinstance(value, str):
                return value
            if isinstance(value, dict) and isinstance(value.get("route"), str):
                return value["route"]
            return None

        result["Router"]["modelAliases"] = {
            name: value
            for name, value in aliases.items()
            if (route := alias_route(value)) is None or route in set(tiers)
        }
    if router.get("topK") is not None:
        raise ValueError(
            f"Consumer {consumer} must remove topK before using strict fallback"
        )
    router.update(
        default=primary, tiers=tiers, strictTierOrder=True, ignoreDirect=False
    )
    sweeps = router.get("retrySweeps")
    if not isinstance(sweeps, dict) or sweeps.get("enabled") is not True:
        raise ValueError(f"Consumer {consumer} must enable native retry sweeps")
    for name in ("sweepCooldownMs", "maxSweeps", "maxHoldMs"):
        if name in sweeps:
            value = sweeps[name]
            minimum = 1 if name == "sweepCooldownMs" else 0
            if type(value) is not int or value < minimum:
                raise ValueError(f"Consumer {consumer} has invalid retrySweeps.{name}")
    if sweeps.get("enabled") is True and sweeps.get("maxHoldMs") == 0:
        # Goverened listeners must not preserve a legacy infinite hold. The
        # source policy can be changed only deliberately, with tests and review.
        sweeps["maxHoldMs"] = 60_000
    return result


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    """Replace one complete configuration; never leave a partially written file."""
    # realpath keeps a dotfile-managed symlink a symlink: replacing the link
    # itself would silently detach the operator's source of truth.
    path = Path(os.path.realpath(path))
    # mkstemp stages at mode 0600 and os.replace keeps the staging mode, so
    # restore the destination's own mode; a first write uses the umask default
    # a plain file creation would get, because a fixed 0600 config is unreadable
    # for the service user in deploy/ccr-rust.service.
    mode = path.stat().st_mode & 0o777 if path.exists() else 0o666 & ~UMASK
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            # fchmod through the wrapper: if it raises, the context manager
            # closes the descriptor instead of leaking the raw fd.
            os.fchmod(output.fileno(), mode)
            json.dump(value, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        # The rename itself is not durable until the directory entry is synced,
        # but some filesystems reject directory fsync outright; the write has
        # already committed, so that failure must not abort remaining consumers.
        try:
            directory = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        except OSError:
            pass
    finally:
        Path(temporary).unlink(missing_ok=True)


def staging_files(consumer: Path) -> list[Path]:
    """Staging files for one consumer, beside its symlink-resolved target."""
    # atomic_json stages beside the realpath target, so a symlinked consumer's
    # stranded files live in the target's directory, never in the policy root.
    resolved = Path(os.path.realpath(consumer))
    return [c for c in resolved.parent.glob(f".{resolved.name}.*") if c.is_file()]


def prepare(
    root: Path = ROOT,
) -> tuple[
    dict[str, Any],
    dict[str, tuple[dict[str, Any], dict[str, Any]]],
    list[str],
]:
    policy = load_policy(root)
    # A crash between fsync and replace strands a dot-prefixed staging file.
    # A matching name proves nothing about ownership: an operator backup or a
    # concurrent writer's in-flight staging file can match the same pattern, and
    # check is documented read-only. So stale files are reported, never deleted.
    stale = sorted(
        {
            str(candidate)
            for name in policy["consumers"]
            for candidate in staging_files(root / name)
        }
    )
    snapshots: dict[str, tuple[dict[str, Any], dict[str, Any]]] = {}
    for name in policy["consumers"]:
        on_disk = read_json(root / name)
        snapshots[name] = (on_disk, derive(on_disk, policy, name))
    return policy, snapshots, stale


def sync(root: Path = ROOT, check: bool = False) -> dict[str, Any]:
    policy, snapshots, stale = prepare(root)
    changed = [
        name for name, (on_disk, derived) in snapshots.items() if on_disk != derived
    ]
    if not check:
        for name in changed:
            atomic_json(root / name, snapshots[name][1])
    return {
        "policy_sha256": hashlib.sha256(
            json.dumps(policy, sort_keys=True).encode()
        ).hexdigest(),
        "changed": changed,
        "consumers": {
            name: derived["Router"]["tiers"] for name, (_, derived) in snapshots.items()
        },
        # Only a run that actually rewrote files requires restarts; under check
        # the drift is reported in changed and nothing was applied.
        "restart_required": [] if check else changed,
        "stale_staging": stale,
        "runtime_activation": "not_checked",
    }


def credential_environment(config: dict[str, Any], path: Path | None) -> dict[str, str]:
    env = os.environ.copy()
    values: dict[str, Any] = {}
    if path is not None:
        # Open once and fstat that same handle: a separate stat→read pair lets
        # the mode widen (or the file be swapped) between check and load.
        with open(path, encoding="utf-8") as handle:
            if os.fstat(handle.fileno()).st_mode & 0o077:
                raise ValueError("Runtime credentials must be private (mode 0600)")
            values = json.load(handle)
        if not isinstance(values, dict):
            raise TypeError(f"Expected an object: {path.name}")
    for name in set(ENV_REF.findall(json.dumps(config))):
        value = values.get(name, env.get(name))
        if not isinstance(value, str) or not value:
            raise ValueError(f"Missing runtime credential or variable: {name}")
        env[name] = value
    return env


def _probe_headers(provider: dict[str, Any], secret: str) -> list[tuple[str, str]]:
    """Credential headers in the same precedence CCR dispatch uses.

    Responses providers send `Authorization: Bearer`. Anthropic providers send
    the configured `auth_header`, defaulting to `x-api-key`. Provider
    `extra_headers` are attached to every attempt so a provider that
    authenticates through a custom header is probed the way CCR calls it.
    """
    protocol = provider.get("protocol", "responses")
    if protocol == "anthropic":
        configured = provider.get("auth_header") or "x-api-key"
        name = str(configured).strip().lower()
        if name in ("authorization", "bearer", "authorization-bearer"):
            pairs = [("Authorization", f"Bearer {secret}")]
        else:
            pairs = [(name, secret)]
    else:
        pairs = [("Authorization", f"Bearer {secret}")]
    for extra_name, extra_value in (provider.get("extra_headers") or {}).items():
        if isinstance(extra_value, str):
            pairs.append((extra_name, extra_value))
    return pairs


def _probe_requests(
    provider: dict[str, Any], secret: str
) -> list[tuple[str, urllib.request.Request]]:
    """Credential-carrying probes, cheapest first.

    The read-only `/models` listing is preferred. Anthropic-compatible
    upstreams that do not implement it answer 404 or 405, so a single-token
    `/messages` call is the fallback: it is the same request CCR dispatch makes,
    capped at one output token, and only runs after the listing endpoint has
    proved unusable.
    """
    protocol = provider.get("protocol", "responses")
    base = str(provider.get("api_base_url", "")).rstrip("/")
    headers = list(_probe_headers(provider, secret))
    requests: list[tuple[str, urllib.request.Request]] = []
    for header, value in headers:
        requests.append(
            (
                "models",
                urllib.request.Request(
                    f"{base}/models",
                    headers={
                        header: value,
                        "accept": "application/json",
                        "user-agent": "ccr-fallback-policy-preflight/1",
                    },
                    method="GET",
                ),
            )
        )
    if protocol == "anthropic":
        models = provider.get("models")
        model = models[0] if isinstance(models, list) and models else ""
        body = json.dumps(
            {
                "model": model,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            }
        ).encode()
        version = provider.get("anthropic_version") or "2023-06-01"
        for header, value in headers:
            requests.append(
                (
                    "messages",
                    urllib.request.Request(
                        f"{base}/messages",
                        data=body,
                        headers={
                            header: value,
                            "content-type": "application/json",
                            "anthropic-version": version,
                            "user-agent": "ccr-fallback-policy-preflight/1",
                        },
                        method="POST",
                    ),
                )
            )
    return requests


def probe_provider(
    provider: dict[str, Any], env: dict[str, str], timeout: float
) -> dict[str, Any]:
    """Authenticate one shared provider against its live endpoint.

    Only the credential path is exercised. No routed listener is touched, and
    neither the credential nor any response body is returned or logged.
    """
    name = provider.get("name")
    base = provider.get("api_base_url")
    if not isinstance(base, str) or not base.startswith(("http://", "https://")):
        return {
            "provider": name,
            "ok": False,
            "status": None,
            "detail": "provider has no usable api_base_url",
        }
    key_ref = provider.get("api_key", "")
    match = ENV_REF.fullmatch(key_ref) if isinstance(key_ref, str) else None
    if match is None:
        return {
            "provider": name,
            "ok": False,
            "status": None,
            "detail": "api_key is not an environment reference",
        }
    secret = env.get(match.group(1))
    if not secret:
        return {
            "provider": name,
            "ok": False,
            "status": None,
            "detail": "credential reference unresolved",
        }
    attempts: list[dict[str, Any]] = []
    for kind, request in _probe_requests(provider, secret):
        header = next(
            iter(
                k
                for k in request.headers
                if k.lower()
                not in ("accept", "content-type", "anthropic-version", "user-agent")
            )
        )
        try:
            with urllib.request.urlopen(
                request, timeout=timeout, context=ssl.create_default_context()
            ) as response:
                status = response.status
                response.read(2048)
            if status < 400:
                return {
                    "provider": name,
                    "ok": True,
                    "status": status,
                    "detail": f"authenticated on {kind} with {header}",
                }
            attempts.append({"probe": kind, "header": header, "status": status})
        except urllib.error.HTTPError as exc:
            attempts.append({"probe": kind, "header": header, "status": exc.code})
            # The body of an auth failure is not needed: the status alone says
            # which credential to rotate, and it may echo secrets.
            exc.close()
        except (urllib.error.URLError, OSError, ValueError) as exc:
            return {
                "provider": name,
                "ok": False,
                "status": None,
                "detail": f"transport failure: {exc}",
            }
    rejected = [a for a in attempts if a["status"] in (401, 403)]
    return {
        "provider": name,
        "ok": False,
        "status": rejected[0]["status"] if rejected else None,
        "detail": "credential rejected" if rejected else "no probe endpoint answered",
        "attempts": attempts,
    }


def preflight(root: Path = ROOT, timeout: float = 15.0) -> dict[str, Any]:
    """Verify every shared provider credential before a route change is trusted."""
    policy = load_policy(root)
    credentials = root / "runtime-credentials.json"
    shared = policy.get("providers", [])
    if not shared:
        return {"providers": [], "ok": True, "detail": "no shared providers"}
    env = credential_environment(
        {"probe": json.dumps(shared)}, credentials if credentials.is_file() else None
    )
    results = [probe_provider(p, env, timeout) for p in shared]
    return {
        "providers": results,
        "ok": all(r["ok"] for r in results),
        "policy_sha256": hashlib.sha256(
            json.dumps(policy, sort_keys=True).encode()
        ).hexdigest(),
    }


def installed_binary() -> Path | None:
    """Resolve the binary from the runtime-test override, cargo, or PATH."""
    candidates = [
        Path(value) for value in (os.environ.get("CCR_TEST_BINARY"),) if value
    ]
    candidates.append(
        Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
        / "bin"
        / "ccr-rust"
    )
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    found = shutil.which("ccr-rust")
    return Path(found) if found else None


def serve(config_path: Path, arguments: list[str], root: Path = ROOT) -> None:
    # Preflight before any side effect: a missing binary or an unresolvable
    # credential must not leave the consumer config rewritten for a failed
    # launch.
    binary = installed_binary()
    if binary is None:
        raise FileNotFoundError(
            "ccr-rust not found; set CCR_TEST_BINARY or CARGO_HOME, install with"
            " cargo install --path . --force, or put ccr-rust on PATH"
        )
    policy = load_policy(root)
    on_disk = read_json(config_path)
    if (
        config_path.parent.resolve() == root.resolve()
        and config_path.name in policy["consumers"]
    ):
        config = derive(on_disk, policy, config_path.name)
        # A missing credentials file is acceptable when every referenced variable
        # is already inherited; values are resolved before the rewrite so a
        # failed launch cannot leave the consumer config updated but unserved.
        # Credential injection stays scoped to registered consumers so an
        # arbitrary --config cannot pull runtime credentials into its environment.
        credentials = root / "runtime-credentials.json"
        env = credential_environment(
            config, credentials if credentials.is_file() else None
        )
        if config != on_disk:
            atomic_json(config_path, config)
    else:
        env = os.environ.copy()
    os.execve(str(binary), [str(binary), "--config", str(config_path), *arguments], env)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("sync", "check", "preflight"))
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument(
        "--json", action="store_true", help="Print machine-readable results"
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=15.0,
        help="Per-provider credential probe timeout in seconds (default: 15)",
    )
    args = parser.parse_args(argv)
    try:
        if args.command == "preflight":
            result = preflight(args.root, args.timeout)
            if args.json:
                print(json.dumps(result))
            else:
                for entry in result["providers"]:
                    mark = "ok" if entry["ok"] else "FAILED"
                    print(f"{entry['provider']}: {mark} {entry.get('detail', '')}")
            return 0 if result["ok"] else 1
        result = sync(args.root, check=args.command == "check")
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(f"Fallback policy error: {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(result))
    else:
        for name, routes in result["consumers"].items():
            print(f"{name}: {' -> '.join(routes)}")
        print(
            f"{'Drifted' if args.command == 'check' else 'Updated'}: {len(result['changed'])}"
        )
        for stale in result["stale_staging"]:
            print(f"Stale staging file (delete manually): {stale}")
    return int(args.command == "check" and bool(result["changed"]))


if __name__ == "__main__":
    sys.exit(main())
