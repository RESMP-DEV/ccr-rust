#!/usr/bin/env python3
"""Sample the Z.AI coding-plan quota window and per-model usage.

The watcher reads the account API key from a credentials file (the
``CCR_ZAI_API_KEY`` field) or from the ``CCR_ZAI_API_KEY`` environment
variable, queries the read-only monitor endpoints under ``https://api.z.ai``,
and stores every poll as a JSON document. Per-interval deltas (per-model token
deltas, aggregate call deltas, plus window percentage and currentValue deltas)
make GLM tier utilization comparable over time instead of by eye.

Only the Python standard library is used. The API key is never printed, never
logged, and is redacted from every error message.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Mapping, Sequence
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

DEFAULT_KEY_FILE = "/Users/kearm/.claude-code-router/runtime-credentials.json"
DEFAULT_ORIGIN = "https://api.z.ai"
DEFAULT_OUT = "build/reports/zai-quota-watch/watch.jsonl"
DEFAULT_INTERVAL = 300.0
DEFAULT_TIMEOUT = 30.0
DEFAULT_WINDOW_HOURS = 24.0
KEY_FIELD = "CCR_ZAI_API_KEY"
REDACTED = "***REDACTED***"
QUOTA_PATH = "/api/monitor/usage/quota/limit"
MODEL_USAGE_PATH = "/api/monitor/usage/model-usage"
TIME_FORMAT = "%Y-%m-%d %H:%M:%S"
SNIPPET_LIMIT = 200


class UsageError(Exception):
    """Operator input problem: bad arguments, missing key, unwritable output."""


class ApiError(Exception):
    """A monitor endpoint could not be read or could not be understood."""


def redact(text: str, secret: str | None) -> str:
    """Return ``text`` with every occurrence of ``secret`` replaced."""
    if not secret:
        return text
    return text.replace(secret, REDACTED)


def to_float(value: Any) -> float | None:
    """Return ``value`` as a float, or None when it is absent or not numeric."""
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        text = value.strip().rstrip("%")
        if not text:
            return None
        try:
            return float(text)
        except ValueError:
            return None
    return None


def flatten_metrics(payload: Any, depth: int = 3) -> dict[str, float]:
    """Flatten numeric leaves of ``payload`` into dotted key paths."""
    metrics: dict[str, float] = {}
    if not isinstance(payload, dict) or depth <= 0:
        return metrics
    for key, value in payload.items():
        if isinstance(value, dict):
            for nested_key, nested_value in flatten_metrics(value, depth - 1).items():
                metrics[f"{key}.{nested_key}"] = nested_value
            continue
        number = to_float(value)
        if number is not None:
            metrics[str(key)] = number
    return metrics


def _sum_matching(
    metrics: Mapping[str, float],
    needles: Sequence[str],
    excluded: Sequence[str] = (),
) -> float | None:
    total = 0.0
    matched = False
    for key, value in metrics.items():
        lowered = key.lower()
        if any(term in lowered for term in excluded):
            continue
        if any(term in lowered for term in needles):
            matched = True
            total += value
    return total if matched else None


def token_total(metrics: Mapping[str, float]) -> float | None:
    """Sum the token counters found in ``metrics``, if any."""
    return _sum_matching(metrics, ("token",))


def call_total(metrics: Mapping[str, float]) -> float | None:
    """Sum the call counters found in ``metrics``, if any."""
    return _sum_matching(metrics, ("call", "request"), excluded=("token",))


def _series_last(values: Any) -> float | None:
    """Return the last numeric entry of a response series."""
    if not isinstance(values, list) or not values:
        return None
    return to_float(values[-1])


def _series_delta(values: Any) -> float | None:
    """Return the difference between the last two entries of a series."""
    if not isinstance(values, list) or len(values) < 2:
        return None
    current = to_float(values[-1])
    previous = to_float(values[-2])
    if current is None or previous is None:
        return None
    return current - previous


def _series_field(entry: Mapping[str, Any], names: Sequence[str]) -> list[Any]:
    """Return the first series stored in ``entry`` under one of ``names``."""
    for name in names:
        value = entry.get(name)
        if isinstance(value, list):
            return value
    return []


def _entry_call_count(entry: Mapping[str, Any]) -> float | None:
    """Return a per-model call count from a series or a numeric field."""
    last = _series_last(_series_field(entry, ("callCount", "modelCallCount")))
    if last is not None:
        return last
    for name in ("totalCallCount", "callCount", "modelCallCount", "totalCalls"):
        value = to_float(entry.get(name))
        if value is not None:
            return value
    return None


def _extract_key_field(data: Any) -> str | None:
    """Return the ``CCR_ZAI_API_KEY`` value from a decoded credentials file."""
    if not isinstance(data, dict):
        return None
    for container in (data, data.get("credentials")):
        if not isinstance(container, dict):
            continue
        value = container.get(KEY_FIELD)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def resolve_api_key(
    key_file: str | os.PathLike[str] | None = None,
    environ: Mapping[str, str] | None = None,
) -> str:
    """Resolve the API key from the credentials file, then the environment."""
    env = os.environ if environ is None else environ
    path = Path(key_file) if key_file is not None else Path(DEFAULT_KEY_FILE)
    if path.exists():
        try:
            raw = path.read_text(encoding="utf-8")
        except OSError as exc:
            raise UsageError(f"cannot read key file {path}: {exc}") from exc
        try:
            decoded = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise UsageError(f"key file {path} is not valid JSON: {exc}") from exc
        value = _extract_key_field(decoded)
        if value is not None:
            return value
    value = env.get(KEY_FIELD)
    if value and value.strip():
        return value.strip()
    raise UsageError(f"no {KEY_FIELD} found in {path} or in the environment")


def parse_quota_payload(payload: Any) -> dict[str, Any]:
    """Normalize a quota/limit payload into limits keyed by type and unit."""
    data = payload.get("data") if isinstance(payload, dict) else None
    data = data if isinstance(data, dict) else {}
    limits: list[dict[str, Any]] = []
    raw_limits = data.get("limits")
    if isinstance(raw_limits, list):
        for raw in raw_limits:
            if not isinstance(raw, dict):
                continue
            raw_type = raw.get("type")
            raw_unit = raw.get("unit")
            limit_type = "" if raw_type is None else str(raw_type)
            unit = "" if raw_unit is None else str(raw_unit)
            usage_details: dict[str, float | None] = {}
            raw_details = raw.get("usageDetails")
            if isinstance(raw_details, list):
                for detail in raw_details:
                    if not isinstance(detail, dict):
                        continue
                    model_code = detail.get("modelCode")
                    if model_code is None:
                        continue
                    usage_details[str(model_code)] = to_float(detail.get("usage"))
            reset_time = raw.get("nextResetTime")
            limits.append(
                {
                    "key": f"{limit_type}|{unit}",
                    "type": limit_type,
                    "unit": unit,
                    "number": to_float(raw.get("number")),
                    "usage": to_float(raw.get("usage")),
                    "current_value": to_float(raw.get("currentValue")),
                    "remaining": to_float(raw.get("remaining")),
                    "percentage": to_float(raw.get("percentage")),
                    "next_reset_time": (
                        str(reset_time) if reset_time is not None else None
                    ),
                    "usage_details": usage_details,
                }
            )
    level = data.get("level")
    return {
        "level": str(level) if level is not None else None,
        "limits": limits,
    }


def parse_model_usage_payload(payload: Any) -> dict[str, Any]:
    """Normalize a model-usage payload into per-model token and call counters.

    The live contract carries the per-model series in ``data.modelDataList``:
    each entry holds a ``modelCode``, a ``tokensUsage`` series aligned with
    ``data.x_time``, and the window ``totalTokens``. A payload that omits
    ``modelDataList`` is read from ``data.modelSummaryList`` instead, keyed by
    ``modelCode`` and falling back to ``modelName``. A per-model call count is
    taken from the entry when the entry carries one; the aggregate call count
    (``data.modelCallCount`` and ``data.totalUsage.totalModelCallCount``) is
    never borrowed for a model that does not report its own.
    """
    data = payload.get("data") if isinstance(payload, dict) else None
    data = data if isinstance(data, dict) else {}
    models: dict[str, dict[str, Any]] = {}

    raw_models = data.get("modelDataList")
    if isinstance(raw_models, list):
        for entry in raw_models:
            if not isinstance(entry, dict):
                continue
            model_code = entry.get("modelCode")
            if model_code is None:
                model_code = entry.get("modelName")
            if model_code is None:
                continue
            token_series = _series_field(entry, ("tokensUsage", "tokenUsage"))
            model_name = entry.get("modelName")
            models[str(model_code)] = {
                "model": str(model_code),
                "model_name": str(model_name) if model_name is not None else None,
                "window_total_tokens": to_float(entry.get("totalTokens")),
                "last_hour_tokens": _series_last(token_series),
                "delta_from_previous_hour": _series_delta(token_series),
                "last_hour_calls": _entry_call_count(entry),
            }

    if not models:
        # A summary entry carries no per-model series of its own, so it is the
        # fallback for a payload that omits modelDataList.
        for entry in _summary_entries(data):
            model_key = entry.get("modelCode")
            if model_key is None:
                model_key = entry.get("modelName")
            if model_key is None:
                continue
            model_name = entry.get("modelName")
            token_series = _series_field(entry, ("tokensUsage", "tokenUsage"))
            models[str(model_key)] = {
                "model": str(model_key),
                "model_name": str(model_name) if model_name is not None else None,
                "window_total_tokens": to_float(entry.get("totalTokens")),
                "last_hour_tokens": _series_last(token_series),
                "delta_from_previous_hour": _series_delta(token_series),
                "last_hour_calls": _entry_call_count(entry),
            }

    total_metrics = flatten_metrics(data.get("totalUsage"))
    x_time = data.get("x_time")
    granularity = data.get("granularity")
    return {
        "granularity": str(granularity) if granularity is not None else None,
        "x_time": x_time if isinstance(x_time, list) else [],
        "total_usage": {
            "tokens": token_total(total_metrics),
            "calls": call_total(total_metrics),
            "last_hour_tokens": _series_last(data.get("tokensUsage")),
            "last_hour_calls": _series_last(data.get("modelCallCount")),
            "metrics": total_metrics,
        },
        "models": models,
    }


def _summary_entries(data: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Return the model summary entries a payload carries, if any."""
    candidates: list[Any] = [data.get("modelSummaryList")]
    total = data.get("totalUsage")
    if isinstance(total, dict):
        candidates.append(total.get("modelSummaryList"))
    for candidate in candidates:
        if isinstance(candidate, list):
            return [entry for entry in candidate if isinstance(entry, dict)]
    return []


