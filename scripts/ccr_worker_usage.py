#!/usr/bin/env python3
"""Summarize CCR worker usage, listener counters, and Codex parent sessions."""

from __future__ import annotations

import argparse
import ipaddress
import json
import re
import socket
import sys
import urllib.parse
import urllib.request
from collections import Counter
from dataclasses import asdict, dataclass
from datetime import date, datetime, timedelta
from pathlib import Path

DEFAULT_METRICS_URL = "http://127.0.0.1:3457/metrics"
DEFAULT_RUNS_DIR = Path.home() / ".cache" / "ccr-worker-runs"
DEFAULT_SESSIONS_ROOT = Path.home() / ".codex" / "sessions"
PARENT_SESSION_ORIGINATOR = "codex-tui"
LEDGER_MARKERS = ("Workers:", "Parent-only:", "Terminal budget:")
SHELL_TOOL_NAMES = frozenset({"shell", "exec_command", "container.exec", "local_shell"})
WORKER_COMMAND = "codex-ccr-worker"
CREATE_THREAD_TOOL = "create_thread"
PROMETHEUS_NUMBER = r"[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?"
REQUEST_METRIC = re.compile(
    rf'^ccr_requests_total\{{tier="([^"]+)"\}}\s+({PROMETHEUS_NUMBER})'
)


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


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
    qoder_lane: dict[str, int]


@dataclass(frozen=True)
class ParentSessionRecord:
    session: str
    cwd: str
    turns: int
    tool_calls: int
    shell_calls: int
    codex_ccr_worker_launches: int
    create_thread_launches: int
    ledger_messages: int
    native_child: bool


@dataclass(frozen=True)
class ParentSessionUsage:
    days: int
    sessions: int
    root_parent_sessions: int
    native_child_sessions: int
    sessions_with_native_child_delegation: int
    sessions_with_no_worker_launch: int
    turns: int
    tool_calls: int
    shell_calls: int
    native_child_tool_calls: int
    codex_ccr_worker_launches: int
    create_thread_launches: int
    ledger_messages: int
    malformed_lines: int
    items: list[ParentSessionRecord]


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
    if (
        parsed.scheme != "http"
        or parsed.hostname is None
        or parsed.username is not None
        or parsed.password is not None
    ):
        raise ValueError("--url must be an HTTP loopback listener URL")
    try:
        port = parsed.port
        addresses = socket.getaddrinfo(parsed.hostname, port, type=socket.SOCK_STREAM)
    except (OSError, ValueError) as error:
        raise ValueError("--url must resolve to an HTTP loopback listener") from error
    try:
        loopback = all(
            ipaddress.ip_address(address[4][0]).is_loopback for address in addresses
        )
    except ValueError as error:
        raise ValueError("--url must resolve to an HTTP loopback listener") from error
    if not addresses or not loopback:
        raise ValueError("--url must resolve to an HTTP loopback listener")


def read_listener_usage(url: str, timeout: float) -> ListenerUsage:
    validate_local_http_url(url)
    request = urllib.request.Request(url, headers={"accept": "text/plain"})
    opener = urllib.request.build_opener(NoRedirectHandler)
    with opener.open(request, timeout=timeout) as response:
        if response.status != 200:
            raise ValueError(f"metrics endpoint returned HTTP {response.status}")
        return parse_listener_metrics(response.read().decode("utf-8"), url)


def run_date(run_dir: Path) -> date | None:
    match = re.match(r"(\d{8})-\d{6}-", run_dir.name)
    if match is None:
        return None
    try:
        return datetime.strptime(match.group(1), "%Y%m%d").date()
    except ValueError:
        return None


def qoder_requested(status: dict[str, object]) -> bool:
    if status.get("qoder_primary_requested") is True:
        return True
    route = status.get("requested_route")
    return isinstance(route, str) and (route == "qoder" or route.startswith("qoder,"))


def qoder_served(status: dict[str, object]) -> bool:
    if status.get("engine") == "qoder":
        return True
    route = status.get("served_route")
    return isinstance(route, str) and (route == "qoder" or route.startswith("qoder,"))


