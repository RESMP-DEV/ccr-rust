#!/usr/bin/env python3
"""Fixed task suite and checkers for the CCR per-model benchmark.

The suite is deterministic and offline. Every scenario has exactly one
objective expected outcome, so a score answers "did this response satisfy the
contract" instead of "did this response look good". Five scenario kinds are
covered:

- ``exact_text``   instruction following with byte-level output expectations
- ``strict_json``  whole-output JSON conformance, no fences or trailing prose
- ``code_fix``     a deterministic Python bug fix, checked by executing the
                   returned snippet in a local subprocess
- ``arithmetic``   a multi-step word problem with a numeric answer
- ``vision``       a two-color image question, skipped for text-only routes

Only the code-fix checker runs anything: it executes model-generated Python in
a subprocess with a timeout. That is not a security sandbox, and the runner
can be told to skip the scenario entirely.
"""

from __future__ import annotations

import base64
import json
import math
import os
import re
import struct
import subprocess
import sys
import tempfile
import zlib
from dataclasses import dataclass, field
from typing import Any

SUITE_ID = "ccr-model-benchmark"
SUITE_VERSION = "v1"

SNIPPET_EXECUTION_TIMEOUT_SECONDS = 20.0

# The runner executed in the child process. It is a plain string so the
# candidate code never touches this module's namespace.
SNIPPET_RUNNER_SOURCE = """
import json
import sys

payload = json.loads(sys.stdin.read())
namespace = {"__name__": "candidate"}
try:
    exec(compile(payload["code"], "<candidate>", "exec"), namespace)
except BaseException as error:
    print(json.dumps({"status": "exec_error",
                      "detail": "%s: %s" % (type(error).__name__, error)}))
    raise SystemExit(0)
if payload["function_name"] not in namespace:
    print(json.dumps({"status": "missing_function",
                      "detail": "function %s was not defined" % payload["function_name"]}))
    raise SystemExit(0)
try:
    exec(compile(payload["tests"], "<checks>", "exec"), namespace)
except AssertionError as error:
    print(json.dumps({"status": "assertion_failed",
                      "detail": str(error) or "an assertion failed"}))
    raise SystemExit(0)
except BaseException as error:
    print(json.dumps({"status": "check_error",
                      "detail": "%s: %s" % (type(error).__name__, error)}))
    raise SystemExit(0)
print(json.dumps({"status": "passed", "detail": "all checks held"}))
"""


@dataclass(frozen=True)
class CheckOutcome:
    """Result of applying one checker to one model response."""

    passed: bool
    detail: str


@dataclass(frozen=True)
class Scenario:
    """One fixed benchmark task.

    The optional expectation fields are scenario specific; each checker reads
    only the fields its kind defines.
    """

    scenario_id: str
    title: str
    kind: str
    prompt: str
    max_output_tokens: int = 512
    requires_vision: bool = False
    requires_execution: bool = False
    expected_text: str = ""
    expected_json: dict[str, Any] = field(default_factory=dict)
    expected_number: int = 0
    expected_color: str = ""
    function_name: str = ""
    test_source: str = ""
    image_png: bytes | None = None


EXACT_PROMPT = (
    "Repeat the token below exactly once. Copy it character for character, "
    "including lowercase letters and the hyphen. Do not add quotes, "
    "punctuation, spaces, or any other text.\n\n"
    "bench-ok-42"
)

JSON_PROMPT = (
    "Return exactly one JSON object and nothing else. No markdown code fences, "
    "no explanations, no text before or after the object.\n"
    "The object must contain exactly these three keys:\n"
    '- "city": the string "Lisbon"\n'
    '- "count": the integer 7\n'
    '- "ok": the boolean true'
)

BUGGY_SOURCE = '''def fizz_sum(numbers):
    """Return the sum of the numbers that are divisible by three."""
    total = 0
    for index in range(len(numbers) - 1):
        if numbers[index] % 3 == 0:
            total += numbers[index]
    return total
'''

CODE_FIX_PROMPT = (
    "The Python function below has a bug: it never inspects the last element "
    "of the list.\n\n"
    "```python\n" + BUGGY_SOURCE + "```\n\n"
    "Rewrite the function so it is correct for every list, including the "
    "empty list. Reply with exactly one Python code block that defines the "
    "corrected function. Keep the name fizz_sum. Do not include tests, "
    "examples, print statements, or explanations."
)

CODE_FIX_TESTS = """assert fizz_sum([1, 2, 3]) == 3, "fizz_sum([1, 2, 3]) must equal 3"
assert fizz_sum([3, 3, 3, 3]) == 12, "fizz_sum([3, 3, 3, 3]) must equal 12"
assert fizz_sum([]) == 0, "fizz_sum([]) must equal 0"
assert fizz_sum([-3, 1, 6]) == 3, "fizz_sum([-3, 1, 6]) must equal 3"
"""