class ZaiMonitor:
    """Read-only client for the Z.AI usage monitor endpoints."""

    def __init__(
        self, origin: str, api_key: str, timeout: float = DEFAULT_TIMEOUT
    ) -> None:
        self.origin = origin.rstrip("/")
        self.api_key = api_key
        self.timeout = timeout

    def _url(self, path: str, query: str = "") -> str:
        url = f"{self.origin}{path}"
        return f"{url}?{query}" if query else url

    def _http_error(self, url: str, error: urllib.error.HTTPError) -> str:
        snippet = ""
        try:
            snippet = error.read().decode("utf-8", "replace")[:SNIPPET_LIMIT]
        except OSError:
            snippet = ""
        detail = f": {snippet}" if snippet else ""
        return redact(f"HTTP {error.code} from {url}{detail}", self.api_key)

    def _get(self, path: str, query: str = "") -> tuple[int, Any]:
        url = self._url(path, query)
        request = urllib.request.Request(  # noqa: S310 - operator supplied origin
            url,
            headers={"authorization": self.api_key, "accept": "application/json"},
        )
        try:
            # The origin is operator supplied; the endpoint path is a constant.
            with urllib.request.urlopen(request, timeout=self.timeout) as response:  # noqa: S310
                status = response.status
                body = response.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as error:
            raise ApiError(self._http_error(url, error)) from error
        except urllib.error.URLError as error:
            message = redact(f"request to {url} failed: {error.reason}", self.api_key)
            raise ApiError(message) from error
        except OSError as error:
            message = redact(f"request to {url} failed: {error}", self.api_key)
            raise ApiError(message) from error
        if status != 200:
            raise ApiError(redact(f"HTTP {status} from {url}", self.api_key))
        try:
            payload = json.loads(body)
        except json.JSONDecodeError as error:
            message = redact(f"invalid JSON from {url}: {error}", self.api_key)
            raise ApiError(message) from error
        return status, payload

    def fetch_quota_limits(self) -> tuple[int, Any]:
        """Return the HTTP status and decoded body of the quota/limit endpoint."""
        return self._get(QUOTA_PATH)

    def fetch_model_usage(self, start: datetime, end: datetime) -> tuple[int, Any]:
        """Return the HTTP status and decoded body of the model-usage endpoint."""
        query = urllib.parse.urlencode(
            {
                "startTime": start.strftime(TIME_FORMAT),
                "endTime": end.strftime(TIME_FORMAT),
            },
            quote_via=urllib.parse.quote,
        )
        return self._get(MODEL_USAGE_PATH, query)