def summarize_qoder_lane(status: dict[str, object], qoder_lane: Counter[str]) -> None:
    attempt = status.get("qoder_primary_attempt")
    if attempt:
        qoder_lane["attempted"] += 1
        outcome = attempt.get("outcome") if isinstance(attempt, dict) else None
        if isinstance(outcome, str) and outcome:
            qoder_lane[f"outcome:{outcome}"] += 1
        if qoder_served(status):
            qoder_lane["served"] += 1
        else:
            qoder_lane["fell_back"] += 1
    elif status.get("qoder_bypass_reason"):
        qoder_lane[str(status["qoder_bypass_reason"])] += 1
    elif qoder_requested(status) and status.get("outcome") != "running":
        qoder_lane["eligible_not_attempted"] += 1
    else:
        qoder_lane["not_recorded"] += 1


def summarize_runs(runs_dir: Path, days: int) -> RunUsage:
    if days < 1:
        raise ValueError("--days must be positive")
    if not runs_dir.is_dir():
        raise ValueError("--runs-dir must be an existing directory")
    cutoff = datetime.now().date() - timedelta(days=days - 1)
    outcomes: Counter[str] = Counter()
    profiles: Counter[str] = Counter()
    routes: Counter[str] = Counter()
    qoder_lane: Counter[str] = Counter()
    read_only = 0
    network_allowed = 0
    elapsed_total = 0.0
    count = 0
    run_directories = sorted(path for path in runs_dir.iterdir() if path.is_dir())
    for run_dir in run_directories:
        if (run_day := run_date(run_dir)) is None or run_day < cutoff:
            continue
        status_path = run_dir / "status.json"
        if not status_path.exists():
            outcomes["receipt_pending"] += 1
            count += 1
            continue
        try:
            status = json.loads(status_path.read_text(encoding="utf-8"))
            if not isinstance(status, dict):
                raise TypeError("status JSON is not an object") from None
            elapsed = float(status.get("elapsed_seconds") or 0)
        except (OSError, TypeError, ValueError):
            outcomes["unreadable"] += 1
            count += 1
            continue
        outcomes[str(status.get("outcome") or "receipt_pending")] += 1
        profiles[str(status.get("timeout_profile") or "unspecified")] += 1
        routes[str(status.get("requested_route") or "unassigned")] += 1
        summarize_qoder_lane(status, qoder_lane)
        read_only += int(bool(status.get("read_only")))
        network_allowed += int(bool(status.get("network_allowed")))
        elapsed_total += elapsed
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
        qoder_lane=dict(sorted(qoder_lane.items())),
    )


def _recorded_date(value: object) -> date | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).date()
    except ValueError:
        return None


def _path_date(path: Path, root: Path) -> date | None:
    try:
        parts = path.relative_to(root).parts
    except ValueError:
        return None
    if len(parts) < 3:
        return None
    year, month, day = parts[:3]
    if (len(year), len(month), len(day)) != (4, 2, 2):
        return None
    try:
        return date(int(year), int(month), int(day))
    except ValueError:
        return None


def _command_text(payload: dict[str, object]) -> str:
    fragments: list[str] = []
    arguments = payload.get("arguments")
    parsed: object = arguments
    if isinstance(arguments, str):
        try:
            parsed = json.loads(arguments)
        except ValueError:
            fragments.append(arguments)
            parsed = None
    if isinstance(parsed, dict):
        for key in ("cmd", "command"):
            value = parsed.get(key)
            if isinstance(value, str):
                fragments.append(value)
            elif isinstance(value, list):
                fragments.append(" ".join(str(item) for item in value))
    for key in ("command", "input"):
        value = payload.get(key)
        if isinstance(value, str):
            fragments.append(value)
        elif isinstance(value, list):
            fragments.append(" ".join(str(item) for item in value))
    action = payload.get("action")
    if isinstance(action, dict):
        command = action.get("command")
        if isinstance(command, str):
            fragments.append(command)
        elif isinstance(command, list):
            fragments.append(" ".join(str(item) for item in command))
    return " ".join(fragments)


def _is_ledger_message(payload: dict[str, object]) -> bool:
    content = payload.get("content")
    fragments: list[str] = []
    if isinstance(content, str):
        fragments.append(content)
    elif isinstance(content, list):
        for part in content:
            if isinstance(part, dict):
                text = part.get("text")
                if isinstance(text, str):
                    fragments.append(text)
    text = "\n".join(fragments)
    return all(marker in text for marker in LEDGER_MARKERS)


