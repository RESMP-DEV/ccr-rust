#!/usr/bin/env python3
"""Objective per-model benchmark harness for the local CCR HTTP API.

The harness sends a fixed, verifiable task suite (see
``scripts/ccr_model_benchmark_suite.py``) to explicit provider routes such as
``zai,glm-5.4`` and writes one durable scorecard per route under
``build/reports/model-benchmark/<slug>.json``.

Two measurements are kept apart on purpose. Correctness is a boolean per run
and is decided by a checker that reads the whole response. Speed is measured
alongside it (TTFT, wall time, tokens per second) and never substitutes for
correctness: a fast wrong answer is a failed run.

Usage:

    python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --reps 3
    python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --dry-run
    python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --json

Only the Python standard library is used. The harness never changes routing,
configuration, or live services; it only issues chat completion requests
against the local listener.
"""

from __future__ import annotations

import argparse
import json
import platform
import re
import statistics
import sys
from collections import Counter
from collections.abc import Sequence
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import ccr_model_benchmark_suite as suite
import ccr_model_benchmark_transport as transport

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASE_URL = "http://127.0.0.1:3456"
DEFAULT_OUT_DIR = ROOT / "build" / "reports" / "model-benchmark"
DEFAULT_REPETITIONS = 3
SCORECARD_SCHEMA = "ccr-model-benchmark/v1"

NON_CLAIMS = [
    "Scores come from one fixed prompt suite at one moment in time; they do "
    "not measure general model quality, reasoning depth, or long-horizon "
    "agentic behavior.",
    "Latency and throughput describe this machine, this listener, and this "
    "network path; they are not provider-intrinsic numbers.",
    "Skipped scenarios contribute no accuracy signal: the accuracy "
    "denominator counts attempted runs only.",
    "Token counts use upstream-reported usage when the route reports it and "
    "a character-based estimate otherwise, so estimated tokens make "
    "tokens/second approximate.",
    "The code-fix checker executes model-generated Python in a local "
    "subprocess with a timeout; that is not a security sandbox.",
    "The vision scenario is skipped unless the route is declared "
    "vision-capable; an undeclared route is never sent an image.",
    "No score here proves which route is better for a real workflow; it only "
    "compares responses to these fixed checks.",
    "A run whose served tier names a different provider than the requested "
    "route was answered by a fallback tier, so that run's score does not "
    "describe the requested model. A clean per-model comparison needs a "
    "single-tier listener or an inspection of the recorded served tier.",
]


@dataclass
class RunRecord:
    """One scenario execution for one route."""

    scenario_id: str
    title: str
    kind: str
    repetition: int
    passed: bool | None
    skipped: bool
    skip_reason: str | None
    ttft_seconds: float | None
    wall_seconds: float
    output_tokens: int | None
    input_tokens: int | None
    usage_source: str
    error_class: str
    error_detail: str | None
    output_chars: int
    output_preview: str
    # Tier provenance: the tier that served this run and the routes the
    # router reported as rate limited. Both default to absent so a record
    # built without them stays valid.
    served_tier: str | None = None
    rate_limited_tiers: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "scenario_id": self.scenario_id,
            "title": self.title,
            "kind": self.kind,
            "repetition": self.repetition,
            "passed": self.passed,
            "skipped": self.skipped,
            "skip_reason": self.skip_reason,
            "ttft_seconds": self.ttft_seconds,
            "wall_seconds": round(self.wall_seconds, 4),
            "output_tokens": self.output_tokens,
            "input_tokens": self.input_tokens,
            "usage_source": self.usage_source,
            "error_class": self.error_class,
            "error_detail": self.error_detail,
            "output_chars": self.output_chars,
            "output_preview": self.output_preview,
            "served_tier": self.served_tier,
            "rate_limited_tiers": list(self.rate_limited_tiers),
        }