def empty_state() -> dict[str, Any]:
    """Return the empty baseline used for the first poll of a run."""
    return {
        "quota": {"captured_at": None, "limits": {}},
        "models": {"captured_at": None, "models": {}, "aggregate": {}},
    }


def _delta(previous: float | None, current: float | None) -> float | None:
    """Return ``current - previous``, or None when either side is unknown."""
    if previous is None or current is None:
        return None
    return current - previous


def _interval_seconds(previous: str | None, current: str) -> float | None:
    """Return the seconds between two ISO8601 timestamps, or None."""
    if not previous:
        return None
    try:
        previous_at = datetime.fromisoformat(previous)
        current_at = datetime.fromisoformat(current)
    except ValueError:
        return None
    return (current_at - previous_at).total_seconds()


def compute_deltas(
    previous_state: Mapping[str, Any],
    captured_at: str,
    quota_limits: Mapping[str, Mapping[str, Any]],
    models: Mapping[str, Mapping[str, Any]],
    aggregate: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Compute per-interval deltas against the previous successful sample."""
    previous_quota = previous_state.get("quota") or {}
    previous_models = previous_state.get("models") or {}
    previous_limits = previous_quota.get("limits") or {}
    previous_model_rows = previous_models.get("models") or {}
    previous_aggregate = previous_models.get("aggregate") or {}

    limit_deltas: dict[str, Any] = {}
    for key, current in quota_limits.items():
        previous = previous_limits.get(key) or {}
        previous_details = previous.get("usage_details") or {}
        current_details = current.get("usage_details") or {}
        detail_deltas: dict[str, float | None] = {}
        for model_code, current_usage in current_details.items():
            detail_deltas[model_code] = _delta(
                previous_details.get(model_code), current_usage
            )
        limit_deltas[key] = {
            "percentage_delta": _delta(
                previous.get("percentage"), current.get("percentage")
            ),
            "current_value_delta": _delta(
                previous.get("current_value"), current.get("current_value")
            ),
            "usage_detail_deltas": detail_deltas,
        }

    model_deltas: dict[str, Any] = {}
    for model_code, current in models.items():
        previous = previous_model_rows.get(model_code) or {}
        model_deltas[model_code] = {
            "token_delta": _delta(
                previous.get("window_total_tokens"),
                current.get("window_total_tokens"),
            ),
            "last_hour_token_delta": _delta(
                previous.get("last_hour_tokens"),
                current.get("last_hour_tokens"),
            ),
            "call_delta": _delta(
                previous.get("last_hour_calls"),
                current.get("last_hour_calls"),
            ),
        }

    current_aggregate = aggregate or {}
    aggregate_deltas = {
        "token_delta": _delta(
            previous_aggregate.get("total_tokens"),
            current_aggregate.get("total_tokens"),
        ),
        "call_delta": _delta(
            previous_aggregate.get("total_calls"),
            current_aggregate.get("total_calls"),
        ),
        "last_hour_token_delta": _delta(
            previous_aggregate.get("last_hour_tokens"),
            current_aggregate.get("last_hour_tokens"),
        ),
        "last_hour_call_delta": _delta(
            previous_aggregate.get("last_hour_calls"),
            current_aggregate.get("last_hour_calls"),
        ),
    }

    return {
        "quota": {
            "previous_captured_at": previous_quota.get("captured_at"),
            "interval_seconds": _interval_seconds(
                previous_quota.get("captured_at"), captured_at
            ),
            "limits": limit_deltas,
        },
        "models": {
            "previous_captured_at": previous_models.get("captured_at"),
            "interval_seconds": _interval_seconds(
                previous_models.get("captured_at"), captured_at
            ),
            "per_model": model_deltas,
            "aggregate": aggregate_deltas,
        },
    }


def capture_snapshot(
    monitor: ZaiMonitor,
    *,
    window_start: datetime,
    window_end: datetime,
    previous_state: Mapping[str, Any] | None = None,
    mode: str = "once",
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Poll both endpoints once and return the record and the next baseline."""
    captured_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
    baseline = previous_state or empty_state()
    errors: list[dict[str, str]] = []

    quota: dict[str, Any] = {
        "ok": False,
        "status": None,
        "error": None,
        "level": None,
        "limits": [],
    }
    quota_limits: dict[str, dict[str, Any]] = {}
    try:
        status, payload = monitor.fetch_quota_limits()
        parsed = parse_quota_payload(payload)
    except (ApiError, OSError, ValueError, TypeError) as error:
        message = redact(str(error), monitor.api_key)
        quota["error"] = message
        errors.append({"scope": "quota", "message": message})
    else:
        quota.update(
            {
                "ok": True,
                "status": status,
                "level": parsed["level"],
                "limits": parsed["limits"],
            }
        )
        quota_limits = {
            limit["key"]: {
                "percentage": limit["percentage"],
                "current_value": limit["current_value"],
                "usage_details": limit["usage_details"],
            }
            for limit in parsed["limits"]
        }

    model_usage: dict[str, Any] = {
        "ok": False,
        "status": None,
        "error": None,
        "window": {
            "start_time": window_start.strftime(TIME_FORMAT),
            "end_time": window_end.strftime(TIME_FORMAT),
        },
        "granularity": None,
        "x_time": [],
        "total_usage": {
            "tokens": None,
            "calls": None,
            "last_hour_tokens": None,
            "last_hour_calls": None,
            "metrics": {},
        },
        "models": {},
    }
    models: dict[str, dict[str, Any]] = {}
    aggregate: dict[str, Any] = {}
    try:
        status, payload = monitor.fetch_model_usage(window_start, window_end)
        parsed = parse_model_usage_payload(payload)
    except (ApiError, OSError, ValueError, TypeError) as error:
        message = redact(str(error), monitor.api_key)
        model_usage["error"] = message
        errors.append({"scope": "model_usage", "message": message})
    else:
        model_usage.update(
            {
                "ok": True,
                "status": status,
                "granularity": parsed["granularity"],
                "x_time": parsed["x_time"],
                "total_usage": parsed["total_usage"],
                "models": parsed["models"],
            }
        )
        models = {
            model_code: {
                "window_total_tokens": entry["window_total_tokens"],
                "last_hour_tokens": entry["last_hour_tokens"],
                "last_hour_calls": entry["last_hour_calls"],
            }
            for model_code, entry in parsed["models"].items()
        }
        aggregate = {
            "total_tokens": parsed["total_usage"]["tokens"],
            "total_calls": parsed["total_usage"]["calls"],
            "last_hour_tokens": parsed["total_usage"]["last_hour_tokens"],
            "last_hour_calls": parsed["total_usage"]["last_hour_calls"],
        }

    deltas = compute_deltas(baseline, captured_at, quota_limits, models, aggregate)
    baseline_quota = baseline.get("quota") or {}
    baseline_models = baseline.get("models") or {}
    next_state = {
        "quota": {
            "captured_at": (
                captured_at if quota["ok"] else baseline_quota.get("captured_at")
            ),
            "limits": (
                quota_limits if quota["ok"] else baseline_quota.get("limits") or {}
            ),
        },
        "models": {
            "captured_at": (
                captured_at if model_usage["ok"] else baseline_models.get("captured_at")
            ),
            "models": (
                models if model_usage["ok"] else baseline_models.get("models") or {}
            ),
            "aggregate": (
                aggregate
                if model_usage["ok"]
                else baseline_models.get("aggregate") or {}
            ),
        },
    }
    record = {
        "captured_at": captured_at,
        "mode": mode,
        "quota": quota,
        "model_usage": model_usage,
        "deltas": deltas,
        "errors": errors,
    }
    return record, next_state


def poll_once(
    monitor: ZaiMonitor,
    *,
    window_start: datetime,
    window_end: datetime,
    previous_state: Mapping[str, Any] | None = None,
    mode: str = "once",
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Poll once and never raise, so a watch loop survives a bad poll."""
    try:
        return capture_snapshot(
            monitor,
            window_start=window_start,
            window_end=window_end,
            previous_state=previous_state,
            mode=mode,
        )
    except Exception as error:  # noqa: BLE001 - a bad poll must not kill a run
        message = redact(f"unexpected poll failure: {error}", monitor.api_key)
        record = failed_record(message, window_start, window_end, mode)
        return record, dict(previous_state or empty_state())


def failed_record(
    message: str,
    window_start: datetime,
    window_end: datetime,
    mode: str,
) -> dict[str, Any]:
    """Return a record describing a poll that could not produce data."""
    captured_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
    return {
        "captured_at": captured_at,
        "mode": mode,
        "quota": {
            "ok": False,
            "status": None,
            "error": message,
            "level": None,
            "limits": [],
        },
        "model_usage": {
            "ok": False,
            "status": None,
            "error": message,
            "window": {
                "start_time": window_start.strftime(TIME_FORMAT),
                "end_time": window_end.strftime(TIME_FORMAT),
            },
            "granularity": None,
            "x_time": [],
            "total_usage": {
                "tokens": None,
                "calls": None,
                "last_hour_tokens": None,
                "last_hour_calls": None,
                "metrics": {},
            },
            "models": {},
        },
        "deltas": compute_deltas(empty_state(), captured_at, {}, {}),
        "errors": [{"scope": "poll", "message": message}],
    }


HELP_DOES = """\
What this script does:
  * Reads the Z.AI account API key from a credentials file (the CCR_ZAI_API_KEY
    field, at the top level or under "credentials") or from the CCR_ZAI_API_KEY
    environment variable. The key is never printed, never logged, and is
    redacted from every error message.
  * Queries the read-only monitor endpoints /api/monitor/usage/quota/limit and
    /api/monitor/usage/model-usage and records what they return.
  * Records per-interval deltas: per-model token deltas, aggregate call
    deltas, plus the quota window percentage and currentValue deltas, against
    the previous poll of the same run.
  * Reads the per-model series from "modelDataList" and keys the records by
    "modelCode": "window_total_tokens" is the model's window total,
    "last_hour_tokens" is its last point of the hourly series, and
    "delta_from_previous_hour" is the change from the point before it. A
    payload without "modelDataList" is read from "modelSummaryList" instead,
    keyed by "modelCode" and falling back to "modelName".
  * A per-model "last_hour_calls" is read from the model entry when the entry
    reports one. Otherwise it stays null rather than borrowing the aggregate
    call count, which is recorded once at the top level.
"""


HELP_DOES_NOT = """\
What this script does NOT prove:
  * It does not prove the reported usage is correct, complete, or attributed
    to the account you expect: the endpoints return whatever the supplied key
    can see, and a wrong key silently reports the wrong account.
  * It does not prove that CCR served a given GLM tier, nor anything about
    routing, latency, model quality, or provider-side enforcement.
  * It does not prove the quota window resets when the API says it does.
  * Deltas are in-memory and per-process: they describe the interval between
    two polls of this run only, and are not a durable baseline across
    restarts.
  * A missing field is reported as null, not as zero: null means "the API did
    not say", not "nothing was used".
"""


def build_parser() -> argparse.ArgumentParser:
    """Build the command-line interface, including its non-claims."""
    parser = argparse.ArgumentParser(
        prog="zai_quota_watch.py",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=(
            "Poll the read-only Z.AI monitor endpoints for the coding-plan "
            "quota window and per-model usage, and record per-interval deltas "
            "so GLM tier utilization can be compared over time."
        ),
        epilog=HELP_DOES + "\n" + HELP_DOES_NOT,
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--once",
        action="store_true",
        help="print one JSON snapshot to stdout and exit",
    )
    mode.add_argument(
        "--watch",
        action="store_true",
        help="append one JSON record per poll to --out until interrupted",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=DEFAULT_INTERVAL,
        help="seconds between --watch polls (default: %(default)s)",
    )
    parser.add_argument(
        "--out",
        default=DEFAULT_OUT,
        help="JSONL file --watch appends to (default: %(default)s)",
    )
    parser.add_argument(
        "--key-file",
        default=DEFAULT_KEY_FILE,
        help="JSON credentials file holding CCR_ZAI_API_KEY (default: %(default)s)",
    )
    parser.add_argument(
        "--origin",
        default=DEFAULT_ORIGIN,
        help="API origin to read (default: %(default)s)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help="per-request timeout in seconds (default: %(default)s)",
    )
    parser.add_argument(
        "--window-hours",
        type=float,
        default=DEFAULT_WINDOW_HOURS,
        help="model-usage window length in hours (default: %(default)s)",
    )
    return parser


def watch_loop(monitor: ZaiMonitor, args: argparse.Namespace) -> int:
    """Poll until interrupted, appending one JSON record per poll."""
    out_path = Path(args.out)
    try:
        out_path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        print(f"error: cannot create {out_path.parent}: {error}", file=sys.stderr)
        return 2
    print(
        f"watching {monitor.origin} every {args.interval}s into {out_path}",
        file=sys.stderr,
    )
    state = empty_state()
    try:
        while True:
            window_end = datetime.now(timezone.utc)
            window_start = window_end - timedelta(hours=args.window_hours)
            record, state = poll_once(
                monitor,
                window_start=window_start,
                window_end=window_end,
                previous_state=state,
                mode="watch",
            )
            try:
                with out_path.open("a", encoding="utf-8") as handle:
                    handle.write(json.dumps(record, sort_keys=True) + "\n")
            except OSError as error:
                print(f"error: cannot write {out_path}: {error}", file=sys.stderr)
                return 2
            if record["errors"]:
                first = record["errors"][0]
                print(
                    f"poll error ({first['scope']}): {first['message']}",
                    file=sys.stderr,
                )
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 0


def main(argv: Sequence[str] | None = None) -> int:
    """Return 0 on success, 2 on a usage error, 3 on an API error."""
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.interval <= 0:
        print("error: --interval must be positive", file=sys.stderr)
        return 2
    if args.window_hours <= 0:
        print("error: --window-hours must be positive", file=sys.stderr)
        return 2
    try:
        api_key = resolve_api_key(args.key_file)
    except UsageError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    monitor = ZaiMonitor(args.origin, api_key, args.timeout)
    if args.once:
        window_end = datetime.now(timezone.utc)
        window_start = window_end - timedelta(hours=args.window_hours)
        record, _ = poll_once(
            monitor,
            window_start=window_start,
            window_end=window_end,
            previous_state=None,
            mode="once",
        )
        print(json.dumps(record, sort_keys=True))
        return 3 if record["errors"] else 0
    return watch_loop(monitor, args)


if __name__ == "__main__":
    sys.exit(main())