def _summarize_session_file(
    path: Path, root: Path, cutoff: date
) -> tuple[ParentSessionRecord | None, int, bool]:
    originator = ""
    cwd = ""
    turn_trigger: str | None = None
    turn_trigger_seen = False
    turns = 0
    tool_calls = 0
    shell_calls = 0
    worker_launches = 0
    create_thread_launches = 0
    ledger_messages = 0
    malformed = 0
    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for raw_line in handle:
                line = raw_line.strip()
                if not line:
                    continue
                try:
                    record = json.loads(line)
                except ValueError:
                    malformed += 1
                    continue
                if not isinstance(record, dict):
                    malformed += 1
                    continue
                record_type = record.get("type")
                payload = record.get("payload")
                if not isinstance(payload, dict):
                    payload = {}
                if record_type == "session_meta":
                    if originator:
                        continue
                    value = payload.get("originator") or record.get("originator")
                    originator = value if isinstance(value, str) else ""
                    started = _recorded_date(payload.get("timestamp"))
                    if started is None:
                        started = _recorded_date(record.get("timestamp"))
                    if started is None:
                        started = _path_date(path, root)
                    if originator != PARENT_SESSION_ORIGINATOR:
                        return None, 0, False
                    if started is None or started < cutoff:
                        return None, 0, False
                    if not cwd:
                        value = payload.get("cwd")
                        if isinstance(value, str):
                            cwd = value
                    continue
                if record_type == "turn_context":
                    turns += 1
                    if not cwd:
                        value = payload.get("cwd")
                        if isinstance(value, str):
                            cwd = value
                    continue
                if record_type == "event_msg":
                    if not turn_trigger_seen and payload.get("type") == "task_started":
                        turn_trigger_seen = True
                        attribution = payload.get("turn_attribution")
                        value = (
                            attribution.get("turn_trigger")
                            if isinstance(attribution, dict)
                            else None
                        )
                        if isinstance(value, str) and value:
                            turn_trigger = value
                    continue
                if record_type != "response_item":
                    continue
                item_type = payload.get("type")
                if item_type == "message":
                    is_assistant = payload.get("role") == "assistant"
                    if is_assistant and _is_ledger_message(payload):
                        ledger_messages += 1
                    continue
                if item_type in ("function_call", "custom_tool_call"):
                    tool_calls += 1
                    name = payload.get("name")
                    name = name if isinstance(name, str) else ""
                    if name in SHELL_TOOL_NAMES:
                        shell_calls += 1
                        if WORKER_COMMAND in _command_text(payload):
                            worker_launches += 1
                    if name == CREATE_THREAD_TOOL:
                        create_thread_launches += 1
                    continue
                if item_type == "local_shell_call":
                    tool_calls += 1
                    shell_calls += 1
                    if WORKER_COMMAND in _command_text(payload):
                        worker_launches += 1
    except OSError:
        return None, 0, False
    if originator != PARENT_SESSION_ORIGINATOR:
        return None, 0, False
    try:
        session = str(path.relative_to(root))
    except ValueError:
        session = path.name
    return (
        ParentSessionRecord(
            session=session,
            cwd=cwd,
            turns=turns,
            tool_calls=tool_calls,
            shell_calls=shell_calls,
            codex_ccr_worker_launches=worker_launches,
            create_thread_launches=create_thread_launches,
            ledger_messages=ledger_messages,
            native_child=turn_trigger is None,
        ),
        malformed,
        turn_trigger == "user",
    )


def summarize_parent_sessions(sessions_root: Path, days: int) -> ParentSessionUsage:
    if days < 1:
        raise ValueError("--days must be positive")
    if not sessions_root.is_dir():
        raise ValueError("--sessions-root must be an existing directory")
    cutoff = date.today() - timedelta(days=days - 1)
    records: list[ParentSessionRecord] = []
    malformed = 0
    root_parents = 0
    root_parents_without_worker_launch = 0
    native_child_tool_calls = 0
    for path in sorted(sessions_root.rglob("*.jsonl")):
        record, malformed_lines, root_parent = _summarize_session_file(
            path, sessions_root, cutoff
        )
        malformed += malformed_lines
        if record is None:
            continue
        records.append(record)
        if root_parent:
            root_parents += 1
            if record.codex_ccr_worker_launches == 0:
                root_parents_without_worker_launch += 1
        if record.native_child:
            native_child_tool_calls += record.tool_calls
    return ParentSessionUsage(
        days=days,
        sessions=len(records),
        root_parent_sessions=root_parents,
        native_child_sessions=sum(record.native_child for record in records),
        sessions_with_native_child_delegation=sum(
            record.create_thread_launches > 0 for record in records
        ),
        sessions_with_no_worker_launch=root_parents_without_worker_launch,
        turns=sum(record.turns for record in records),
        tool_calls=sum(record.tool_calls for record in records),
        shell_calls=sum(record.shell_calls for record in records),
        native_child_tool_calls=native_child_tool_calls,
        codex_ccr_worker_launches=sum(
            record.codex_ccr_worker_launches for record in records
        ),
        create_thread_launches=sum(record.create_thread_launches for record in records),
        ledger_messages=sum(record.ledger_messages for record in records),
        malformed_lines=malformed,
        items=records,
    )