@dataclass
class RouteResult:
    """Every run recorded for one route."""

    route: str
    runs: list[RunRecord] = field(default_factory=list)
    skipped: list[dict[str, str]] = field(default_factory=list)

    @property
    def slug(self) -> str:
        return route_slug(self.route)


def route_slug(route: str) -> str:
    """Return a filesystem-safe slug for a provider route id."""
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "_", route.strip())
    return cleaned.strip("_") or "route"


def route_provider(route: str) -> str:
    """Return the provider component of a `provider,model` route id."""
    return route.split(",", 1)[0].strip().lower()


def tier_provider(tier: str | None) -> str | None:
    """Return the provider component of an `x-ccr-tier` header value.

    `x-ccr-tier` carries the configured display name of the tier that served
    the request, which is the provider name unless the provider configures a
    custom `tier_name`. A display name is therefore compared as-is, and a
    provider with a custom `tier_name` reads as a different provider than
    its route id names.
    """
    if tier is None:
        return None
    value = tier.split(",", 1)[0].strip().lower()
    return value or None


def build_messages(scenario: suite.Scenario) -> list[dict[str, Any]]:
    """Build the chat messages for a scenario, including image parts."""
    if scenario.requires_vision:
        if scenario.image_png is None:
            raise ValueError(f"scenario {scenario.scenario_id} has no image fixture")
        return [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": scenario.prompt},
                    {
                        "type": "image_url",
                        "image_url": {"url": suite.png_data_url(scenario.image_png)},
                    },
                ],
            }
        ]
    return [{"role": "user", "content": scenario.prompt}]


def build_payload(
    route: str, scenario: suite.Scenario, temperature: float
) -> dict[str, Any]:
    """Build the chat completion request for one scenario."""
    return {
        "model": route,
        "messages": build_messages(scenario),
        "max_tokens": scenario.max_output_tokens,
        "temperature": temperature,
        "stream": True,
        "stream_options": {"include_usage": True},
    }


def execute_scenario(
    route: str,
    scenario: suite.Scenario,
    repetition: int,
    base_url: str,
    api_key: str,
    temperature: float,
    timeout_seconds: float,
) -> RunRecord:
    """Run one scenario once and record pass/fail plus measurements."""
    payload = build_payload(route, scenario, temperature)
    outcome = transport.stream_chat_completion(
        base_url, api_key, payload, timeout_seconds
    )
    text = outcome.text
    if outcome.error_class is not None:
        return RunRecord(
            scenario_id=scenario.scenario_id,
            title=scenario.title,
            kind=scenario.kind,
            repetition=repetition,
            passed=False,
            skipped=False,
            skip_reason=None,
            ttft_seconds=outcome.ttft_seconds,
            wall_seconds=outcome.wall_seconds,
            output_tokens=outcome.output_tokens,
            input_tokens=outcome.input_tokens,
            usage_source=outcome.usage_source,
            error_class=outcome.error_class,
            error_detail=outcome.error_detail,
            output_chars=len(text),
            output_preview=text[:240],
            served_tier=outcome.served_tier,
            rate_limited_tiers=list(outcome.rate_limited_tiers),
        )
    verdict = suite.run_checker(scenario, text)
    return RunRecord(
        scenario_id=scenario.scenario_id,
        title=scenario.title,
        kind=scenario.kind,
        repetition=repetition,
        passed=verdict.passed,
        skipped=False,
        skip_reason=None,
        ttft_seconds=outcome.ttft_seconds,
        wall_seconds=outcome.wall_seconds,
        output_tokens=outcome.output_tokens,
        input_tokens=outcome.input_tokens,
        usage_source=outcome.usage_source,
        error_class="none" if verdict.passed else "check_failed",
        error_detail=None if verdict.passed else verdict.detail,
        output_chars=len(text),
        output_preview=text[:240],
        served_tier=outcome.served_tier,
        rate_limited_tiers=list(outcome.rate_limited_tiers),
    )


def _median(values: Sequence[float]) -> float | None:
    if not values:
        return None
    return float(statistics.median(values))


