#!/usr/bin/env python3
"""Run the deterministic CCR-Rust quality matrix and write a receipt."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import time
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPORT_DIR = ROOT / "build" / "reports" / "quality"
RECEIPT_PATH = REPORT_DIR / "receipt.json"


@dataclass(frozen=True)
class Check:
    name: str
    command: tuple[str, ...]
    cwd: Path = ROOT
    env: tuple[tuple[str, str], ...] = ()


def git_output(*arguments: str) -> str:
    result = subprocess.run(
        ("git", *arguments),
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    return result.stdout.rstrip("\n")


def selected_checks(*, fast: bool, coverage: bool, mutation: bool) -> tuple[Check, ...]:
    if fast:
        return (
            Check(
                "property-invariants",
                (
                    "cargo",
                    "test",
                    "--all-features",
                    "--locked",
                    "--test",
                    "property_invariants",
                ),
            ),
        )

    checks = [
        Check("format", ("cargo", "fmt", "--check")),
        Check(
            "clippy",
            (
                "cargo",
                "clippy",
                "--all-features",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ),
        ),
    ]

    if coverage:
        output_dir = ROOT / "build" / "reports" / "coverage"
        output_dir.mkdir(parents=True, exist_ok=True)
        checks.append(
            Check(
                "llvm-coverage",
                (
                    "cargo",
                    "llvm-cov",
                    "--all-features",
                    "--locked",
                    "--html",
                    "--fail-under-lines",
                    "75",
                    "--output-dir",
                    str(output_dir),
                ),
                env=(
                    (
                        "CARGO_TARGET_DIR",
                        str(ROOT / "build" / "cache" / "coverage-target"),
                    ),
                ),
            )
        )
    else:
        checks.append(
            Check(
                "rust-tests",
                ("cargo", "test", "--all-features", "--locked"),
            )
        )

    checks.extend(
        [
            Check(
                "no-default-features",
                ("cargo", "check", "--no-default-features", "--locked"),
            ),
            Check(
                "python-helpers",
                (
                    os.sys.executable,
                    "-m",
                    "unittest",
                    "discover",
                    "-s",
                    "scripts",
                    "-p",
                    "test_*.py",
                ),
            ),
            Check(
                "vendored-gp-routing",
                (
                    "cargo",
                    "test",
                    "--manifest-path",
                    "vendor/gp-routing/Cargo.toml",
                    "--locked",
                ),
            ),
        ]
    )

    if mutation:
        checks.append(
            Check(
                "mutation-testing",
                (
                    "cargo",
                    "mutants",
                    "--all-features",
                ),
            )
        )

    return tuple(checks)


def run_check(check: Check) -> dict[str, object]:
    log_path = REPORT_DIR / f"{check.name}.log"
    environment = os.environ.copy()
    environment["CARGO_INCREMENTAL"] = "0"
    environment.update(check.env)
    existing_profiles = set(root_coverage_profiles())
    started = time.monotonic()
    with log_path.open("w", encoding="utf-8") as log:
        try:
            process = subprocess.run(
                check.command,
                cwd=check.cwd,
                env=environment,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
            exit_code = process.returncode
        except FileNotFoundError as error:
            log.write(f"missing executable: {error}\n")
            exit_code = 127
    if check.name == "llvm-coverage":
        for profile in set(root_coverage_profiles()) - existing_profiles:
            profile.unlink(missing_ok=True)
    elapsed = time.monotonic() - started
    return {
        "name": check.name,
        "command": list(check.command),
        "exit_code": exit_code,
        "elapsed_seconds": round(elapsed, 3),
        "log": str(log_path.relative_to(ROOT)),
    }


def root_coverage_profiles():
    return ROOT.glob("default_*.profraw")


def status_paths(status: str) -> list[str]:
    records = status.split("\0")
    paths: list[str] = []
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if not record:
            continue
        status_code, path = record[:2], record[3:]
        if "R" in status_code or "C" in status_code:
            previous_path = records[index] if index < len(records) else path
            index += 1
            paths.append(f"{previous_path} -> {path}")
        else:
            paths.append(path)
    return paths


def dirty_files() -> list[str]:
    return sorted(set(status_paths(git_output("status", "--porcelain=v1", "-z"))))


def coverage_summary() -> dict[str, dict[str, object]] | None:
    index = ROOT / "build" / "reports" / "coverage" / "html" / "index.html"
    if not index.exists():
        return None
    html = index.read_text(encoding="utf-8")
    percentages = re.findall(r"<pre>\s*([0-9.]+)% \((\d+)/(\d+)\)</pre>", html)
    if len(percentages) < 3:
        return None
    function, line, region = percentages[-3:]
    return {
        "functions": {
            "percent": float(function[0]),
            "covered": int(function[1]),
            "coverable": int(function[2]),
        },
        "lines": {
            "percent": float(line[0]),
            "covered": int(line[1]),
            "coverable": int(line[2]),
        },
        "regions": {
            "percent": float(region[0]),
            "covered": int(region[1]),
            "coverable": int(region[2]),
        },
    }


def write_receipt(
    *, fast: bool, coverage: bool, mutation: bool, results: Sequence[dict[str, object]]
) -> None:
    receipt = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "agent": "Codex",
        "mode": "fast" if fast else "default",
        "coverage": coverage,
        "mutation": mutation,
        "platform": platform.platform(),
        "source_revision": git_output("rev-parse", "HEAD"),
        "source_dirty": dirty_files(),
        "scope": (
            "Deterministic offline quality checks; no live provider is called. "
            "Coverage and mutation are included only when requested."
        ),
        "checks": list(results),
        **({"coverage_summary": coverage_summary()} if coverage else {}),
        "aggregate_passed": all(result["exit_code"] == 0 for result in results),
    }
    RECEIPT_PATH.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--fast", action="store_true", help="run property invariants only"
    )
    mode.add_argument(
        "--coverage", action="store_true", help="run LLVM source coverage"
    )
    mode.add_argument(
        "--mutation", action="store_true", help="run mutation diagnostics"
    )
    parser.add_argument("--list", action="store_true", help="list checks and exit")
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    checks = selected_checks(
        fast=arguments.fast,
        coverage=arguments.coverage,
        mutation=arguments.mutation,
    )
    if arguments.list:
        for check in checks:
            print(" ".join(check.command))
        return 0

    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    RECEIPT_PATH.unlink(missing_ok=True)
    results = [run_check(check) for check in checks]
    write_receipt(
        fast=arguments.fast,
        coverage=arguments.coverage,
        mutation=arguments.mutation,
        results=results,
    )
    for result in results:
        marker = "PASS" if result["exit_code"] == 0 else "FAIL"
        print(
            f"{marker}: {result['name']} "
            f"({result['elapsed_seconds']:.3f}s, log={result['log']})",
            file=os.sys.stderr,
        )
    return 0 if all(result["exit_code"] == 0 for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
