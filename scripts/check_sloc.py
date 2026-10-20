#!/usr/bin/env python3
"""Enforce the Rust source-line ratchet without rewriting legacy hotspots."""

from __future__ import annotations

import argparse
import json
from collections.abc import Sequence
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "src"
# Nonblank source lines at main commit 49838f4 on 2026-10-19. A reduction
# should lower this number; an increase requires splitting a hotspot first.
TOTAL_CEILING = 39_294
FILE_CEILING = 2_500
# Files already above 1,200 lines get individual ceilings at their measured
# baseline. They may shrink or be split, but cannot grow while the ratchet is
# being established.
HOTSPOT_CEILINGS: dict[str, int] = {
    "router/responses_api.rs": 2_419,
    "router/mod.rs": 1_807,
    "router/dispatch.rs": 1_805,
    "config/mod.rs": 1_353,
    "router/streaming.rs": 1_360,
}


def nonblank_lines(path: Path) -> int:
    return sum(
        bool(line.strip()) for line in path.read_text(encoding="utf-8").splitlines()
    )


def check(root: Path = SOURCE_ROOT) -> dict[str, Any]:
    errors: list[str] = []
    files: dict[str, int] = {}
    for path in sorted(root.rglob("*.rs")):
        relative = path.relative_to(root).as_posix()
        count = nonblank_lines(path)
        files[relative] = count
        ceiling = min(FILE_CEILING, HOTSPOT_CEILINGS.get(relative, FILE_CEILING))
        if count > ceiling:
            errors.append(
                f"{relative} has {count} nonblank lines; ceiling is {ceiling}"
            )
    total = sum(files.values())
    if total > TOTAL_CEILING:
        errors.append(
            f"total src/**/*.rs nonblank lines is {total}; ceiling is {TOTAL_CEILING}"
        )
    return {
        "ok": not errors,
        "total": total,
        "total_ceiling": TOTAL_CEILING,
        "file_ceiling": FILE_CEILING,
        "hotspot_ceilings": HOTSPOT_CEILINGS,
        "files": files,
        "errors": errors,
    }


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--json", action="store_true", help="emit machine-readable JSON"
    )
    arguments = parser.parse_args(argv)
    result = check()
    if arguments.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(
            f"Rust nonblank source lines: {result['total']}/{result['total_ceiling']}"
        )
        for error in result["errors"]:
            print(f"error: {error}")
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