def print_usage(
    listener: ListenerUsage | None,
    runs: RunUsage | None,
    json_output: bool,
    parent_sessions: ParentSessionUsage | None = None,
) -> None:
    result: dict[str, object] = {}
    if listener is not None:
        result["listener"] = asdict(listener)
    if runs is not None:
        result["runs"] = asdict(runs)
    if parent_sessions is not None:
        result["parent_sessions"] = asdict(parent_sessions)
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
        print(f"  qoder_lane: {json.dumps(runs.qoder_lane, sort_keys=True)}")
        print(
            "  permissions: "
            f"read_only={runs.read_only}, network_allowed={runs.network_allowed}"
        )
        print(f"  elapsed_seconds_total: {runs.elapsed_seconds_total}")
    if parent_sessions is not None:
        print(
            f"parent_sessions: last {parent_sessions.days} day(s), "
            f"sessions={parent_sessions.sessions}, "
            f"root_parent_sessions={parent_sessions.root_parent_sessions}, "
            f"native_child_sessions={parent_sessions.native_child_sessions}, "
            "sessions_with_native_child_delegation="
            f"{parent_sessions.sessions_with_native_child_delegation}, "
            "sessions_with_no_worker_launch="
            f"{parent_sessions.sessions_with_no_worker_launch}"
        )
        print(
            "  totals: "
            f"turns={parent_sessions.turns}, "
            f"tool_calls={parent_sessions.tool_calls}, "
            f"shell_calls={parent_sessions.shell_calls}, "
            "native_child_tool_calls="
            f"{parent_sessions.native_child_tool_calls}, "
            "codex_ccr_worker_launches="
            f"{parent_sessions.codex_ccr_worker_launches}, "
            f"create_thread_launches={parent_sessions.create_thread_launches}, "
            f"ledger_messages={parent_sessions.ledger_messages}, "
            f"malformed_lines={parent_sessions.malformed_lines}"
        )
        for item in parent_sessions.items:
            print(
                f"  session {item.session}: cwd={item.cwd or '-'} "
                f"turns={item.turns} tool_calls={item.tool_calls} "
                f"shell_calls={item.shell_calls} "
                "codex_ccr_worker_launches="
                f"{item.codex_ccr_worker_launches} "
                f"create_thread_launches={item.create_thread_launches} "
                f"ledger_messages={item.ledger_messages} "
                f"native_child={item.native_child}"
            )


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=DEFAULT_METRICS_URL)
    parser.add_argument("--runs-dir", type=Path, default=DEFAULT_RUNS_DIR)
    parser.add_argument("--days", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("--runs-only", action="store_true")
    parser.add_argument("--listener-only", action="store_true")
    parser.add_argument("--parent-sessions", action="store_true")
    parser.add_argument("--sessions-root", type=Path, default=None)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.runs_only and args.listener_only:
        parser.error("--runs-only and --listener-only are mutually exclusive")
    if args.parent_sessions and (args.runs_only or args.listener_only):
        parser.error(
            "--parent-sessions is mutually exclusive with --runs-only "
            "and --listener-only"
        )
    if args.sessions_root is not None and not args.parent_sessions:
        parser.error("--sessions-root requires --parent-sessions")
    if args.days < 1:
        parser.error("--days must be positive")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    parent_sessions: ParentSessionUsage | None = None
    try:
        if args.parent_sessions:
            listener = None
            runs = None
            parent_sessions = summarize_parent_sessions(
                args.sessions_root or DEFAULT_SESSIONS_ROOT, args.days
            )
        else:
            listener = (
                read_listener_usage(args.url, args.timeout)
                if not args.runs_only
                else None
            )
            runs = (
                None if args.listener_only else summarize_runs(args.runs_dir, args.days)
            )
    except (OSError, ValueError) as error:
        print(f"ccr-worker-usage: {error}", file=sys.stderr)
        return 1
    print_usage(listener, runs, args.json, parent_sessions)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
