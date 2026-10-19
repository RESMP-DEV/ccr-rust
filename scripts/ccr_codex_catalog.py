#!/usr/bin/env python3
"""Keep Codex metadata aligned with the governed CCR route aliases."""

from __future__ import annotations

import argparse
import copy
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any

DEFAULT_CATALOG = Path.home() / ".codex" / "ccr-live-model-catalog.json"
SOURCE_SLUG = "glm-5.3"

ENTRIES: tuple[dict[str, str], ...] = (
    {
        "slug": "ccr-primary",
        "display_name": "CCR Primary (GLM 5.3)",
        "description": "Stable CCR primary alias; metadata mirrors the current GLM 5.3 primary tier.",
    },
    {
        "slug": "glm-5.3-flash",
        "display_name": "GLM 5.3 Flash (Z.ai via CCR)",
        "description": "GLM 5.3 Flash through the bare CCR alias; metadata mirrors GLM 5.3.",
    },
    {
        "slug": "zai,glm-5.3-flash",
        "display_name": "GLM 5.3 Flash (Z.ai via CCR)",
        "description": "GLM 5.3 Flash through the explicit Z.ai CCR route; metadata mirrors GLM 5.3.",
    },
    {
        "slug": "qwen3.8-flash",
        "display_name": "Qwen 3.8 Flash (via CCR)",
        "description": "Qwen 3.8 Flash through the bare CCR alias; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "qwen,qwen3.8-flash",
        "display_name": "Qwen 3.8 Flash (via CCR)",
        "description": "Qwen 3.8 Flash through the exact Qwen CCR route; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "qwen3.8-max",
        "display_name": "Qwen 3.8 Max (via CCR)",
        "description": "Qwen 3.8 Max through the bare CCR alias; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "qwen,qwen3.8-max",
        "display_name": "Qwen 3.8 Max (via CCR)",
        "description": "Qwen 3.8 Max through the exact Qwen CCR route; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "MiniMax-M3.1-Flash",
        "display_name": "MiniMax M3.1 Flash (via CCR)",
        "description": "MiniMax M3.1 Flash through the bare CCR alias; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "minimax,MiniMax-M3.1-Flash",
        "display_name": "MiniMax M3.1 Flash (via CCR)",
        "description": "MiniMax M3.1 Flash through the exact MiniMax CCR route; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "deepseek-v4.1-flash-1017",
        "display_name": "DeepSeek V4.1 Flash 1017 (via CCR)",
        "description": "Qwen's dated DeepSeek checkpoint through the bare CCR alias; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "qwen,deepseek-v4.1-flash-1017",
        "display_name": "DeepSeek V4.1 Flash 1017 (via CCR)",
        "description": "Qwen's dated DeepSeek checkpoint through the exact Qwen CCR route; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "deepseek-v4.1-flash",
        "display_name": "DeepSeek V4.1 Flash (via CCR)",
        "description": "Qwen's undated DeepSeek checkpoint through the bare CCR alias; metadata mirrors GLM 5.3 tool tuning.",
    },
    {
        "slug": "qwen,deepseek-v4.1-flash",
        "display_name": "DeepSeek V4.1 Flash (via CCR)",
        "description": "Qwen's undated DeepSeek checkpoint through the exact Qwen CCR route; metadata mirrors GLM 5.3 tool tuning.",
    },
)

RETIRED_SLUGS = frozenset(
    {
        "glm-5.3-flashx",
        "zai,glm-5.3-flashx",
        "MiniMax-M3.1-Flash-Preview",
        "minimax,MiniMax-M3.1-Flash-Preview",
    }
)


def load_catalog(path: Path) -> dict[str, Any]:
    document = json.loads(path.read_text())
    if not isinstance(document, dict) or not isinstance(document.get("models"), list):
        raise TypeError("catalog must contain a models array")
    for entry in document["models"]:
        if not isinstance(entry, dict) or not isinstance(entry.get("slug"), str):
            raise TypeError("every catalog model entry must be an object with a slug")
    return document


