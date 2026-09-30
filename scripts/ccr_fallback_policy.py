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
    value = json.loads(path.read_text())
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
    for name, settings in consumers.items():
        if Path(name).name != name or not name.endswith(".json"):
            raise ValueError("Consumer names must be JSON basenames")
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
        if (
            len(parts) != 2
            or parts[0] not in providers
            or parts[1] not in providers[parts[0]].get("models", [])
        ):
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
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as output:
            json.dump(value, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def prepare(root: Path = ROOT) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    policy = load_policy(root)
    # Validate every consumer before writing any file.
    configs = {
        name: derive(read_json(root / name), policy, name)
        for name in policy["consumers"]
    }
    return policy, configs


def sync(root: Path = ROOT, check: bool = False) -> dict[str, Any]:
    policy, configs = prepare(root)
    changed = [
        name for name, config in configs.items() if read_json(root / name) != config
    ]
    if not check:
        for name in changed:
            atomic_json(root / name, configs[name])
    return {
        "policy_sha256": hashlib.sha256(
            json.dumps(policy, sort_keys=True).encode()
        ).hexdigest(),
        "changed": changed,
        "consumers": {
            name: config["Router"]["tiers"] for name, config in configs.items()
        },
        "restart_required": changed if not check else [],
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


def serve(config_path: Path, arguments: list[str], root: Path = ROOT) -> None:
    policy = load_policy(root)
    config = read_json(config_path)
    if (
        config_path.parent.resolve() == root.resolve()
        and config_path.name in policy["consumers"]
    ):
        config = derive(config, policy, config_path.name)
        if config != read_json(config_path):
            atomic_json(config_path, config)
    env = credential_environment(config, root / "runtime-credentials.json")
    binary = str(Path.home() / ".cargo/bin/ccr-rust")
    os.execve(binary, [binary, "--config", str(config_path), *arguments], env)


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