ARITHMETIC_PROMPT = (
    "A crate holds 4 rows of 6 bottles each, so it starts with 24 bottles. "
    "17 bottles are removed from the crate. Then 9 more bottles are added. "
    "How many bottles are in the crate now? Reply with only the final "
    "integer and nothing else."
)

VISION_PROMPT = (
    "This image contains two solid color blocks side by side. What single "
    "color fills the LEFT block? Answer with one color word only."
)

COLOR_WORDS = (
    "red",
    "blue",
    "green",
    "yellow",
    "orange",
    "purple",
    "pink",
    "brown",
    "black",
    "white",
    "gray",
    "grey",
)


def build_two_color_png(
    left_rgb: tuple[int, int, int],
    right_rgb: tuple[int, int, int],
    size: int = 64,
) -> bytes:
    """Build a deterministic PNG whose left and right halves are solid colors."""
    rows: list[bytes] = []
    for _ in range(size):
        row = bytearray([0])
        for column in range(size):
            row.extend(left_rgb if column < size // 2 else right_rgb)
        rows.append(bytes(row))
    header = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)
    return b"".join(
        (
            b"\x89PNG\r\n\x1a\n",
            _png_chunk(b"IHDR", header),
            _png_chunk(b"IDAT", zlib.compress(b"".join(rows), 9)),
            _png_chunk(b"IEND", b""),
        )
    )