def entry_by_slug(document: dict[str, Any], slug: str) -> dict[str, Any] | None:
    for entry in document["models"]:
        if entry.get("slug") == slug:
            return entry
    return None


def derive_entry(source: dict[str, Any], target: dict[str, str]) -> dict[str, Any]:
    derived = copy.deepcopy(source)
    slug = target["slug"]
    for field in ("id", "model", "slug"):
        derived[field] = slug
    for field in ("displayName", "display_name"):
        derived[field] = target["display_name"]
    derived["description"] = target["description"]
    return derived


def catalog_sync(
    path: Path = DEFAULT_CATALOG,
    *,
    apply: bool = False,
    snapshot_root: Path | None = None,
    take_snapshot: bool = True,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "catalog": str(path),
        "applied": apply,
        "changes": [],
        "errors": [],
    }
    try:
        document = load_catalog(path)
        source = entry_by_slug(document, SOURCE_SLUG)
        if source is None:
            raise ValueError(f"source model metadata is absent: {SOURCE_SLUG}")
        if not isinstance(source.get("context_window"), int):
            raise TypeError(
                f"source model metadata has invalid context_window: {SOURCE_SLUG}"
            )

        models = [
            entry
            for entry in document["models"]
            if entry.get("slug") not in RETIRED_SLUGS
        ]
        removed = len(document["models"]) - len(models)
        if removed:
            result["changes"].append(
                {"action": "remove", "slugs": sorted(RETIRED_SLUGS)}
            )

        counts: dict[str, int] = {}
        for entry in models:
            slug = entry.get("slug")
            counts[slug] = counts.get(slug, 0) + 1
        for slug, count in sorted(counts.items(), key=lambda item: str(item[0])):
            if count > 1:
                result["errors"].append(f"duplicate catalog slug: {slug}")
        if result["errors"]:
            raise ValueError("; ".join(result["errors"]))

        for target in ENTRIES:
            slug = target["slug"]
            matches = [entry for entry in models if entry.get("slug") == slug]
            derived = derive_entry(source, target)
            current = matches[0] if matches else None
            action = "update" if current is not None else "add"
            if current != derived:
                result["changes"].append({"action": action, "slug": slug})
            models = [entry for entry in models if entry.get("slug") != slug]
            models.append(derived)

        document["models"] = models
        if result["errors"]:
            raise ValueError("; ".join(result["errors"]))
    except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
        result["errors"].append(f"{type(error).__name__}: {error}")

    result["ok"] = not result["changes"] and not result["errors"]
    if not apply or result["errors"] or not result["changes"]:
        return result
    if take_snapshot:
        result["snapshot"] = take_pre_write_snapshot(snapshot_root)
    write_catalog(path, document)
    result["ok"] = True
    return result


def take_pre_write_snapshot(root: Path | None) -> dict[str, Any]:
    script_root = Path(__file__).resolve().parent
    if str(script_root) not in sys.path:
        sys.path.insert(0, str(script_root))
    import ccr_recovery

    return ccr_recovery.pre_snapshot(
        root or ccr_recovery.SNAPSHOT_ROOT, "pre-catalog-sync"
    )


def write_catalog(path: Path, document: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    mode = path.stat().st_mode & 0o777 if path.exists() else 0o600
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", dir=path.parent
    )
    try:
        with os.fdopen(descriptor, "w") as handle:
            json.dump(document, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary_name, mode)
        os.replace(temporary_name, path)
    except BaseException:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass
        raise


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument(
        "--check", action="store_true", help="Report drift without writing"
    )
    parser.add_argument("--snapshot-root", type=Path)
    parser.add_argument(
        "--no-snapshot",
        action="store_true",
        help="Skip the pre-write recovery snapshot (tests and emergency use only)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    result = catalog_sync(
        args.catalog,
        apply=not args.check,
        snapshot_root=args.snapshot_root,
        take_snapshot=not args.no_snapshot,
    )
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ok"] and not result["errors"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
