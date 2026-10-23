#!/usr/bin/env python3
"""Derive registered CCR configurations from one shared fallback policy."""

from __future__ import annotations

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
    # User-approved 2026-10-11 removal of the former first tier
    # "zai,glm-5.3-flashx": its coding-plan trial expired 2026-10-09
    # (429 code 1311, "plan does not yet include access"), so leading
    # with it made every dispatch pay one guaranteed-refusal round trip.
    # Restored 2026-10-18 by user direction after the cordon's premise was
    # disproved: the monthly counter (4000/4000, resets 2026-10-28) drew no
    # enforcement wall. Z.AI meters usage without distinguishing model
    # sessions from tool use, the router served thousands of successful GLM
    # calls after that probe read 0%, and the ZCode app kept serving GLM on
    # the same plan. The observed completion limits are the rolling 5-hour
    # prompt window and concurrency caps.
    # Worker order (2026-10-21 user authorization): MiniMax M3.1 Flash is the
    # scoped-worker primary because the Z.AI GLM plan keeps exhausting its
    # monthly utilization window, while MiniMax absorbs that volume. GLM-5.3
    # remains the main/auth primary; GLM-5.3 Flash becomes the final shared
    # fallback instead of the first worker tier. The MiniMax ID below is the
    # reviewed one in this source tree and is what the live listeners
    # already serve; legacy `-Preview` aliases are canonicalized on
    # dispatch rather than pinned here.
    # Qwen was removed 2026-10-19 by user direction: the token plan's monthly
    # utilization window was exhausted, so every dispatch paid a guaranteed
    # refusal before reaching a tier that could still serve. The provider and
    # its credential stay registered below so a dated re-approval restores it
    # without rewiring keys. Live utilization at removal is not re-checked here
    # on purpose: an exhausted window cannot be optimized around, only skipped.
    "minimax,MiniMax-M3.1-Flash",
    "zai,glm-5.3-flash",
    # 2026-10-21 user direction: native DeepSeek returns as the last cloud
    # tier, ahead of the local model. A live probe the same day authenticated
    # and completed a real request, so the earlier PAYG-balance cordon is lifted.
    "deepseek,deepseek-flash",
    # 2026-10-21 user direction: the keyed B550 Strata Coder service is the
    # stronger local coding crawl-back. It sits ahead of local27, but remains
    # constrained to one in-flight request because its current engine config is
    # a FIFO server with a 16384-token context.
    "strata-b550,qwen3.8-flash-next-coder",
    # 2026-10-21 user direction: the resident local Qwen3.8-27B MXFP4 MLX
    # server is the very last crawl-back tier for every consumer (standard,
    # interactive/auth, workers), so a cloud-wide outage or exhausted windows
    # still leave a serving model. It costs nothing and cannot 429, but it is
    # the slowest and weakest tier, hence last.
    "local27,qwen3.8-27b-mxfp4",
)
MACHINE_CONSUMERS: dict[str, str] = {
    # Primaries returned to their pre-cordon tiers with the 2026-10-18
    # user-directed GLM restore.
    "config.json": "zai,glm-5.3",
    "config-auth.json": "zai,glm-5.3",
    # 2026-10-21: the worker primary moved to MiniMax so GLM Flash is no
    # longer the first tier a scoped worker burns its window on.
    "glm-workers.json": "minimax,MiniMax-M3.1-Flash",
    # OCR keeps its distinct code-review primary. SenseNova is not inserted:
    # that consumer is review traffic, not image OCR, and the user explicitly
    # excluded SenseNova from review work.
    "ocr-reviewers.json": "openrouter,nvidia/nemotron-3-ultra-550b-a55b:free",
}
# No consumer holds a private route seat. The 2026-10-19 Qwen removal took
# the main/auth `qwen,qwen3.8-max` seat out with it; every consumer now uses the
# shared chain, and a new private seat needs a dated approval here.
MACHINE_CONSUMER_ADDITIONAL_ROUTES: dict[str, tuple[str, ...]] = {}
# Direct-only seats are configured in consumer JSON but never inserted into
# Router.tiers. CCR serves an explicit provider,model pin as a one-tier request
# while normal failures continue through the shared chain.
MACHINE_CONSUMER_DIRECT_ROUTES: dict[str, tuple[str, ...]] = {
    "glm-workers.json": ("sensenova-vision,sensenova-6.8-flash-lite",),
}
MACHINE_SHARED_PROVIDERS: dict[str, dict[str, Any]] = {
    "zai": {
        "name": "zai",
        "api_base_url": "https://api.z.ai/api/v1",
        "api_key": "${CCR_ZAI_API_KEY}",
        "protocol": "responses",
        "auth_header": "authorization",
        # glm-5.3-flashx stays a registered capability only: its coding-plan
        # trial expired 2026-10-09 and no chain entry routes to it, so
        # derive() filters it out of generated consumer configs.
        "models": ["glm-5.3", "glm-5.3-flashx", "glm-5.3-flash"],
        "honor_ratelimit_headers": False,
    },
    # OpenRouter remains the code-review primary. Keep the definition here so
    # restoring the OCR consumer never depends on a prior derived JSON file.
    "openrouter": {
        "name": "openrouter",
        "api_base_url": "https://openrouter.ai/api/v1",
        "api_key": "${CCR_OPENROUTER_API_KEY}",
        "protocol": "openai",
        "extra_headers": {"User-Agent": "claude-cli/2.1.30 (external, cli)"},
        "models": ["nvidia/nemotron-3-ultra-550b-a55b:free"],
    },
    # Registered option, unrouted since 2026-10-11 (user: not working
    # reliably). Kept wired so re-adding the chain entry restores it.
    "gmi": {
        "name": "gmi",
        "api_base_url": "https://api.gmi-serving.com/v1",
        "api_key": "${CCR_GMI_API_KEY}",
        "protocol": "openai",
        # Cloudflare rejects Python-urllib with 403/1010. CCR uses reqwest,
        # but pin a stable agent so provider identity does not vary.
        "extra_headers": {"User-Agent": "ccr-rust/1 (+gmi)"},
        "models": ["Qwen/Qwen3.8-Max-0902"],
    },
    # SenseNova Token Plan (free public beta). The generic entry stays
    # registered without a fallback seat so code-review and worker traffic
    # cannot consume it accidentally.
    "sensenova": {
        "name": "sensenova",
        "api_base_url": "https://token.sensenova.ai/v1",
        "api_key": "${CCR_SENSENOVA_API_KEY}",
        "protocol": "openai",
        "models": ["sensenova-6.8-flash-lite"],
        "tier_name": "ccr-sensenova",
    },
    # Registered capability for an explicit tool-less vision/extraction lane;
    # deliberately unrouted because the existing OCR listener is code review,
    # not image OCR. With adaptive thinking plus effort none, image extraction
    # measured 1.6s/72 output tokens versus 6.7s/446 tokens.
    "sensenova-vision": {
        "name": "sensenova-vision",
        "api_base_url": "https://token.sensenova.ai/v1",
        "api_key": "${CCR_SENSENOVA_API_KEY}",
        "protocol": "openai",
        "models": ["sensenova-6.8-flash-lite"],
        "tier_name": "ccr-sensenova-vision",
        "force_adaptive_thinking": True,
        "force_reasoning_effort": "none",
    },
    # Routed again 2026-10-21 (balance topped up; unrouted 2026-10-11 to
    # 2026-10-21 after preflight 402).
    "deepseek": {
        "name": "deepseek",
        "api_base_url": "https://api.deepseek.com/anthropic/v1",
        "api_key": "${CCR_DEEPSEEK_API_KEY}",
        "protocol": "anthropic",
        "models": ["deepseek-flash"],
    },
    # Qwen Token Plan (Standard tier). The sk-sp- key is plan-specific and
    # must use this Anthropic-compatible endpoint, including the final /v1.
    # Registered and live-tested 2026-10-18; model IDs came from /models and
    # two real completions through CCR.
    "qwen": {
        "name": "qwen",
        "api_base_url": "https://token-plan.maas.qwencloudapi.com/apps/anthropic/v1",
        "api_key": "${CCR_QWEN_API_KEY}",
        "protocol": "anthropic",
        "models": [
            "qwen3.8-flash",
            "qwen3.8-max",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.6-flash",
            "deepseek-v4.1-flash-1017",
            "deepseek-v4.1-flash",
            "deepseek-v4-pro",
            "glm-5.3",
            "glm-5.2",
            "auto",
        ],
        # Qwen enforces Anthropic's required max_tokens even when Codex omits
        # it. Inject a generous default without raising a client's lower cap.
        "transformer": {
            "use": [["maxtoken", {"max_tokens": 32768, "only_if_missing": True}]]
        },
    },
    # Resident local MLX server (dflash_mlx, Qwen3.8-27B MXFP4) on this Mac.
    # Points at the backend (:8081), not the admission proxy (:8080): the proxy
    # rejects stream:true, and CCR clients stream. maxInflight mirrors the
    # proxy's LOCAL27_MAX_BACKEND=4 so the backend keeps the same bound. The
    # server ignores the request model name and needs no auth; the credential
    # reference exists only because shared providers require one.
    "local27": {
        "name": "local27",
        "api_base_url": "http://127.0.0.1:8081/v1",
        "api_key": "${CCR_LOCAL27_API_KEY}",
        "protocol": "openai",
        "models": ["qwen3.8-27b-mxfp4"],
        "maxInflight": 4,
        # The server defaults to 512 completion tokens when a request omits
        # max_tokens (Codex does); the model's context is 262144.
        "transformer": {
            "use": [["maxtoken", {"max_tokens": 16384, "only_if_missing": True}]]
        },
    },
    # Keyed LAN Strata service on B550. The localhost tunnel is ZeroTier-first
    # with authenticated router/LAN and Strix-jump fallbacks, so it never pins
    # a historical IPv4 address. The service enforces the referenced credential.
    # Forcing reasoning off avoids a client that omits an effort spending its
    # output budget in reasoning_content.
    "strata-b550": {
        "name": "strata-b550",
        "api_base_url": "http://127.0.0.1:18081/v1",
        "api_key": "${CCR_STRATA_B550_API_KEY}",
        "protocol": "openai",
        "models": ["qwen3.8-flash-next-coder"],
        "maxInflight": 1,
        "force_reasoning_effort": "none",
        "transformer": {
            "use": [["maxtoken", {"max_tokens": 4096, "only_if_missing": True}]]
        },
    },
    "minimax": {
        "name": "minimax",
        "api_base_url": "https://api.minimax.io/anthropic/v1",
        "api_key": "${CCR_MINIMAX_API_KEY}",
        "protocol": "anthropic",
        "auth_header": "authorization",
        "models": ["MiniMax-M3.1-Flash"],
        # Required for MiniMax M3/M3.1 compatibility: adaptive thinking
        # injection and malformed-output sanitization live in the transformer.
        # Without this the registry builds an empty chain and neither runs.
        "transformer": {"use": ["minimax"]},
        # 2026-10-22 user-directed delegation tuning: MiniMax 429s are
        # token-volume collisions, not request-count limits (measured
        # 2026-10-22: identical dispatch rates in minutes with and without
        # 429s; ten concurrent small requests served cleanly; three ~150K
        # token prefills collided), so the count-only AIMD cannot prevent
        # them. A 250K in-flight prefill budget admits one large worker
        # context plus a medium one and defers the next large preflight to
        # the chain (GLM-5.3-Flash) instead of paying an upstream 429 and
        # the AIMD cut cascade that follows it.
        "maxInflightTokens": 250000,
    },
}
# os.umask is process-global and not atomic, so a set/restore dance inside each
# write leaves a window where an importing thread creates files mode 0666.
# Reading it once at import confines that window to interpreter startup.
UMASK = os.umask(0)
os.umask(UMASK)