def _png_chunk(tag: bytes, data: bytes) -> bytes:
    checksum = zlib.crc32(tag + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", checksum)


def png_data_url(png: bytes) -> str:
    """Return an inline data URL for an OpenAI-compatible image part."""
    encoded = base64.b64encode(png).decode("ascii")
    return f"data:image/png;base64,{encoded}"


def build_scenarios() -> tuple[Scenario, ...]:
    """Return the fixed benchmark suite in a stable order."""
    return (
        Scenario(
            scenario_id="exact_instruction",
            title="Exact instruction following",
            kind="exact_text",
            prompt=EXACT_PROMPT,
            max_output_tokens=32,
            expected_text="bench-ok-42",
        ),
        Scenario(
            scenario_id="strict_json",
            title="Strict JSON conformance",
            kind="strict_json",
            prompt=JSON_PROMPT,
            max_output_tokens=128,
            expected_json={"city": "Lisbon", "count": 7, "ok": True},
        ),
        Scenario(
            scenario_id="code_fix",
            title="Deterministic Python bug fix",
            kind="code_fix",
            prompt=CODE_FIX_PROMPT,
            max_output_tokens=512,
            requires_execution=True,
            function_name="fizz_sum",
            test_source=CODE_FIX_TESTS,
        ),
        Scenario(
            scenario_id="arithmetic_chain",
            title="Multi-step arithmetic and logic",
            kind="arithmetic",
            prompt=ARITHMETIC_PROMPT,
            max_output_tokens=64,
            expected_number=16,
        ),
        Scenario(
            scenario_id="vision_two_color",
            title="Two-color image question",
            kind="vision",
            prompt=VISION_PROMPT,
            max_output_tokens=32,
            requires_vision=True,
            expected_color="red",
            image_png=build_two_color_png((220, 30, 30), (30, 60, 220)),
        ),
    )


def check_exact_text(output: str, scenario: Scenario) -> CheckOutcome:
    """Pass only when the whole response is the expected token."""
    actual = output.strip()
    if actual == scenario.expected_text:
        return CheckOutcome(True, "response matched the expected token exactly")
    return CheckOutcome(
        False,
        f"expected {scenario.expected_text!r}, got {actual!r}",
    )


def check_strict_json(output: str, scenario: Scenario) -> CheckOutcome:
    """Pass only when the whole response is one JSON object with exact keys."""
    text = output.strip()
    if not text:
        return CheckOutcome(False, "response was empty")
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError as error:
        return CheckOutcome(False, f"response is not a single JSON document: {error}")
    if not isinstance(parsed, dict):
        return CheckOutcome(
            False,
            f"expected a JSON object, got {type(parsed).__name__}",
        )
    expected_keys = set(scenario.expected_json)
    actual_keys = set(parsed)
    if actual_keys != expected_keys:
        missing = sorted(expected_keys - actual_keys)
        extra = sorted(actual_keys - expected_keys)
        return CheckOutcome(
            False,
            f"key mismatch (missing={missing}, unexpected={extra})",
        )
    for key, expected in scenario.expected_json.items():
        actual = parsed[key]
        if type(actual) is not type(expected):
            return CheckOutcome(
                False,
                f"key {key!r} expected {type(expected).__name__}, "
                f"got {type(actual).__name__}",
            )
        if actual != expected:
            return CheckOutcome(
                False,
                f"key {key!r} expected {expected!r}, got {actual!r}",
            )
    return CheckOutcome(True, "response is a single conforming JSON object")


def extract_python_code(output: str) -> str | None:
    """Return the candidate Python source from a response, if any."""
    blocks = re.findall(r"```[ \t]*(?:python|py)?[ \t]*\n(.*?)```", output, re.DOTALL)
    if blocks:
        return max(blocks, key=len).strip()
    stripped = output.strip()
    if "def " in stripped:
        return stripped
    return None


def execute_snippet(
    code: str,
    test_source: str,
    function_name: str,
    timeout_seconds: float = SNIPPET_EXECUTION_TIMEOUT_SECONDS,
) -> CheckOutcome:
    """Execute candidate code in a subprocess and apply the fixed checks."""
    payload = json.dumps(
        {"code": code, "tests": test_source, "function_name": function_name}
    )
    with tempfile.TemporaryDirectory(prefix="ccr-model-benchmark-") as directory:
        environment = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": directory,
            "LC_ALL": "C",
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONIOENCODING": "utf-8",
        }
        try:
            completed = subprocess.run(
                [sys.executable, "-c", SNIPPET_RUNNER_SOURCE],
                input=payload,
                capture_output=True,
                text=True,
                cwd=directory,
                env=environment,
                timeout=timeout_seconds,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return CheckOutcome(
                False,
                f"candidate code exceeded {timeout_seconds:g}s execution timeout",
            )
        except OSError as error:
            return CheckOutcome(False, f"could not start checker process: {error}")

    output = (completed.stdout or "").strip()
    if not output:
        stderr = (completed.stderr or "").strip()
        return CheckOutcome(
            False,
            f"checker produced no result (exit {completed.returncode}): {stderr[:200]}",
        )
    try:
        result = json.loads(output.splitlines()[-1])
    except json.JSONDecodeError as error:
        return CheckOutcome(False, f"checker result was not JSON: {error}")
    if not isinstance(result, dict):
        return CheckOutcome(False, "checker result was not a JSON object")
    status = result.get("status")
    detail = str(result.get("detail", ""))
    if status == "passed":
        return CheckOutcome(True, detail or "candidate code passed every check")
    return CheckOutcome(False, f"{status}: {detail}" if detail else str(status))


def check_code_fix(output: str, scenario: Scenario) -> CheckOutcome:
    """Pass only when the returned snippet satisfies the fixed assertions."""
    code = extract_python_code(output)
    if code is None:
        return CheckOutcome(False, "response contained no Python code block")
    return execute_snippet(code, scenario.test_source, scenario.function_name)


def check_arithmetic(output: str, scenario: Scenario) -> CheckOutcome:
    """Pass only when the last integer in the response is the expected one."""
    numbers = re.findall(r"-?\d+", output)
    if not numbers:
        return CheckOutcome(False, "response contained no integer")
    actual = int(numbers[-1])
    if actual == scenario.expected_number:
        return CheckOutcome(
            True,
            f"final answer {actual} matched the expected value",
        )
    return CheckOutcome(
        False,
        f"expected final answer {scenario.expected_number}, extracted {actual}",
    )


def check_vision(output: str, scenario: Scenario) -> CheckOutcome:
    """Pass only when the first color mentioned is the expected one."""
    lowered = output.lower()
    positions: list[tuple[int, str]] = []
    for color in COLOR_WORDS:
        match = re.search(rf"\b{color}\b", lowered)
        if match is not None:
            positions.append((match.start(), color))
    if not positions:
        return CheckOutcome(False, "response mentioned no color word")
    positions.sort()
    first = positions[0][1]
    if first == scenario.expected_color:
        return CheckOutcome(True, f"first color mentioned was {first}")
    return CheckOutcome(
        False,
        f"expected {scenario.expected_color}, first color mentioned was {first}",
    )


CHECKERS = {
    "exact_text": check_exact_text,
    "strict_json": check_strict_json,
    "code_fix": check_code_fix,
    "arithmetic": check_arithmetic,
    "vision": check_vision,
}


def run_checker(scenario: Scenario, output: str) -> CheckOutcome:
    """Apply the checker registered for the scenario kind."""
    checker = CHECKERS.get(scenario.kind)
    if checker is None:
        return CheckOutcome(False, f"no checker registered for kind {scenario.kind!r}")
    return checker(output, scenario)


def scenario_plan(scenario: Scenario) -> dict[str, Any]:
    """Return a JSON-safe description of a scenario for reports."""
    return {
        "scenario_id": scenario.scenario_id,
        "title": scenario.title,
        "kind": scenario.kind,
        "max_output_tokens": scenario.max_output_tokens,
        "requires_vision": scenario.requires_vision,
        "requires_execution": scenario.requires_execution,
        "has_image_fixture": scenario.image_png is not None,
    }


def scenario_file_digest(scenario: Scenario) -> str:
    """Return a short digest of the scenario prompt for report provenance."""
    import hashlib

    return hashlib.sha256(scenario.prompt.encode("utf-8")).hexdigest()[:16]


def estimate_tokens(text: str) -> int:
    """Rough token estimate used only when the upstream reports no usage."""
    if not text:
        return 0
    return max(1, math.ceil(len(text) / 4))
