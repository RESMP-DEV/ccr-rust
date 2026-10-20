#!/usr/bin/env python3
"""Summarize bounded CCR worker activity and listener tier counters."""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.parse
import urllib.request
from collections import Counter
from dataclasses import asdict, dataclass
from datetime import date, datetime, timedelta
from pathlib import Path

DEFAULT_METRICS_URL = "http://127.0.0.1:3457/metrics"
DEFAULT_RUNS_DIR = Path.home() / ".cache" / "ccr-worker-runs"
REQUEST_METRIC = re.compile(r'^ccr_requests_total\{tier="([^"]+)"\}\s+([0-9.]+)')


@dataclass(frozen=True)
class ListenerUsage:
    url: str
    active_requests: int | None
    tier_requests: dict[str, int]


@dataclass(frozen=True)
class RunUsage:
    days: int
    runs: int
    outcomes: dict[str, int]
    timeout_profiles: dict[str, int]
    read_only: int
    network_allowed: int
    requested_routes: dict[str, int]
    elapsed_seconds_total: float


def parse_listener_metrics(metrics: str, url: str) -> ListenerUsage:
    active_requests: int | None = None
    tier_requests: dict[str, int] = {}
    for line in metrics.splitlines():
        if line.startswith("ccr_active_requests "):
            active_requests = int(float(line.split(maxsplit=1)[1]))
            continue
        match = REQUEST_METRIC.match(line)
        if match:
            tier_requests[match.group(1)] = int(float(match.group(2)))
    if active_requests is None:
        raise ValueError("metrics do not expose ccr_active_requests")
    return ListenerUsage(url, active_requests, tier_requests)


def validate_local_http_url(url: str) -> None:
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "localhost"}:
        raise ValueError("--url must be an HTTP loopback listener URL")


def read_listener_usage(url: str, timeout: float) -> ListenerUsage:
    validate_local_http_url(url)
    request = urllib.request.Request(url, headers={"accept": "text/plain"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        if response.status != 200:
            raise ValueError(f"metrics endpoint returned HTTP {response.status}")
        return parse_listener_metrics(response.read().decode("utf-8"), url)


def run_date(run_dir: Path) -> date | None:
    match = re.match(r"(\d{8})-\d{6}-", run_dir.name)
    if match is None:
        return None
    return datetime.strptime(match.group(1), "%Y%m%d").date()


def summarize_runs(runs_dir: Path, days: int) -> RunUsage:
    if days < 1:
        raise ValueError("--days must be positive")
    cutoff = datetime.now().date() - timedelta(days=days - 1)
    outcomes: Counter[str] = Counter()
    profiles: Counter[str] = Counter()
    routes: Counter[str] = Counter()
    read_only = 0
    network_allowed = 0
    elapsed_total = 0.0
    count = 0
    for status_path in sorted(runs_dir.glob("*/status.json")):
        if (run_day := run_date(status_path.parent)) is None or run_day < cutoff:
            continue
        try:
            status = json.loads(status_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            outcomes["unreadable"] += 1
            count += 1
            continue
        outcomes[str(status.get("outcome") or "receipt_pending")] += 1
        profiles[str(status.get("timeout_profile") or "unspecified")] += 1
        routes[str(status.get("requested_route") or "unassigned")] += 1
        read_only += int(bool(status.get("read_only")))
        network_allowed += int(bool(status.get("network_allowed")))
        elapsed_total += float(status.get("elapsed_seconds") or 0)
        count += 1
    return RunUsage(
        days=days,
        runs=count,
        outcomes=dict(sorted(outcomes.items())),
        timeout_profiles=dict(sorted(profiles.items())),
        read_only=read_only,
        network_allowed=network_allowed,
        requested_routes=dict(sorted(routes.items())),
        elapsed_seconds_total=round(elapsed_total, 3),
    )


def print_usage(
    listener: ListenerUsage | None, runs: RunUsage | None, json_output: bool
) -> None:
    result: dict[str, object] = {}
    if listener is not None:
        result["listener"] = asdict(listener)
    if runs is not None:
        result["runs"] = asdict(runs)
    if json_output:
        print(json.dumps(result, indent=2, sort_keys=True))
        return
    if listener is not None:
        print(f"listener: {listener.url}")
        print(f"  active_requests: {listener.active_requests}")
        for tier, requests in sorted(listener.tier_requests.items()):
            print(f"  requests_total[{tier}]: {requests}")
    if runs is not None:
        print(f"runs: last {runs.days} day(s), count={runs.runs}")
        print(f"  outcomes: {json.dumps(runs.outcomes, sort_keys=True)}")
        print(
            f"  timeout_profiles: {json.dumps(runs.timeout_profiles, sort_keys=True)}"
        )
        print(
            f"  requested_routes: {json.dumps(runs.requested_routes, sort_keys=True)}"
        )
        print(
            "  permissions: "
            f"read_only={runs.read_only}, network_allowed={runs.network_allowed}"
        )
        print(f"  elapsed_seconds_total: {runs.elapsed_seconds_total}")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=DEFAULT_METRICS_URL)
    parser.add_argument("--runs-dir", type=Path, default=DEFAULT_RUNS_DIR)
    parser.add_argument("--days", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("--runs-only", action="store_true")
    parser.add_argument("--listener-only", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.runs_only and args.listener_only:
        parser.error("--runs-only and --listener-only are mutually exclusive")
    if args.days < 1:
        parser.error("--days must be positive")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        listener = (
            read_listener_usage(args.url, args.timeout) if not args.runs_only else None
        )
        runs = None if args.listener_only else summarize_runs(args.runs_dir, args.days)
    except (OSError, ValueError) as error:
        print(f"ccr-worker-usage: {error}", file=sys.stderr)
        return 1
    print_usage(listener, runs, args.json)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
