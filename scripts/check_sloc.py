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
# Nonblank source lines at main commit 49838f4 on 2026-10-19, raised on
# 2026-10-22 after splitting test concerns out of the Responses and Dispatch
# hotspot files. Raised again after preserving per-block signed reasoning and
# splitting the incremental-stream tests into their own module. Re-pinned for
# token-aware admission control, then raised after moving provider request
# overrides out of the Dispatch hotspot. Raised 2026-10-23 for the
# redacted_thinking content-block support Muse needs on its Messages surface.
# Ceilings sit at the measured counts with zero headroom.
# A reduction should lower this number; another increase requires splitting a
# hotspot first. Raised by 6 lines 2026-10-23 for the round-trip comment on
# the DeepSeek thinking placeholder (PR #79). Re-pinned 2026-10-23 for the DeepSeek leading-thinking
# injection transformer (shared helper in transformer/mod.rs plus the
# factory-registry path in transform/deepseek.rs) merged alongside the
# Muse/Holo4 routing work. Raised to 41,024 on 2026-10-23 for the
# oversized-request solo admission rule and its tests in admission.rs.
# Raised to 41,372 on 2026-11-23. Two causes, both measured on this tree:
# the outstanding worker-inspector and admission work already in the dirty
# checkout accounted for 196 lines above the previous pin, and the
# provider-level model map added 152. The two hotspot ceilings the map
# touched were not raised: config/mod.rs fell to 1,368 and
# router/dispatch.rs to 1,787 after the model-map validation moved into
# src/config/model_map.rs and both test concerns moved into sibling test
# modules, so the map cost no hotspot headroom.
# Raised to 41,572 on 2026-11-24 for served-model and reasoning-token
# accounting. The 57-line streaming test moved to
# src/router/streaming_reasoning_tests.rs and the reasoning parsers moved to
# src/router/usage_metrics.rs, which kept dispatch.rs and router/mod.rs under
# their ceilings. streaming.rs (+13) and sse.rs (+2) carry the production
# wiring that cannot leave those files, so only those two ceilings move, by
# exactly that measured amount.
TOTAL_CEILING = 41_572
FILE_CEILING = 2_500
# Files already above 1,200 lines get individual ceilings at their measured
# baseline. They may shrink or be split, but cannot grow while the ratchet is
# being established.
HOTSPOT_CEILINGS: dict[str, int] = {
    "router/responses_api.rs": 2_419,
    "router/mod.rs": 1_831,
    "router/dispatch.rs": 1_805,
    "config/mod.rs": 1_414,
    "router/streaming.rs": 1_386,
    "sse.rs": 296,
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