def aggregate_tier_provenance(
    runs: Sequence[RunRecord], route: str | None
) -> dict[str, Any]:
    """Count the runs whose response was not served by the requested route.

    A run is contaminated when the tier that served it names a different
    provider than the requested route, or when the router reported rate
    limited tiers for that run. The two conditions are counted separately and
    then combined, so a run that was both rate limited and served by another
    tier is one contaminated run rather than two.

    A run that reported no served tier is not counted as contaminated; it is
    counted as unknown provenance instead, because a missing header is not
    evidence of a fallback.
    """
    attempted = [run for run in runs if not run.skipped]
    requested_provider = route_provider(route) if route else None
    served_tiers: Counter[str] = Counter()
    rate_limited_names: Counter[str] = Counter()
    runs_with_served_tier = 0
    mismatch_runs = 0
    rate_limited_runs = 0
    contaminated_runs = 0
    for run in attempted:
        served_provider = tier_provider(run.served_tier)
        mismatch = (
            served_provider is not None
            and requested_provider is not None
            and served_provider != requested_provider
        )
        rate_limited = bool(run.rate_limited_tiers)
        if run.served_tier:
            runs_with_served_tier += 1
            served_tiers[run.served_tier] += 1
        if mismatch:
            mismatch_runs += 1
        if rate_limited:
            rate_limited_runs += 1
            for name in run.rate_limited_tiers:
                rate_limited_names[name] += 1
        if mismatch or rate_limited:
            contaminated_runs += 1
    return {
        "requested_provider": requested_provider,
        "runs_with_served_tier": runs_with_served_tier,
        "runs_without_served_tier": len(attempted) - runs_with_served_tier,
        "served_tier_mismatch_runs": mismatch_runs,
        "rate_limited_runs": rate_limited_runs,
        "contaminated_runs": contaminated_runs,
        "served_tiers": dict(sorted(served_tiers.items())),
        "rate_limited_tier_names": dict(sorted(rate_limited_names.items())),
        "clean": contaminated_runs == 0 and runs_with_served_tier == len(attempted),
    }


def aggregate_runs(
    runs: Sequence[RunRecord], route: str | None = None
) -> dict[str, Any]:
    """Aggregate per-run records into the reported scorecard summary."""
    attempted = [run for run in runs if not run.skipped]
    passed = [run for run in attempted if run.passed]
    failed = [run for run in attempted if not run.passed]

    ttft_values = [
        run.ttft_seconds for run in attempted if run.ttft_seconds is not None
    ]
    wall_values = [run.wall_seconds for run in attempted]

    generated_seconds = 0.0
    generated_tokens = 0
    for run in attempted:
        if run.output_tokens is None or run.ttft_seconds is None:
            continue
        span = run.wall_seconds - run.ttft_seconds
        if span <= 0:
            continue
        generated_seconds += span
        generated_tokens += run.output_tokens
    tokens_per_second = (
        generated_tokens / generated_seconds if generated_seconds > 0 else None
    )

    by_scenario: dict[str, list[RunRecord]] = {}
    for run in attempted:
        by_scenario.setdefault(run.scenario_id, []).append(run)
    stable_runs = 0
    per_scenario: dict[str, Any] = {}
    for scenario_id, scenario_runs in sorted(by_scenario.items()):
        scenario_passed = sum(1 for run in scenario_runs if run.passed)
        scenario_failed = len(scenario_runs) - scenario_passed
        stable_runs += max(scenario_passed, scenario_failed)
        per_scenario[scenario_id] = {
            "attempts": len(scenario_runs),
            "passed": scenario_passed,
            "failed": scenario_failed,
            "accuracy": scenario_passed / len(scenario_runs),
            "median_ttft_seconds": _median(
                [
                    run.ttft_seconds
                    for run in scenario_runs
                    if run.ttft_seconds is not None
                ]
            ),
            "median_wall_seconds": _median([run.wall_seconds for run in scenario_runs]),
        }

    error_classes = Counter(
        run.error_class for run in attempted if run.error_class != "none"
    )
    attempted_count = len(attempted)
    return {
        "runs_attempted": attempted_count,
        "runs_passed": len(passed),
        "runs_failed": len(failed),
        "runs_skipped": sum(1 for run in runs if run.skipped),
        "accuracy": (len(passed) / attempted_count) if attempted_count else None,
        "failure_rate": (
            (attempted_count - len(passed)) / attempted_count
            if attempted_count
            else None
        ),
        "median_ttft_seconds": _median(ttft_values),
        "median_wall_seconds": _median(wall_values),
        "tokens_per_second": tokens_per_second,
        "stability": (stable_runs / attempted_count) if attempted_count else None,
        "error_classes": dict(sorted(error_classes.items())),
        "tier_provenance": aggregate_tier_provenance(runs, route),
        "per_scenario": per_scenario,
    }


