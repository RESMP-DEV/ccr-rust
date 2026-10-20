#!/usr/bin/env python3
"""Run the deterministic CCR-Rust quality matrix and write a receipt."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import sys
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
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        message = result.stderr.strip()
        raise RuntimeError(f"git {' '.join(arguments)} failed: {message}")
    return result.stdout.rstrip("\n")


def git_state() -> tuple[str, list[str]]:
    try:
        return git_output("rev-parse", "HEAD"), dirty_files()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        return "unknown", [f"git metadata unavailable: {error}"]


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
            "python-format",
            (
                "ruff",
                "format",
                "--check",
                "benchmarks",
                "scripts",
                "tests/provider_validation",
            ),
        ),
        Check(
            "python-lint",
            (
                "ruff",
                "check",
                "benchmarks",
                "scripts",
                "tests/provider_validation",
            ),
        ),
        Check("python-typecheck", ("ty", "check", "scripts")),
        Check(
            "rust-sloc-ratchet",
            (sys.executable, "scripts/check_sloc.py", "--json"),
        ),
        Check(
            "shellcheck",
            (
                "shellcheck",
                "benchmarks/run_stress_test.sh",
                "examples/smoke-test.sh",
            ),
        ),
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
                "debug-binary",
                ("cargo", "build", "--all-features", "--locked"),
                env=(("CARGO_TARGET_DIR", str(ROOT / "target")),),
            ),
            Check(
                "python-helpers",
                (
                    sys.executable,
                    "-m",
                    "unittest",
                    "discover",
                    "-s",
                    "scripts",
                    "-p",
                    "test_*.py",
                ),
                env=(
                    (
                        "CCR_TEST_BINARY",
                        str(ROOT / "target" / "debug" / "ccr-rust"),
                    ),
                    ("CARGO_TARGET_DIR", str(ROOT / "target")),
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
        except OSError as error:
            log.write(f"could not run command: {error}\n")
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


def prepare_coverage_report() -> None:
    """Create the report directory and invalidate a previous HTML summary."""
    output_dir = ROOT / "build" / "reports" / "coverage"
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "html" / "index.html").unlink(missing_ok=True)


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
    """Return label-matched totals from the current llvm-cov HTML report."""
    index = ROOT / "build" / "reports" / "coverage" / "html" / "index.html"
    if not index.exists():
        return None
    html = index.read_text(encoding="utf-8")
    return parse_coverage_summary(html)


def parse_coverage_summary(html: str) -> dict[str, dict[str, object]]:
    """Parse llvm-cov totals by header label instead of positional order."""
    header_match = re.search(r"<tr>(.*?)</tr>", html, re.IGNORECASE)
    totals_match = re.search(
        r"<tr[^>]*>\s*<td[^>]*>\s*<pre>\s*Totals\s*</pre>\s*</td>(.*?)</tr>",
        html,
        re.IGNORECASE,
    )
    if header_match is None or totals_match is None:
        raise ValueError("llvm-cov HTML summary table is missing or unreadable")

    headers = [
        value.strip()
        for value in re.findall(r"<td[^>]*>\s*([^<]+?)\s*</td>", header_match.group(1))
    ]
    totals = re.findall(
        r"<pre>\s*([0-9.]+)% \((\d+)/(\d+)\)\s*</pre>", totals_match.group(1)
    )
    expected = ("Function Coverage", "Line Coverage", "Region Coverage")
    if tuple(headers[1:4]) != expected or len(totals) != 3:
        raise ValueError("llvm-cov HTML summary columns are unsupported or incomplete")

    values = {
        "functions": totals[0],
        "lines": totals[1],
        "regions": totals[2],
    }
    return {
        name: {
            "percent": float(value[0]),
            "covered": int(value[1]),
            "coverable": int(value[2]),
        }
        for name, value in values.items()
    }


def write_receipt(
    *, fast: bool, coverage: bool, mutation: bool, results: Sequence[dict[str, object]]
) -> None:
    source_revision, source_dirty = git_state()
    summary: dict[str, dict[str, object]] | None = None
    summary_error: str | None = None
    if coverage:
        try:
            summary = coverage_summary()
            if summary is None:
                summary_error = "coverage HTML index was not generated"
        except (OSError, ValueError) as error:
            summary_error = str(error)

    receipt = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "agent": os.environ.get("QUALITY_AGENT", "Codex"),
        "mode": "+".join(
            [
                name
                for name, enabled in (
                    ("fast", fast),
                    ("coverage", coverage),
                    ("mutation", mutation),
                )
                if enabled
            ]
        )
        or "default",
        "coverage": coverage,
        "mutation": mutation,
        "platform": platform.platform(),
        "source_revision": source_revision,
        "source_dirty": source_dirty,
        "scope": (
            "Deterministic offline quality checks; no live provider is called. "
            "Coverage and mutation are included only when requested."
        ),
        "checks": list(results),
        **(
            {"coverage_summary": summary, "coverage_summary_error": summary_error}
            if coverage
            else {}
        ),
        "aggregate_passed": all(result["exit_code"] == 0 for result in results),
    }
    RECEIPT_PATH.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    fast_mode = parser.add_mutually_exclusive_group()
    fast_mode.add_argument(
        "--fast", action="store_true", help="run property invariants only"
    )
    fast_mode.add_argument(
        "--coverage", action="store_true", help="run LLVM source coverage"
    )
    parser.add_argument(
        "--mutation", action="store_true", help="add mutation diagnostics"
    )
    parser.add_argument("--list", action="store_true", help="list checks and exit")
    arguments = parser.parse_args(argv)
    if arguments.fast and arguments.mutation:
        parser.error("--mutation cannot be combined with --fast")
    return arguments


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

    if arguments.coverage:
        prepare_coverage_report()
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
            file=sys.stderr,
        )
    return 0 if all(result["exit_code"] == 0 for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
