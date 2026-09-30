#!/usr/bin/env python3
"""Derive registered CCR configurations from one shared fallback policy."""

import argparse
import copy
import hashlib
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path.home() / ".claude-code-router"
ENV_REF = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


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
        if Path(name).name != name or not name.endswith(".json") or name in reserved:
            raise ValueError(
                "Consumer names must be JSON basenames that do not collide with policy files"
            )
        if not isinstance(settings, dict) or not isinstance(
            settings.get("primary"), str
        ):
            raise TypeError(f"Missing primary route for {name}")
        extras = settings.get("additional_routes", [])
        if not isinstance(extras, list) or any(not isinstance(r, str) for r in extras):
            raise ValueError(f"Invalid additional routes for {name}")
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
    providers = {p["name"]: p for p in result["Providers"]}
    for provider in policy.get("providers", []):
        if not isinstance(provider, dict) or not isinstance(provider.get("name"), str):
            raise TypeError("Invalid shared provider definition")
        key = provider.get("api_key", "")
        if not isinstance(key, str) or ENV_REF.fullmatch(key) is None:
            raise ValueError(
                "Shared providers require environment credential references"
            )
        providers[provider["name"]] = copy.deepcopy(provider)
    for route in tiers:
        parts = route.split(",", 1)
        models = providers.get(parts[0], {}).get("models") if len(parts) == 2 else None
        if len(parts) != 2 or not isinstance(models, list) or parts[1] not in models:
            raise ValueError(f"Consumer {consumer} has an unavailable route: {route}")
    result["Providers"] = list(providers.values())
    router = result["Router"]
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
    return result


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    """Replace one complete configuration; never leave a partially written file."""
    # mkstemp stages at mode 0600 and os.replace keeps the staging mode, so
    # restore the destination's own mode or every sync narrows 0640 to 0600.
    mode = path.stat().st_mode & 0o777 if path.exists() else None
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        if mode is not None:
            os.fchmod(fd, mode)
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            json.dump(value, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def prepare(
    root: Path = ROOT,
) -> tuple[dict[str, Any], dict[str, tuple[dict[str, Any], dict[str, Any]]]]:
    policy = load_policy(root)
    # A crash between fsync and replace strands a dot-prefixed staging file; the
    # finally clause cannot run after SIGKILL, so reap leftovers before writing.
    for stale in root.glob(".*.json.*"):
        stale.unlink()
    snapshots: dict[str, tuple[dict[str, Any], dict[str, Any]]] = {}
    for name in policy["consumers"]:
        on_disk = read_json(root / name)
        snapshots[name] = (on_disk, derive(on_disk, policy, name))
    return policy, snapshots


def sync(root: Path = ROOT, check: bool = False) -> dict[str, Any]:
    policy, snapshots = prepare(root)
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
        "runtime_activation": "not_checked",
    }


def credential_environment(config: dict[str, Any], path: Path) -> dict[str, str]:
    if path.stat().st_mode & 0o077:
        raise ValueError("Runtime credentials must be private (mode 0600)")
    values = read_json(path)
    env = os.environ.copy()
    for name in set(ENV_REF.findall(json.dumps(config))):
        value = values.get(name, env.get(name))
        if not isinstance(value, str) or not value:
            raise ValueError(f"Missing runtime credential or variable: {name}")
        env[name] = value
    return env


def installed_binary() -> Path:
    return Path.home() / ".cargo/bin/ccr-rust"


def serve(config_path: Path, arguments: list[str], root: Path = ROOT) -> None:
    # Preflight before any side effect: a missing binary must not leave the
    # consumer config rewritten or credentials read for a failed launch.
    binary = installed_binary()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise FileNotFoundError(
            f"ccr-rust binary not found or not executable: {binary}"
        )
    policy = load_policy(root)
    config = read_json(config_path)
    if (
        config_path.parent.resolve() == root.resolve()
        and config_path.name in policy["consumers"]
    ):
        config = derive(config, policy, config_path.name)
        if config != read_json(config_path):
            atomic_json(config_path, config)
        # Credential injection stays scoped to registered consumers so an
        # arbitrary --config cannot pull runtime credentials into its environment.
        env = credential_environment(config, root / "runtime-credentials.json")
    else:
        env = os.environ.copy()
    os.execve(str(binary), [str(binary), "--config", str(config_path), *arguments], env)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("sync", "check"))
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument(
        "--json", action="store_true", help="Print machine-readable results"
    )
    args = parser.parse_args(argv)
    try:
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
    return int(args.command == "check" and bool(result["changed"]))


if __name__ == "__main__":
    sys.exit(main())