def build_scorecard(
    route: str,
    runs: Sequence[RunRecord],
    skipped: Sequence[dict[str, str]],
    config: dict[str, Any],
    mode: str,
    scenarios: Sequence[suite.Scenario],
) -> dict[str, Any]:
    """Build the durable scorecard document for one route."""
    return {
        "schema": SCORECARD_SCHEMA,
        "mode": mode,
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "route": route,
        "suite": {
            "id": suite.SUITE_ID,
            "version": suite.SUITE_VERSION,
            "scenarios": [suite.scenario_plan(item) for item in scenarios],
        },
        "config": config,
        "environment": {
            "python": platform.python_version(),
            "platform": platform.platform(),
            "hostname": platform.node(),
        },
        "runs": [run.to_dict() for run in runs],
        "skipped": list(skipped),
        "summary": aggregate_runs(runs, route),
        "non_claims": list(NON_CLAIMS),
    }


def write_scorecard(
    out_dir: Path, route: str, scorecard: dict[str, Any], suffix: str = ""
) -> Path:
    """Write one scorecard and return its path."""
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{route_slug(route)}{suffix}.json"
    scorecard["scorecard_path"] = str(path)
    path.write_text(
        json.dumps(scorecard, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return path


def parse_routes(values: Sequence[str]) -> list[str]:
    """Split repeated or semicolon-separated route arguments.

    Comma is never a separator: a provider route id is itself
    ``provider,model`` (for example ``zai,glm-5.4``).
    """
    routes: list[str] = []
    for value in values:
        for token in value.split(";"):
            route = token.strip()
            if route and route not in routes:
                routes.append(route)
    return routes


def skip_reason(
    scenario: suite.Scenario,
    route: str,
    vision_capable: Sequence[str],
    allow_execution: bool,
) -> str | None:
    """Return why a scenario is skipped for a route, or None when it runs."""
    if scenario.requires_vision and route not in vision_capable:
        return (
            f"skipped: route {route} is treated as text-only; declare it with "
            "--vision-capable to send the image fixture"
        )
    if scenario.requires_execution and not allow_execution:
        return "skipped: local snippet execution is disabled by --no-exec for this run"
    return None


def format_summary_line(route: str, summary: dict[str, Any]) -> str:
    """Format one compact comparison row."""
    accuracy = summary.get("accuracy")
    ttft = summary.get("median_ttft_seconds")
    wall = summary.get("median_wall_seconds")
    speed = summary.get("tokens_per_second")
    stability = summary.get("stability")

    def number(value: float | None, digits: int = 2) -> str:
        return "n/a" if value is None else f"{value:.{digits}f}"

    return (
        f"{route:<28} {number(accuracy):>5}  {number(ttft, 3):>7}s  "
        f"{number(wall, 3):>8}s  {number(speed, 1):>7}  "
        f"{number(stability):>5}  {number(summary.get('failure_rate')):>5}  "
        f"{summary.get('runs_passed')}/{summary.get('runs_attempted')} "
        f"(+{summary.get('runs_skipped')} skipped)"
    )


def format_tier_provenance_line(route: str, block: dict[str, Any]) -> str:
    """Format one compact tier-provenance row."""
    served = ", ".join(
        f"{name}:{count}" for name, count in (block.get("served_tiers") or {}).items()
    )
    return (
        f"{route:<28} served=[{served or 'none'}] "
        f"mismatch={block.get('served_tier_mismatch_runs', 0)} "
        f"rate-limited={block.get('rate_limited_runs', 0)} "
        f"contaminated={block.get('contaminated_runs', 0)} "
        f"unknown={block.get('runs_without_served_tier', 0)}"
    )


def print_summary(scorecards: Sequence[dict[str, Any]]) -> None:
    """Print the compact human-readable comparison."""
    print(
        "route                       acc  medTTFT   medWall   tok/s  stab   fail  runs"
    )
    for scorecard in scorecards:
        print(format_summary_line(scorecard["route"], scorecard["summary"]))
    skipped = [
        (scorecard["route"], entry)
        for scorecard in scorecards
        for entry in scorecard.get("skipped", [])
    ]
    if skipped:
        print("\nskipped scenarios:")
        for route, entry in skipped:
            print(f"  {route} {entry['scenario_id']}: {entry['reason']}")
    provenance = [
        (scorecard["route"], scorecard.get("summary", {}).get("tier_provenance", {}))
        for scorecard in scorecards
    ]
    if provenance:
        print("\ntier provenance:")
        for route, block in provenance:
            print(f"  {format_tier_provenance_line(route, block)}")


def skipped_record(scenario: suite.Scenario, repetition: int, reason: str) -> RunRecord:
    """Build the record written for a scenario that was not attempted."""
    return RunRecord(
        scenario_id=scenario.scenario_id,
        title=scenario.title,
        kind=scenario.kind,
        repetition=repetition,
        passed=None,
        skipped=True,
        skip_reason=reason,
        ttft_seconds=None,
        wall_seconds=0.0,
        output_tokens=None,
        input_tokens=None,
        usage_source="none",
        error_class="skipped",
        error_detail=None,
        output_chars=0,
        output_preview="",
    )


def build_parser() -> argparse.ArgumentParser:
    """Build the command line interface."""
    parser = argparse.ArgumentParser(
        description="Objective per-model benchmark over the local CCR HTTP API"
    )
    parser.add_argument(
        "--models",
        action="append",
        default=[],
        metavar="ROUTE",
        help="provider route to benchmark, for example zai,glm-5.4. Repeat the "
        "flag or separate several routes with ';'. A comma is part of the "
        "route id and is never a separator.",
    )
    parser.add_argument(
        "--base-url",
        default=DEFAULT_BASE_URL,
        help=f"CCR base URL (default: {DEFAULT_BASE_URL})",
    )
    parser.add_argument(
        "--api-key",
        default="test",
        help="bearer token sent to CCR (default: test)",
    )
    parser.add_argument(
        "--reps",
        type=int,
        default=DEFAULT_REPETITIONS,
        help=f"repetitions per scenario per route (default: {DEFAULT_REPETITIONS})",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=120.0,
        help="per-request timeout in seconds (default: 120)",
    )
    parser.add_argument(
        "--max-tokens",
        type=int,
        default=0,
        help="override the per-scenario output token budget (default: 0 keeps "
        "the scenario budget)",
    )
    parser.add_argument(
        "--temperature",
        type=float,
        default=0.0,
        help="sampling temperature (default: 0)",
    )
    parser.add_argument(
        "--out-dir",
        default=str(DEFAULT_OUT_DIR),
        help=f"scorecard directory (default: {DEFAULT_OUT_DIR})",
    )
    parser.add_argument(
        "--scenarios",
        action="append",
        default=[],
        metavar="ID",
        help="restrict the run to these scenario ids; repeatable or ';'-separated",
    )
    parser.add_argument(
        "--vision-capable",
        action="append",
        default=[],
        metavar="ROUTE",
        help="declare a route as able to accept image input; routes that are "
        "not declared skip the vision scenario with an explicit reason",
    )
    parser.add_argument(
        "--no-exec",
        action="store_true",
        help="skip the code-fix scenario instead of executing returned snippets",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="parse the suite, print the plan, and write dry-run scorecards "
        "without contacting the router",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="print the combined scorecards as JSON on stdout",
    )
    return parser


def select_scenarios(requested: Sequence[str]) -> tuple[suite.Scenario, ...]:
    """Return the suite, optionally restricted to the requested scenario ids."""
    scenarios = suite.build_scenarios()
    if not requested:
        return scenarios
    wanted = set(parse_routes(requested))
    selected = tuple(
        scenario for scenario in scenarios if scenario.scenario_id in wanted
    )
    if not selected:
        known = ", ".join(scenario.scenario_id for scenario in scenarios)
        raise ValueError(f"no known scenario matched {sorted(wanted)}; known: {known}")
    return selected


def apply_token_override(
    scenarios: Sequence[suite.Scenario], max_tokens: int
) -> tuple[suite.Scenario, ...]:
    """Return the scenarios with an optional output token override applied."""
    if max_tokens <= 0:
        return tuple(scenarios)
    return tuple(
        suite.Scenario(
            scenario_id=scenario.scenario_id,
            title=scenario.title,
            kind=scenario.kind,
            prompt=scenario.prompt,
            max_output_tokens=max_tokens,
            requires_vision=scenario.requires_vision,
            requires_execution=scenario.requires_execution,
            expected_text=scenario.expected_text,
            expected_json=dict(scenario.expected_json),
            expected_number=scenario.expected_number,
            expected_color=scenario.expected_color,
            function_name=scenario.function_name,
            test_source=scenario.test_source,
            image_png=scenario.image_png,
        )
        for scenario in scenarios
    )


def run_dry_run(
    routes: Sequence[str],
    scenarios: Sequence[suite.Scenario],
    vision_capable: Sequence[str],
    allow_execution: bool,
    config: dict[str, Any],
    out_dir: Path,
) -> list[dict[str, Any]]:
    """Validate the suite and write dry-run scorecards without any request."""
    scorecards: list[dict[str, Any]] = []
    for route in routes:
        runs: list[RunRecord] = []
        skipped: list[dict[str, str]] = []
        for scenario in scenarios:
            reason = skip_reason(scenario, route, vision_capable, allow_execution)
            if reason is None:
                continue
            skipped.append(
                {
                    "scenario_id": scenario.scenario_id,
                    "title": scenario.title,
                    "reason": reason,
                }
            )
            runs.append(skipped_record(scenario, 0, reason))
        scorecard = build_scorecard(route, runs, skipped, config, "dry-run", scenarios)
        write_scorecard(out_dir, route, scorecard, suffix=".dry-run")
        scorecards.append(scorecard)
    return scorecards


def print_dry_run_plan(
    routes: Sequence[str],
    scenarios: Sequence[suite.Scenario],
    vision_capable: Sequence[str],
    allow_execution: bool,
    scorecards: Sequence[dict[str, Any]],
) -> None:
    """Print the parsed plan so a dry run proves the suite was understood."""
    print(
        f"dry run: suite {suite.SUITE_ID} {suite.SUITE_VERSION} "
        f"({len(scenarios)} scenarios, {len(routes)} route(s))"
    )
    for route in routes:
        print(f"route {route}")
        for scenario in scenarios:
            reason = skip_reason(scenario, route, vision_capable, allow_execution)
            state = "skip" if reason else "run"
            print(
                f"  [{state:>4}] {scenario.scenario_id:<20} kind={scenario.kind:<12} "
                f"max_tokens={scenario.max_output_tokens}"
            )
            if reason:
                print(f"         {reason}")
    for scorecard in scorecards:
        print(
            f"dry-run scorecard: {scorecard['scorecard_path']} "
            f"({len(scorecard['skipped'])} skipped scenario(s))"
        )


def main(argv: Sequence[str] | None = None) -> int:
    """Run the benchmark and return the process exit code."""
    parser = build_parser()
    arguments = parser.parse_args(argv)

    routes = parse_routes(arguments.models)
    if not routes:
        parser.error(
            "at least one --models route is required, for example --models zai,glm-5.4"
        )
    if arguments.reps < 1:
        parser.error("--reps must be at least 1")
    try:
        scenarios = select_scenarios(arguments.scenarios)
    except ValueError as error:
        parser.error(str(error))
        raise AssertionError("unreachable") from error

    vision_capable = parse_routes(arguments.vision_capable)
    allow_execution = not arguments.no_exec
    out_dir = Path(arguments.out_dir)
    config = {
        "base_url": arguments.base_url,
        "repetitions": arguments.reps,
        "timeout_seconds": arguments.timeout,
        "temperature": arguments.temperature,
        "max_output_tokens_override": arguments.max_tokens,
        "streaming": True,
        "snippet_execution": allow_execution,
        "vision_capable_routes": list(vision_capable),
        "suite_digest": {
            scenario.scenario_id: suite.scenario_file_digest(scenario)
            for scenario in scenarios
        },
    }

    if arguments.dry_run:
        scorecards = run_dry_run(
            routes, scenarios, vision_capable, allow_execution, config, out_dir
        )
        if arguments.json:
            print(
                json.dumps(
                    {"schema": SCORECARD_SCHEMA, "routes": scorecards},
                    indent=2,
                    sort_keys=True,
                )
            )
        else:
            print_dry_run_plan(
                routes, scenarios, vision_capable, allow_execution, scorecards
            )
        return 0

    scenarios = apply_token_override(scenarios, arguments.max_tokens)
    scorecards: list[dict[str, Any]] = []
    failures = 0
    for route in routes:
        runs: list[RunRecord] = []
        skipped: list[dict[str, str]] = []
        for repetition in range(1, arguments.reps + 1):
            for scenario in scenarios:
                reason = skip_reason(scenario, route, vision_capable, allow_execution)
                if reason is not None:
                    runs.append(skipped_record(scenario, repetition, reason))
                    if not any(
                        entry["scenario_id"] == scenario.scenario_id
                        for entry in skipped
                    ):
                        skipped.append(
                            {
                                "scenario_id": scenario.scenario_id,
                                "title": scenario.title,
                                "reason": reason,
                            }
                        )
                    continue
                record = execute_scenario(
                    route=route,
                    scenario=scenario,
                    repetition=repetition,
                    base_url=arguments.base_url,
                    api_key=arguments.api_key,
                    temperature=arguments.temperature,
                    timeout_seconds=arguments.timeout,
                )
                runs.append(record)
                if record.passed is not True:
                    failures += 1
                print(
                    f"[{'PASS' if record.passed else 'FAIL'}] {route} "
                    f"{scenario.scenario_id} rep={repetition} "
                    f"wall={record.wall_seconds:.3f}s "
                    f"ttft={record.ttft_seconds if record.ttft_seconds is not None else 'n/a'} "
                    f"detail={record.error_detail or 'ok'}",
                    file=sys.stderr,
                )
        scorecard = build_scorecard(route, runs, skipped, config, "live", scenarios)
        write_scorecard(out_dir, route, scorecard)
        scorecards.append(scorecard)

    if arguments.json:
        print(
            json.dumps(
                {"schema": SCORECARD_SCHEMA, "routes": scorecards},
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print_summary(scorecards)
        for scorecard in scorecards:
            print(f"scorecard: {scorecard['scorecard_path']}")

    if failures:
        print(
            f"{failures} attempted run(s) failed a checker or errored",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
