#!/usr/bin/env python3
"""Stress test: 100 concurrent SSE streams through ccr-rust.

Launches N concurrent streaming requests against the ccr-rust proxy and
collects per-stream timing metrics: time-to-first-byte (TTFB), total
duration, chunks received, bytes received, and error classification.

Requires:
  1. Mock SSE backend running (benchmarks/mock_sse_backend.py)
  2. ccr-rust running with config pointing at the mock backend

Usage:
    uv run python benchmarks/stress_sse_streams.py [OPTIONS]

Options:
    --ccr-url       CCR proxy URL (default: http://127.0.0.1:3456)
    --streams       Number of concurrent streams (default: 100)
    --ramp-ms       Ramp-up period: spread stream launches over this window (default: 0)
    --timeout       Per-stream timeout in seconds (default: 120)
    --model         Model name to send in requests (default: mock,mock-model)
    --max-tokens    max_tokens field in request (default: 4096)
    --json-out      Write results to JSON file
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
from dataclasses import asdict, dataclass, field
from typing import Any


@dataclass
class StreamResult:
    stream_id: int
    status_code: int = 0
    ttfb_ms: float = 0.0
    duration_ms: float = 0.0
    chunks_received: int = 0
    bytes_received: int = 0
    error: str = ""
    first_chunk_ts: float = 0.0
    last_chunk_ts: float = 0.0


@dataclass
class StressReport:
    total_streams: int = 0
    successful: int = 0
    failed: int = 0
    errors: dict[str, int] = field(default_factory=dict)
    ttfb_p50_ms: float = 0.0
    ttfb_p95_ms: float = 0.0
    ttfb_p99_ms: float = 0.0
    ttfb_max_ms: float = 0.0
    duration_p50_ms: float = 0.0
    duration_p95_ms: float = 0.0
    duration_p99_ms: float = 0.0
    duration_max_ms: float = 0.0
    total_bytes: int = 0
    total_chunks: int = 0
    throughput_mbps: float = 0.0
    wall_clock_s: float = 0.0
    peak_concurrent: int = 0
    ccr_active_streams_peak: float = 0.0
    ccr_usage: dict[str, Any] | None = None


def percentile(data: list[float], pct: float) -> float:
    """Compute the pct-th percentile of a sorted list."""
    if not data:
        return 0.0
    k = (len(data) - 1) * (pct / 100.0)
    f = int(k)
    c = f + 1
    if c >= len(data):
        return data[f]
    return data[f] + (k - f) * (data[c] - data[f])


def build_request_body(model: str, max_tokens: int, stream_id: int) -> dict:
    """Build an Anthropic-compatible request body."""
    return {
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": f"Stress test stream {stream_id}. Respond with a long passage.",
            }
        ],
        "max_tokens": max_tokens,
        "stream": True,
    }


async def consume_sse_stream(
    session_module: object,
    ccr_url: str,
    model: str,
    max_tokens: int,
    stream_id: int,
    timeout: float,
    concurrent_gauge: list[int],
) -> StreamResult:
    """Open a single SSE stream and consume it, recording metrics."""
    import aiohttp

    result = StreamResult(stream_id=stream_id)
    url = f"{ccr_url}/v1/messages"
    body = build_request_body(model, max_tokens, stream_id)

    t0 = time.monotonic()
    concurrent_gauge[0] += 1

    try:
        client_timeout = aiohttp.ClientTimeout(total=timeout)
        async with aiohttp.ClientSession(timeout=client_timeout) as session:
            async with session.post(
                url,
                json=body,
                headers={"Content-Type": "application/json"},
            ) as resp:
                result.status_code = resp.status

                if resp.status != 200:
                    text = await resp.text()
                    result.error = f"HTTP {resp.status}: {text[:200]}"
                    return result

                first_byte = True
                async for chunk in resp.content.iter_any():
                    now = time.monotonic()
                    if first_byte:
                        result.ttfb_ms = (now - t0) * 1000.0
                        result.first_chunk_ts = now
                        first_byte = False

                    result.chunks_received += 1
                    result.bytes_received += len(chunk)
                    result.last_chunk_ts = now

    except TimeoutError:
        result.error = "timeout"
    except asyncio.CancelledError:
        result.error = "cancelled"
    except Exception as e:
        result.error = f"{type(e).__name__}: {e}"
    finally:
        result.duration_ms = (time.monotonic() - t0) * 1000.0
        concurrent_gauge[0] -= 1

    return result


async def poll_active_streams(ccr_url: str, peak: list[float], stop_event: asyncio.Event) -> None:
    """Periodically poll ccr-rust /v1/usage to track peak active_streams gauge."""
    import aiohttp

    url = f"{ccr_url}/v1/usage"
    while not stop_event.is_set():
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(url, timeout=aiohttp.ClientTimeout(total=2)) as resp:
                    if resp.status == 200:
                        data = await resp.json()
                        current = data.get("active_streams", 0)
                        if current > peak[0]:
                            peak[0] = current
        except Exception:
            pass
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=0.25)
        except TimeoutError:
            pass


async def fetch_final_usage(ccr_url: str) -> dict | None:
    """Fetch final usage stats from ccr-rust after the test."""
    import aiohttp

    try:
        async with aiohttp.ClientSession() as session:
            async with session.get(
                f"{ccr_url}/v1/usage",
                timeout=aiohttp.ClientTimeout(total=5),
            ) as resp:
                if resp.status == 200:
                    return await resp.json()
    except Exception:
        pass
    return None


async def run_stress_test(
    ccr_url: str,
    streams: int,
    ramp_ms: int,
    timeout: float,
    model: str,
    max_tokens: int,
) -> tuple[list[StreamResult], StressReport]:
    """Launch all streams concurrently and collect results."""

    concurrent_gauge: list[int] = [0]
    peak_concurrent: list[int] = [0]
    peak_active_streams: list[float] = [0.0]

    stop_poll = asyncio.Event()
    poll_task = asyncio.create_task(poll_active_streams(ccr_url, peak_active_streams, stop_poll))

    ramp_delay = ramp_ms / 1000.0 / max(streams, 1)

    async def launch_stream(sid: int) -> StreamResult:
        if ramp_delay > 0 and sid > 0:
            await asyncio.sleep(ramp_delay * sid)

        # Track peak concurrency
        r = await consume_sse_stream(
            None, ccr_url, model, max_tokens, sid, timeout, concurrent_gauge
        )
        current = concurrent_gauge[0]
        if current > peak_concurrent[0]:
            peak_concurrent[0] = current
        return r

    wall_t0 = time.monotonic()
    tasks = [asyncio.create_task(launch_stream(i)) for i in range(streams)]
    results = await asyncio.gather(*tasks, return_exceptions=True)
    wall_elapsed = time.monotonic() - wall_t0

    # Stop the poller
    stop_poll.set()
    await poll_task

    # Convert exceptions to error results
    processed: list[StreamResult] = []
    for i, r in enumerate(results):
        if isinstance(r, Exception):
            processed.append(StreamResult(stream_id=i, error=f"{type(r).__name__}: {r}"))
        else:
            processed.append(r)

    # Build report
    report = StressReport(total_streams=streams, wall_clock_s=wall_elapsed)

    successes = [r for r in processed if not r.error]
    failures = [r for r in processed if r.error]
    report.successful = len(successes)
    report.failed = len(failures)

    # Error classification
    for f in failures:
        key = f.error.split(":")[0].strip() if ":" in f.error else f.error
        report.errors[key] = report.errors.get(key, 0) + 1

    if successes:
        ttfbs = sorted(r.ttfb_ms for r in successes)
        durations = sorted(r.duration_ms for r in successes)

        report.ttfb_p50_ms = percentile(ttfbs, 50)
        report.ttfb_p95_ms = percentile(ttfbs, 95)
        report.ttfb_p99_ms = percentile(ttfbs, 99)
        report.ttfb_max_ms = ttfbs[-1]

        report.duration_p50_ms = percentile(durations, 50)
        report.duration_p95_ms = percentile(durations, 95)
        report.duration_p99_ms = percentile(durations, 99)
        report.duration_max_ms = durations[-1]

        report.total_bytes = sum(r.bytes_received for r in successes)
        report.total_chunks = sum(r.chunks_received for r in successes)

        if wall_elapsed > 0:
            report.throughput_mbps = (report.total_bytes * 8) / (wall_elapsed * 1_000_000)

    report.peak_concurrent = peak_concurrent[0]
    report.ccr_active_streams_peak = peak_active_streams[0]

    # Fetch final CCR usage
    report.ccr_usage = await fetch_final_usage(ccr_url)

    return processed, report


def print_report(report: StressReport) -> None:
    """Print a human-readable summary."""
    print("\n" + "=" * 72)
    print("  CCR-RUST SSE STRESS TEST REPORT")
    print("=" * 72)

    print(f"\n  Streams:  {report.total_streams}")
    print(f"  Success:  {report.successful}  |  Failed: {report.failed}")
    print(f"  Wall time: {report.wall_clock_s:.2f}s")
    print(f"  Peak local concurrency: {report.peak_concurrent}")
    print(f"  Peak ccr active_streams: {report.ccr_active_streams_peak:.0f}")

    if report.errors:
        print("\n  Errors:")
        for err_type, count in sorted(report.errors.items(), key=lambda x: -x[1]):
            print(f"    {err_type}: {count}")

    if report.successful > 0:
        print("\n  Time-to-First-Byte (TTFB):")
        print(f"    p50:  {report.ttfb_p50_ms:>10.1f} ms")
        print(f"    p95:  {report.ttfb_p95_ms:>10.1f} ms")
        print(f"    p99:  {report.ttfb_p99_ms:>10.1f} ms")
        print(f"    max:  {report.ttfb_max_ms:>10.1f} ms")

        print("\n  Stream Duration:")
        print(f"    p50:  {report.duration_p50_ms:>10.1f} ms")
        print(f"    p95:  {report.duration_p95_ms:>10.1f} ms")
        print(f"    p99:  {report.duration_p99_ms:>10.1f} ms")
        print(f"    max:  {report.duration_max_ms:>10.1f} ms")

        print("\n  Throughput:")
        print(f"    Total bytes:  {report.total_bytes:>12,}")
        print(f"    Total chunks: {report.total_chunks:>12,}")
        print(f"    Aggregate:    {report.throughput_mbps:>10.2f} Mbps")

    if report.ccr_usage:
        usage = report.ccr_usage
        print("\n  CCR Usage (post-test):")
        print(f"    total_requests: {usage.get('total_requests', 'N/A')}")
        print(f"    total_failures: {usage.get('total_failures', 'N/A')}")
        print(f"    active_streams: {usage.get('active_streams', 'N/A')}")

    print("\n" + "=" * 72)

    # Pass/fail verdict
    if report.failed == 0:
        print("  RESULT: PASS - all streams completed successfully")
    elif report.failed <= report.total_streams * 0.05:
        print(f"  RESULT: WARN - {report.failed}/{report.total_streams} streams failed (<5%)")
    else:
        print(f"  RESULT: FAIL - {report.failed}/{report.total_streams} streams failed")
    print("=" * 72 + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description="Stress test: concurrent SSE streams through ccr-rust")
    parser.add_argument("--ccr-url", default="http://127.0.0.1:3456", help="CCR proxy base URL")
    parser.add_argument("--streams", type=int, default=100, help="Number of concurrent streams")
    parser.add_argument("--ramp-ms", type=int, default=0, help="Ramp-up window in ms (0 = all at once)")
    parser.add_argument("--timeout", type=float, default=120.0, help="Per-stream timeout in seconds")
    parser.add_argument("--model", default="mock,mock-model", help="Model string for request")
    parser.add_argument("--max-tokens", type=int, default=4096, help="max_tokens in request body")
    parser.add_argument("--json-out", default="", help="Write JSON results to file")
    args = parser.parse_args()

    print(f"Starting stress test: {args.streams} concurrent SSE streams")
    print(f"  Target: {args.ccr_url}")
    print(f"  Model:  {args.model}")
    print(f"  Ramp:   {args.ramp_ms}ms | Timeout: {args.timeout}s")

    results, report = asyncio.run(run_stress_test(
        ccr_url=args.ccr_url,
        streams=args.streams,
        ramp_ms=args.ramp_ms,
        timeout=args.timeout,
        model=args.model,
        max_tokens=args.max_tokens,
    ))

    print_report(report)

    if args.json_out:
        output = {
            "report": asdict(report),
            "streams": [asdict(r) for r in results],
        }
        with open(args.json_out, "w") as f:
            json.dump(output, f, indent=2)
        print(f"JSON results written to {args.json_out}")

    # Exit code: 0 if all passed, 1 if >5% failed, 2 if >50% failed
    if report.failed > report.total_streams * 0.5:
        sys.exit(2)
    elif report.failed > report.total_streams * 0.05:
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
