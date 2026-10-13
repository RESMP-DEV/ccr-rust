#!/usr/bin/env python3
"""Wait until a CCR listener has no active requests or streams."""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Sequence
from dataclasses import asdict, dataclass

REQUEST_PATTERN = re.compile(r"^ccr_active_requests\s+([0-9.eE+-]+)$")
STREAM_PATTERN = re.compile(r"^ccr_active_streams\s+([0-9.eE+-]+)$")


@dataclass(frozen=True)
class IdleSnapshot:
    active_requests: float
    active_streams: float


class BusyListener(RuntimeError):
    pass


def parse_metrics(metrics: str) -> IdleSnapshot:
    requests: float | None = None
    streams: float | None = None
    for line in metrics.splitlines():
        request_match = REQUEST_PATTERN.match(line)
        if request_match:
            requests = float(request_match.group(1))
            continue
        stream_match = STREAM_PATTERN.match(line)
        if stream_match:
            streams = float(stream_match.group(1))
    if requests is None or streams is None:
        raise ValueError(
            "metrics do not expose both ccr_active_requests and ccr_active_streams"
        )
    return IdleSnapshot(active_requests=requests, active_streams=streams)


def read_metrics(url: str, timeout: float) -> IdleSnapshot:
    request = urllib.request.Request(url, headers={"accept": "text/plain"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        if response.status != 200:
            raise ValueError(f"metrics endpoint returned HTTP {response.status}")
        return parse_metrics(response.read().decode("utf-8"))


def wait_until_idle(
    url: str,
    *,
    timeout: float,
    poll_interval: float,
    stable_samples: int,
    request_timeout: float,
) -> tuple[IdleSnapshot, float, int]:
    """Return the final snapshot, elapsed seconds, and samples observed."""
    if stable_samples < 2:
        raise ValueError("stable_samples must be at least 2")

    started = time.monotonic()
    consecutive_idle = 0
    samples = 0
    last_error: Exception | None = None
    deadline = started + timeout

    while time.monotonic() < deadline:
        try:
            snapshot = read_metrics(url, request_timeout)
        except (OSError, ValueError, urllib.error.URLError) as error:
            last_error = error
            consecutive_idle = 0
        else:
            samples += 1
            if snapshot.active_requests == 0 and snapshot.active_streams == 0:
                consecutive_idle += 1
                if consecutive_idle >= stable_samples:
                    return snapshot, time.monotonic() - started, samples
            else:
                consecutive_idle = 0
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        time.sleep(min(poll_interval, remaining))

    if last_error is not None:
        raise BusyListener(
            f"listener did not provide stable idle metrics: {last_error}"
        ) from last_error
    raise BusyListener(
        "listener stayed busy or metrics did not become stable before timeout"
    )


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="Prometheus metrics URL")
    parser.add_argument("--timeout", type=float, default=3600.0, help="seconds to wait")
    parser.add_argument(
        "--poll-interval",
        type=float,
        default=0.5,
        help="seconds between metrics samples",
    )
    parser.add_argument(
        "--stable-samples",
        type=int,
        default=2,
        help="consecutive idle samples required",
    )
    parser.add_argument(
        "--request-timeout", type=float, default=2.0, help="per-request HTTP timeout"
    )
    parser.add_argument(
        "--json", action="store_true", help="emit a machine-readable result"
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_args(argv)
    try:
        snapshot, elapsed, samples = wait_until_idle(
            arguments.url,
            timeout=arguments.timeout,
            poll_interval=arguments.poll_interval,
            stable_samples=arguments.stable_samples,
            request_timeout=arguments.request_timeout,
        )
    except (BusyListener, ValueError) as error:
        if arguments.json:
            print(json.dumps({"ok": False, "error": str(error)}))
        else:
            print(f"ccr_wait_idle: {error}", file=sys.stderr)
        return 75

    result = {
        "ok": True,
        "url": arguments.url,
        "elapsed_seconds": round(elapsed, 3),
        "samples": samples,
        **asdict(snapshot),
    }
    print(json.dumps(result) if arguments.json else f"idle after {elapsed:.3f}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