# Whole-body integrity pin for the machine policy JSON. The structural gate
# below checks routes, listener registrations, and provider definitions, but an
# editor could still change an unconstrained field. Any change to the approved
# policy body must update this pin in the same reviewed source change.
MACHINE_POLICY_INTEGRITY_SHA256 = (
    "258c93b9a9bc4e3e5b8bb3644303d8e11fb8b178f9e72fd2f84531f2cb37638e"
)


def recovery_pre_snapshot(label: str) -> dict[str, Any]:
    """Take a best-effort recovery snapshot without blocking a config write."""
    try:
        script_root = Path(__file__).resolve().parent
        if str(script_root) not in sys.path:
            sys.path.insert(0, str(script_root))
        from ccr_recovery import pre_snapshot

        return pre_snapshot(label=label)
    except (ImportError, OSError) as error:
        message = {
            "ok": False,
            "label": label,
            "error": f"{type(error).__name__}: {error}",
        }
        print(
            f"ccr-recovery: pre-write snapshot failed to launch: {message['error']}",
            file=sys.stderr,
        )
        return message


def should_recovery_snapshot(root: Path) -> bool:
    """Only the governed machine root receives machine-surface snapshots."""
    return root.resolve() == MACHINE_ROOT.resolve()


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
        direct = settings.get("direct_routes", [])
        if not isinstance(direct, list) or any(
            not isinstance(r, str) or r.count(",") != 1 for r in direct
        ):
            raise ValueError(f"Invalid direct routes for {name}")
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
            expected_extras = list(MACHINE_CONSUMER_ADDITIONAL_ROUTES.get(name, ()))
            if consumers[name].get("additional_routes", []) != expected_extras:
                raise ValueError(
                    f"Consumer {name} additional routes must remain {expected_extras}"
                )
            expected_direct = list(MACHINE_CONSUMER_DIRECT_ROUTES.get(name, ()))
            if consumers[name].get("direct_routes", []) != expected_direct:
                raise ValueError(
                    f"Consumer {name} direct routes must remain {expected_direct}"
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
        policy_body = json.dumps(policy, sort_keys=True)
        observed = hashlib.sha256(policy_body.encode()).hexdigest()
        if observed != MACHINE_POLICY_INTEGRITY_SHA256:
            raise ValueError(
                "Machine policy body does not match the approved integrity pin:"
                f" expected {MACHINE_POLICY_INTEGRITY_SHA256}, got {observed}."
                " Update the reviewed source pin, not the JSON."
            )
    return policy


def derive(
    config: dict[str, Any], policy: dict[str, Any], consumer: str
) -> dict[str, Any]:
    """Keep the primary first, shared final fallback last, and existing client settings."""
    settings = policy["consumers"][consumer]
    chain = policy["routes"]
    primary = settings["primary"]
    direct_routes = settings.get("direct_routes", [])
    if primary == chain[-1] and len(chain) > 1:
        raise ValueError("The final fallback cannot also be a consumer primary")
    candidates = [
        primary,
        *settings.get("additional_routes", []),
        *chain[:-1],
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
    for route in direct_routes:
        if route in tiers:
            raise ValueError(
                f"Consumer {consumer} direct route must not be a fallback tier: {route}"
            )
        parts = route.split(",", 1)
        models = providers.get(parts[0], {}).get("models") if len(parts) == 2 else None
        if len(parts) != 2 or not isinstance(models, list) or parts[1] not in models:
            raise ValueError(f"Consumer {consumer} has an unavailable route: {route}")
    required_providers = {route.split(",", 1)[0] for route in (*tiers, *direct_routes)}
    active_routes = {*tiers, *direct_routes}
    result["Providers"] = [
        provider
        for provider in providers.values()
        if provider["name"] in required_providers
    ]
    for provider in result["Providers"]:
        provider["models"] = [
            model
            for model in provider.get("models", [])
            if f"{provider['name']},{model}" in active_routes
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
    # The /v1/models listing advertises every tier's bare model id, but the
    # router only dispatches qualified provider,model routes and explicit
    # aliases: a bare id with no alias entry silently falls through to
    # Router.default (observed serving zai,glm-5.3 for a requested
    # MiniMax-M3.1-Flash-Preview). Derive one alias per unambiguous tier
    # model so advertised ids stay routable without hand-editing every
    # consumer; an operator entry is never overridden, and a name two
    # providers share stays unaliased rather than silently choosing a winner.
    derived_aliases = result["Router"].get("modelAliases")
    if not isinstance(derived_aliases, dict):
        derived_aliases = {}
        result["Router"]["modelAliases"] = derived_aliases
    claims: dict[str, int] = {}
    for route in tiers:
        model = route.partition(",")[2]
        if model:
            claims[model] = claims.get(model, 0) + 1
    for route in tiers:
        model = route.partition(",")[2]
        if model and claims[model] == 1 and model not in derived_aliases:
            derived_aliases[model] = route
    # Stable client handle: a pin like Codex's `ccr-primary` follows the
    # consumer's primary tier across cordons and restores without restarting
    # long-lived clients, which read their model pin only at process start.
    derived_aliases.setdefault("ccr-primary", primary)
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
        if changed and should_recovery_snapshot(root):
            recovery_pre_snapshot("pre-sync")
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


def _probe_headers(provider: dict[str, Any], secret: str) -> dict[str, str]:
    """Credential headers in the same precedence CCR dispatch uses.

    Responses providers send `Authorization: Bearer`. Anthropic providers send
    the configured `auth_header`, defaulting to `x-api-key`.

    The auth header is returned alone; provider `extra_headers` are merged in
    `_probe_requests` so every attempt carries them alongside the credential,
    the way CCR dispatch does, rather than replacing it.
    """
    protocol = provider.get("protocol", "responses")
    if protocol == "anthropic":
        configured = provider.get("auth_header") or "x-api-key"
        name = str(configured).strip().lower()
        if name in ("authorization", "bearer", "authorization-bearer"):
            return {"Authorization": f"Bearer {secret}"}
        return {name: secret}
    return {"Authorization": f"Bearer {secret}"}


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
    # Same merge order as CCR dispatch: extra_headers are applied after the
    # auth header, so they override it on a key collision and ride along on
    # every attempt instead of becoming standalone unauthenticated probes.
    headers = {
        **_probe_headers(provider, secret),
        **{
            str(extra_name): extra_value
            for extra_name, extra_value in (provider.get("extra_headers") or {}).items()
            if isinstance(extra_value, str)
        },
    }
    requests: list[tuple[str, urllib.request.Request]] = []
    requests.append(
        (
            "models",
            urllib.request.Request(
                f"{base}/models",
                headers={
                    **headers,
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
        requests.append(
            (
                "messages",
                urllib.request.Request(
                    f"{base}/messages",
                    data=body,
                    headers={
                        **headers,
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
    """Verify every routed shared-provider credential before a route change is trusted.

    A registered provider that no active route references is reported as
    registered_not_routed and never probed: keeping one registered while
    unrouted (an exhausted-balance DeepSeek awaiting top-up, say) must not
    gate route changes on a credential nobody dispatches.
    """
    policy = load_policy(root)
    credentials = root / "runtime-credentials.json"
    if not policy.get("providers"):
        return {"providers": [], "ok": True, "detail": "no shared providers"}
    routed: set[str] = {route.split(",", 1)[0] for route in policy["routes"]}
    for settings in policy["consumers"].values():
        routed.add(settings["primary"].split(",", 1)[0])
        routed |= {
            route.split(",", 1)[0] for route in settings.get("additional_routes", [])
        }
        routed |= {
            route.split(",", 1)[0] for route in settings.get("direct_routes", [])
        }
    shared = [
        provider for provider in policy["providers"] if provider.get("name") in routed
    ]
    env = credential_environment(
        {"probe": json.dumps(shared)}, credentials if credentials.is_file() else None
    )
    results = [probe_provider(p, env, timeout) for p in shared]
    return {
        "providers": results,
        "registered_not_routed": sorted(
            provider.get("name")
            for provider in policy["providers"]
            if provider.get("name") not in routed
        ),
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
            if should_recovery_snapshot(root):
                recovery_pre_snapshot("pre-serve")
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
