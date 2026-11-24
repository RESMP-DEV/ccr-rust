#!/usr/bin/env python3
"""Offline tests for the per-model benchmark harness.

Every test here runs without a network: the checkers are exercised against
recorded responses, the code-fix checker executes snippets in a local
subprocess, and the CLI is driven in dry-run mode.
"""

from __future__ import annotations

import io
import json
import struct
import unittest
import zlib
from contextlib import redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any
from unittest import mock

import ccr_model_benchmark as benchmark
import ccr_model_benchmark_suite as suite
import ccr_model_benchmark_transport as transport


def scenario_by_id(scenario_id: str) -> suite.Scenario:
    """Return one suite scenario by id."""
    for scenario in suite.build_scenarios():
        if scenario.scenario_id == scenario_id:
            return scenario
    raise AssertionError(f"unknown scenario {scenario_id}")


class SuiteDefinitionTests(unittest.TestCase):
    def test_suite_covers_the_required_scenario_kinds(self) -> None:
        kinds = {scenario.kind for scenario in suite.build_scenarios()}
        self.assertEqual(
            kinds,
            {"exact_text", "strict_json", "code_fix", "arithmetic", "vision"},
        )

    def test_scenario_ids_are_unique(self) -> None:
        ids = [scenario.scenario_id for scenario in suite.build_scenarios()]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertEqual(len(ids), 5)

    def test_every_scenario_has_a_registered_checker(self) -> None:
        for scenario in suite.build_scenarios():
            self.assertIn(scenario.kind, suite.CHECKERS)
            outcome = suite.run_checker(scenario, "irrelevant response")
            self.assertIsInstance(outcome, suite.CheckOutcome)

    def test_vision_scenario_ships_an_image_fixture(self) -> None:
        scenario = scenario_by_id("vision_two_color")
        self.assertTrue(scenario.requires_vision)
        self.assertIsNotNone(scenario.image_png)
        self.assertEqual(scenario.expected_color, "red")

    def test_code_fix_scenario_ships_fixed_assertions(self) -> None:
        scenario = scenario_by_id("code_fix")
        self.assertTrue(scenario.requires_execution)
        self.assertEqual(scenario.function_name, "fizz_sum")
        self.assertIn("assert", scenario.test_source)

    def test_scenario_plan_is_json_serializable(self) -> None:
        plan = [suite.scenario_plan(item) for item in suite.build_scenarios()]
        json.dumps(plan)
        self.assertEqual(len(plan), 5)


class ExactTextCheckerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scenario = scenario_by_id("exact_instruction")

    def test_exact_token_passes(self) -> None:
        outcome = suite.run_checker(self.scenario, "bench-ok-42")
        self.assertTrue(outcome.passed)

    def test_surrounding_whitespace_still_passes(self) -> None:
        outcome = suite.run_checker(self.scenario, "  bench-ok-42\n")
        self.assertTrue(outcome.passed)

    def test_trailing_punctuation_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "bench-ok-42.")
        self.assertFalse(outcome.passed)
        self.assertIn("expected", outcome.detail)

    def test_wrong_case_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "BENCH-OK-42")
        self.assertFalse(outcome.passed)

    def test_extra_text_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "bench-ok-42 done")
        self.assertFalse(outcome.passed)


class StrictJsonCheckerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scenario = scenario_by_id("strict_json")

    def test_conforming_object_passes(self) -> None:
        outcome = suite.run_checker(
            self.scenario, '{"city": "Lisbon", "count": 7, "ok": true}'
        )
        self.assertTrue(outcome.passed)

    def test_markdown_fence_fails(self) -> None:
        outcome = suite.run_checker(
            self.scenario,
            '```json\n{"city": "Lisbon", "count": 7, "ok": true}\n```',
        )
        self.assertFalse(outcome.passed)
        self.assertIn("single JSON document", outcome.detail)

    def test_trailing_prose_fails(self) -> None:
        outcome = suite.run_checker(
            self.scenario,
            '{"city": "Lisbon", "count": 7, "ok": true} hope this helps',
        )
        self.assertFalse(outcome.passed)

    def test_extra_key_fails(self) -> None:
        outcome = suite.run_checker(
            self.scenario,
            '{"city": "Lisbon", "count": 7, "ok": true, "extra": 1}',
        )
        self.assertFalse(outcome.passed)
        self.assertIn("key mismatch", outcome.detail)

    def test_missing_key_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, '{"city": "Lisbon", "count": 7}')
        self.assertFalse(outcome.passed)

    def test_string_instead_of_integer_fails(self) -> None:
        outcome = suite.run_checker(
            self.scenario, '{"city": "Lisbon", "count": "7", "ok": true}'
        )
        self.assertFalse(outcome.passed)
        self.assertIn("expected int", outcome.detail)

    def test_boolean_may_not_be_replaced_by_an_integer(self) -> None:
        outcome = suite.run_checker(
            self.scenario, '{"city": "Lisbon", "count": 7, "ok": 1}'
        )
        self.assertFalse(outcome.passed)
        self.assertIn("expected bool", outcome.detail)

    def test_empty_response_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "   ")
        self.assertFalse(outcome.passed)


class CodeFixCheckerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scenario = scenario_by_id("code_fix")

    def test_correct_fix_passes(self) -> None:
        code = (
            "```python\n"
            "def fizz_sum(numbers):\n"
            "    return sum(value for value in numbers if value % 3 == 0)\n"
            "```"
        )
        outcome = suite.run_checker(self.scenario, code)
        self.assertTrue(outcome.passed, outcome.detail)

    def test_original_buggy_version_fails(self) -> None:
        outcome = suite.run_checker(
            self.scenario, f"```python\n{suite.BUGGY_SOURCE}```"
        )
        self.assertFalse(outcome.passed)

    def test_missing_function_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "```python\nx = 1\n```")
        self.assertFalse(outcome.passed)
        self.assertIn("missing_function", outcome.detail)

    def test_raising_snippet_fails(self) -> None:
        outcome = suite.run_checker(
            self.scenario,
            "```python\ndef fizz_sum(numbers):\n    raise ValueError('x')\n```",
        )
        self.assertFalse(outcome.passed)

    def test_response_without_code_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "I cannot help with that.")
        self.assertFalse(outcome.passed)
        self.assertIn("no Python code block", outcome.detail)

    def test_execution_timeout_is_reported(self) -> None:
        outcome = suite.execute_snippet(
            "import time\ntime.sleep(5)\n", "", "fizz_sum", timeout_seconds=0.5
        )
        self.assertFalse(outcome.passed)
        self.assertIn("timeout", outcome.detail)

    def test_extract_python_code_prefers_the_fenced_block(self) -> None:
        extracted = suite.extract_python_code(
            "here you go:\n```python\ndef fizz_sum(n):\n    return 0\n```"
        )
        self.assertIsNotNone(extracted)
        self.assertIn("def fizz_sum", extracted or "")

    def test_extract_python_code_accepts_uncapped_source(self) -> None:
        extracted = suite.extract_python_code("def fizz_sum(n):\n    return 0\n")
        self.assertIsNotNone(extracted)

    def test_extract_python_code_returns_none_for_prose(self) -> None:
        self.assertIsNone(suite.extract_python_code("no code here at all"))


class ArithmeticCheckerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scenario = scenario_by_id("arithmetic_chain")

    def test_bare_answer_passes(self) -> None:
        outcome = suite.run_checker(self.scenario, "16")
        self.assertTrue(outcome.passed)

    def test_final_answer_after_working_passes(self) -> None:
        outcome = suite.run_checker(self.scenario, "24 - 17 = 7, then 7 + 9 = 16")
        self.assertTrue(outcome.passed)

    def test_trap_answer_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "33")
        self.assertFalse(outcome.passed)
        self.assertIn("expected final answer 16", outcome.detail)

    def test_answer_without_digits_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "sixteen bottles")
        self.assertFalse(outcome.passed)
        self.assertIn("no integer", outcome.detail)


class VisionCheckerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scenario = scenario_by_id("vision_two_color")

    def test_expected_color_passes(self) -> None:
        outcome = suite.run_checker(self.scenario, "red")
        self.assertTrue(outcome.passed)

    def test_sentence_mentioning_the_expected_color_passes(self) -> None:
        outcome = suite.run_checker(self.scenario, "The left block is red.")
        self.assertTrue(outcome.passed)

    def test_wrong_color_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "blue")
        self.assertFalse(outcome.passed)

    def test_first_mentioned_color_decides(self) -> None:
        outcome = suite.run_checker(self.scenario, "blue on the left, red on the right")
        self.assertFalse(outcome.passed)

    def test_response_without_a_color_fails(self) -> None:
        outcome = suite.run_checker(self.scenario, "I cannot tell.")
        self.assertFalse(outcome.passed)


class PngFixtureTests(unittest.TestCase):
    def test_png_has_a_valid_signature_and_header(self) -> None:
        png = suite.build_two_color_png((220, 30, 30), (30, 60, 220), size=8)
        self.assertTrue(png.startswith(b"\x89PNG\r\n\x1a\n"))
        width, height = struct.unpack(">II", png[16:24])
        self.assertEqual((width, height), (8, 8))

    def test_png_pixels_carry_the_two_colors(self) -> None:
        size = 8
        png = suite.build_two_color_png((220, 30, 30), (30, 60, 220), size=size)
        start = png.index(b"IDAT") + 4
        length = struct.unpack(">I", png[start - 8 : start - 4])[0]
        raw = zlib.decompress(png[start : start + length])
        stride = 1 + size * 3
        left_pixel = raw[1:4]
        right_pixel = raw[stride - 3 : stride]
        self.assertEqual(left_pixel, bytes((220, 30, 30)))
        self.assertEqual(right_pixel, bytes((30, 60, 220)))

    def test_data_url_is_inline_base64_png(self) -> None:
        url = suite.png_data_url(b"\x89PNG\r\n\x1a\nrest")
        self.assertTrue(url.startswith("data:image/png;base64,"))


class TransportTests(unittest.TestCase):
    @staticmethod
    def sse_lines(*frames: str) -> list[tuple[float, str]]:
        lines: list[tuple[float, str]] = []
        for index, frame in enumerate(frames):
            lines.append((0.1 * index, f"data: {frame}\n"))
            lines.append((0.1 * index + 0.01, "\n"))
        return lines

    def test_content_and_usage_are_extracted(self) -> None:
        lines = self.sse_lines(
            '{"choices": [{"delta": {"content": "bench"}}]}',
            '{"choices": [{"delta": {"content": "-ok"}}]}',
            '{"choices": [{"delta": {}}], "usage": {"prompt_tokens": 11,'
            ' "completion_tokens": 4}}',
            "[DONE]",
        )
        outcome = transport.consume_sse(lines)
        self.assertEqual(outcome.text, "bench-ok")
        self.assertIsNotNone(outcome.ttft_seconds)
        self.assertEqual(outcome.input_tokens, 11)
        self.assertEqual(outcome.output_tokens, 4)
        self.assertEqual(outcome.usage_source, "upstream")

    def test_ttft_is_the_first_content_timestamp(self) -> None:
        lines = self.sse_lines(
            '{"choices": [{"delta": {"content": "x"}}]}',
            '{"choices": [{"delta": {"content": "y"}}]}',
        )
        outcome = transport.consume_sse(lines)
        self.assertEqual(outcome.ttft_seconds, 0.0)

    def test_malformed_frames_are_ignored(self) -> None:
        lines = self.sse_lines(
            "not json", '{"choices": [{"delta": {"content": "ok"}}]}'
        )
        outcome = transport.consume_sse(lines)
        self.assertEqual(outcome.text, "ok")

    def test_anthropic_style_usage_is_understood(self) -> None:
        lines = self.sse_lines(
            '{"choices": [{"delta": {"content": "ok"}}],'
            ' "usage": {"input_tokens": 5, "output_tokens": 2}}'
        )
        outcome = transport.consume_sse(lines)
        self.assertEqual(outcome.input_tokens, 5)
        self.assertEqual(outcome.output_tokens, 2)

    def test_tokens_are_estimated_when_usage_is_absent(self) -> None:
        lines = self.sse_lines('{"choices": [{"delta": {"content": "hello world"}}]}')
        outcome = transport.consume_sse(lines)
        self.assertEqual(outcome.usage_source, "estimated")
        self.assertEqual(outcome.output_tokens, suite.estimate_tokens("hello world"))

    def test_empty_stream_reports_no_text(self) -> None:
        outcome = transport.consume_sse([])
        self.assertEqual(outcome.text, "")
        self.assertIsNone(outcome.ttft_seconds)


class _FakeResponse:
    """A canned HTTP response with CCR tier headers."""

    def __init__(self, status: int, headers: dict[str, str], body: bytes) -> None:
        self.status = status
        self._headers = headers
        self._body = body

    def getheader(self, name: str) -> str | None:
        for key, value in self._headers.items():
            if key.lower() == name.lower():
                return value
        return None

    def readline(self) -> bytes:
        if not self._body:
            return b""
        line, _, self._body = self._body.partition(b"\n")
        return line + b"\n"

    def read(self) -> bytes:
        body, self._body = self._body, b""
        return body


class _FakeConnection:
    """A connection that replays one canned response."""

    def __init__(self, response: _FakeResponse) -> None:
        self._response = response

    def request(
        self, method: str, url: str, body: Any = None, headers: Any = None
    ) -> None:
        return None

    def getresponse(self) -> _FakeResponse:
        return self._response

    def close(self) -> None:
        return None


class TierHeaderTransportTests(unittest.TestCase):
    """The transport must record the tier headers CCR reports."""

    @staticmethod
    def _run(response: _FakeResponse) -> transport.StreamOutcome:
        connection = _FakeConnection(response)
        with mock.patch.object(
            transport,
            "_open_connection",
            return_value=(connection, ""),
        ):
            return transport.stream_chat_completion(
                "http://127.0.0.1:3456",
                "test",
                {"model": "zai,glm-5.4", "messages": [], "stream": True},
                10.0,
            )

    def test_served_tier_header_is_captured_on_a_successful_response(self) -> None:
        outcome = self._run(
            _FakeResponse(
                200,
                {"x-ccr-tier": "minimax"},
                b'data: {"choices": [{"delta": {"content": "bench"}}]}\n\n'
                b"data: [DONE]\n\n",
            )
        )
        self.assertIsNone(outcome.error_class)
        self.assertEqual(outcome.text, "bench")
        self.assertEqual(outcome.served_tier, "minimax")
        self.assertEqual(outcome.rate_limited_tiers, [])

    def test_rate_limited_tiers_header_is_captured_on_a_429(self) -> None:
        outcome = self._run(
            _FakeResponse(
                429,
                {
                    "x-ccr-tier": "zai",
                    "x-ccr-rate-limited-tiers": "zai,glm-5.4,minimax,MiniMax-M3.1-Flash",
                },
                b'{"error": {"message": "limited"}}',
            )
        )
        self.assertEqual(outcome.error_class, "http_error")
        self.assertEqual(outcome.served_tier, "zai")
        self.assertEqual(
            outcome.rate_limited_tiers,
            ["zai,glm-5.4", "minimax,MiniMax-M3.1-Flash"],
        )

    def test_provenance_survives_a_midstream_timeout(self) -> None:
        # Headers arrive before the body. A stream that then times out must
        # keep its served tier, or a clean route could never be reported clean.
        class _TimingOutResponse(_FakeResponse):
            def readline(self, size: int = -1) -> bytes:
                raise TimeoutError("stream stalled after headers")

        outcome = self._run(
            _TimingOutResponse(
                200,
                {
                    "x-ccr-tier": "zai",
                    "x-ccr-rate-limited-tiers": "minimax,MiniMax-M3.1-Flash",
                },
                b"",
            )
        )
        self.assertEqual(outcome.error_class, "timeout")
        self.assertEqual(outcome.served_tier, "zai")
        self.assertEqual(outcome.rate_limited_tiers, ["minimax,MiniMax-M3.1-Flash"])


class TierHeaderParsingTests(unittest.TestCase):
    """The tier headers are parsed without a live listener."""

    def test_missing_headers_report_no_provenance(self) -> None:
        class Stub:
            def getheader(self, name: str) -> str | None:
                return None

        served_tier, rate_limited = transport.read_tier_headers(Stub())
        self.assertIsNone(served_tier)
        self.assertEqual(rate_limited, [])

    def test_header_names_are_the_documented_ones(self) -> None:
        self.assertEqual(transport.TIER_HEADER, "x-ccr-tier")
        self.assertEqual(
            transport.RATE_LIMITED_TIERS_HEADER, "x-ccr-rate-limited-tiers"
        )

    def test_blank_headers_report_no_provenance(self) -> None:
        class Stub:
            def getheader(self, name: str) -> str | None:
                return "  "

        served_tier, rate_limited = transport.read_tier_headers(Stub())
        self.assertIsNone(served_tier)
        self.assertEqual(rate_limited, [])

    def test_rate_limited_routes_are_split_on_commas(self) -> None:
        class Stub:
            def getheader(self, name: str) -> str | None:
                if name == transport.RATE_LIMITED_TIERS_HEADER:
                    return "zai,glm-5.4, minimax,MiniMax-M3.1-Flash"
                return "zai"

        served_tier, rate_limited = transport.read_tier_headers(Stub())
        self.assertEqual(served_tier, "zai")
        self.assertEqual(rate_limited, ["zai,glm-5.4", "minimax,MiniMax-M3.1-Flash"])

    def test_a_single_rate_limited_route_is_recovered(self) -> None:
        self.assertEqual(
            transport.parse_rate_limited_routes("zai,glm-5.4"), ["zai,glm-5.4"]
        )

    def test_a_bare_rate_limited_tier_is_reported_alone(self) -> None:
        self.assertEqual(transport.parse_rate_limited_routes("codex"), ["codex"])


class AggregationTests(unittest.TestCase):
    @staticmethod
    def record(
        scenario_id: str,
        repetition: int,
        passed: bool | None,
        *,
        skipped: bool = False,
        wall: float = 2.0,
        ttft: float | None = 0.5,
        output_tokens: int | None = 100,
        error_class: str = "none",
    ) -> benchmark.RunRecord:
        return benchmark.RunRecord(
            scenario_id=scenario_id,
            title=scenario_id,
            kind="exact_text",
            repetition=repetition,
            passed=passed,
            skipped=skipped,
            skip_reason="skipped for the test" if skipped else None,
            ttft_seconds=ttft,
            wall_seconds=wall,
            output_tokens=output_tokens,
            input_tokens=10,
            usage_source="upstream",
            error_class=error_class,
            error_detail=None,
            output_chars=42,
            output_preview="",
        )

    def test_accuracy_and_failure_rate(self) -> None:
        runs = [
            self.record("a", 1, True),
            self.record("a", 2, True),
            self.record("a", 3, False),
            self.record("b", 1, True),
        ]
        summary = benchmark.aggregate_runs(runs)
        self.assertEqual(summary["runs_attempted"], 4)
        self.assertEqual(summary["runs_passed"], 3)
        self.assertAlmostEqual(summary["accuracy"], 0.75)
        self.assertAlmostEqual(summary["failure_rate"], 0.25)

    def test_skipped_runs_leave_the_denominator(self) -> None:
        runs = [
            self.record("a", 1, True),
            self.record("vision", 1, None, skipped=True, error_class="skipped"),
        ]
        summary = benchmark.aggregate_runs(runs)
        self.assertEqual(summary["runs_attempted"], 1)
        self.assertEqual(summary["runs_skipped"], 1)
        self.assertAlmostEqual(summary["accuracy"], 1.0)

    def test_medians_and_throughput(self) -> None:
        runs = [
            self.record("a", 1, True, wall=2.0, ttft=0.5, output_tokens=150),
            self.record("a", 2, True, wall=4.0, ttft=1.5, output_tokens=250),
        ]
        summary = benchmark.aggregate_runs(runs)
        self.assertAlmostEqual(summary["median_wall_seconds"], 3.0)
        self.assertAlmostEqual(summary["median_ttft_seconds"], 1.0)
        self.assertAlmostEqual(summary["tokens_per_second"], 400.0 / 4.0)

    def test_stability_is_outcome_consistency(self) -> None:
        stable = benchmark.aggregate_runs(
            [self.record("a", 1, True), self.record("a", 2, True)]
        )
        unstable = benchmark.aggregate_runs(
            [self.record("a", 1, True), self.record("a", 2, False)]
        )
        self.assertAlmostEqual(stable["stability"], 1.0)
        self.assertAlmostEqual(unstable["stability"], 0.5)

    def test_error_classes_are_counted(self) -> None:
        runs = [
            self.record("a", 1, False, error_class="http_error"),
            self.record("a", 2, False, error_class="check_failed"),
            self.record("a", 3, False, error_class="http_error"),
        ]
        summary = benchmark.aggregate_runs(runs)
        self.assertEqual(summary["error_classes"], {"check_failed": 1, "http_error": 2})

    def test_per_scenario_breakdown_is_reported(self) -> None:
        runs = [
            self.record("a", 1, True),
            self.record("a", 2, False),
            self.record("b", 1, True),
        ]
        summary = benchmark.aggregate_runs(runs)
        self.assertAlmostEqual(summary["per_scenario"]["a"]["accuracy"], 0.5)
        self.assertAlmostEqual(summary["per_scenario"]["b"]["accuracy"], 1.0)

    def test_empty_run_set_has_no_ratios(self) -> None:
        summary = benchmark.aggregate_runs([])
        self.assertIsNone(summary["accuracy"])
        self.assertIsNone(summary["tokens_per_second"])
        self.assertEqual(summary["runs_attempted"], 0)


class TierProvenanceTests(unittest.TestCase):
    """A run served by another tier must not be scored as the requested model."""

    @staticmethod
    def record(
        scenario_id: str = "a",
        repetition: int = 1,
        *,
        passed: bool | None = True,
        skipped: bool = False,
        served_tier: str | None = None,
        rate_limited_tiers: list[str] | None = None,
    ) -> benchmark.RunRecord:
        return benchmark.RunRecord(
            scenario_id=scenario_id,
            title=scenario_id,
            kind="exact_text",
            repetition=repetition,
            passed=passed,
            skipped=skipped,
            skip_reason="skipped for the test" if skipped else None,
            ttft_seconds=0.5,
            wall_seconds=2.0,
            output_tokens=100,
            input_tokens=10,
            usage_source="upstream",
            error_class="none",
            error_detail=None,
            output_chars=42,
            output_preview="",
            served_tier=served_tier,
            rate_limited_tiers=list(rate_limited_tiers or []),
        )

    def test_matching_served_tier_is_not_contaminated(self) -> None:
        runs = [self.record(served_tier="zai")]
        block = benchmark.aggregate_runs(runs, "zai,glm-5.4")["tier_provenance"]
        self.assertEqual(block["requested_provider"], "zai")
        self.assertEqual(block["served_tier_mismatch_runs"], 0)
        self.assertEqual(block["contaminated_runs"], 0)
        self.assertTrue(block["clean"])
        self.assertEqual(block["served_tiers"], {"zai": 1})

    def test_fallback_to_another_provider_is_contaminated(self) -> None:
        runs = [
            self.record(repetition=1, served_tier="zai"),
            self.record(repetition=2, served_tier="minimax"),
            self.record(repetition=3, served_tier="minimax"),
        ]
        block = benchmark.aggregate_runs(runs, "zai,glm-5.4")["tier_provenance"]
        self.assertEqual(block["served_tier_mismatch_runs"], 2)
        self.assertEqual(block["contaminated_runs"], 2)
        self.assertFalse(block["clean"])
        self.assertEqual(block["served_tiers"], {"minimax": 2, "zai": 1})

    def test_rate_limited_run_is_contaminated(self) -> None:
        runs = [self.record(rate_limited_tiers=["zai,glm-5.4"])]
        block = benchmark.aggregate_runs(runs, "zai,glm-5.4")["tier_provenance"]
        self.assertEqual(block["rate_limited_runs"], 1)
        self.assertEqual(block["contaminated_runs"], 1)
        self.assertEqual(block["rate_limited_tier_names"], {"zai,glm-5.4": 1})
        self.assertFalse(block["clean"])

    def test_a_run_that_is_both_is_counted_once(self) -> None:
        runs = [self.record(served_tier="minimax", rate_limited_tiers=["zai,glm-5.4"])]
        block = benchmark.aggregate_runs(runs, "zai,glm-5.4")["tier_provenance"]
        self.assertEqual(block["served_tier_mismatch_runs"], 1)
        self.assertEqual(block["rate_limited_runs"], 1)
        self.assertEqual(block["contaminated_runs"], 1)

    def test_missing_served_tier_is_unknown_not_contaminated(self) -> None:
        runs = [self.record(served_tier=None)]
        block = benchmark.aggregate_runs(runs, "zai,glm-5.4")["tier_provenance"]
        self.assertEqual(block["runs_without_served_tier"], 1)
        self.assertEqual(block["served_tier_mismatch_runs"], 0)
        self.assertEqual(block["contaminated_runs"], 0)
        self.assertFalse(block["clean"])

    def test_skipped_runs_are_excluded(self) -> None:
        runs = [
            self.record(
                passed=None,
                skipped=True,
                served_tier="minimax",
                rate_limited_tiers=["zai,glm-5.4"],
            )
        ]
        block = benchmark.aggregate_runs(runs, "zai,glm-5.4")["tier_provenance"]
        self.assertEqual(block["contaminated_runs"], 0)
        self.assertEqual(block["runs_without_served_tier"], 0)
        self.assertTrue(block["clean"])

    def test_aggregation_without_a_route_stays_compatible(self) -> None:
        runs = [self.record(served_tier="minimax")]
        summary = benchmark.aggregate_runs(runs)
        block = summary["tier_provenance"]
        self.assertIsNone(block["requested_provider"])
        self.assertEqual(block["served_tier_mismatch_runs"], 0)
        self.assertEqual(block["served_tiers"], {"minimax": 1})
        self.assertEqual(summary["runs_attempted"], 1)

    def test_tier_provider_reads_the_provider_component(self) -> None:
        self.assertEqual(benchmark.route_provider("zai,glm-5.4"), "zai")
        self.assertEqual(benchmark.tier_provider("zai"), "zai")
        self.assertEqual(benchmark.tier_provider("zai,glm-5.4"), "zai")
        self.assertIsNone(benchmark.tier_provider(None))

    def test_run_record_serializes_the_tier_provenance(self) -> None:
        record = self.record(served_tier="minimax", rate_limited_tiers=["zai,glm-5.4"])
        payload = record.to_dict()
        self.assertEqual(payload["served_tier"], "minimax")
        self.assertEqual(payload["rate_limited_tiers"], ["zai,glm-5.4"])

    def test_run_record_without_provenance_stays_valid(self) -> None:
        record = benchmark.RunRecord(
            scenario_id="a",
            title="a",
            kind="exact_text",
            repetition=1,
            passed=True,
            skipped=False,
            skip_reason=None,
            ttft_seconds=0.5,
            wall_seconds=2.0,
            output_tokens=100,
            input_tokens=10,
            usage_source="upstream",
            error_class="none",
            error_detail=None,
            output_chars=42,
            output_preview="",
        )
        payload = record.to_dict()
        self.assertIsNone(payload["served_tier"])
        self.assertEqual(payload["rate_limited_tiers"], [])

    def test_scorecard_reports_the_tier_provenance(self) -> None:
        scorecard = benchmark.build_scorecard(
            "zai,glm-5.4",
            [self.record(served_tier="minimax")],
            [],
            {"base_url": "http://127.0.0.1:3456"},
            "live",
            suite.build_scenarios(),
        )
        block = scorecard["summary"]["tier_provenance"]
        self.assertEqual(block["requested_provider"], "zai")
        self.assertEqual(block["contaminated_runs"], 1)
        self.assertEqual(scorecard["runs"][0]["served_tier"], "minimax")
        json.dumps(scorecard)

    def test_summary_prints_the_tier_provenance(self) -> None:
        scorecard = benchmark.build_scorecard(
            "zai,glm-5.4",
            [self.record(served_tier="minimax")],
            [],
            {"base_url": "http://127.0.0.1:3456"},
            "live",
            suite.build_scenarios(),
        )
        output = io.StringIO()
        with redirect_stdout(output):
            benchmark.print_summary([scorecard])
        printed = output.getvalue()
        self.assertIn("tier provenance:", printed)
        self.assertIn("served=[minimax:1]", printed)
        self.assertIn("contaminated=1", printed)


class ExecuteScenarioTierTests(unittest.TestCase):
    """A run record must carry the tier provenance of its response."""

    @staticmethod
    def outcome(**overrides: Any) -> transport.StreamOutcome:
        values: dict[str, Any] = {
            "text": "bench-ok-42",
            "ttft_seconds": 0.1,
            "input_tokens": 5,
            "output_tokens": 2,
            "usage_source": "upstream",
            "error_class": None,
            "error_detail": None,
            "served_tier": "minimax",
            "rate_limited_tiers": ["zai,glm-5.4"],
        }
        values.update(overrides)
        return transport.StreamOutcome(**values)

    @staticmethod
    def execute(outcome: transport.StreamOutcome) -> benchmark.RunRecord:
        with mock.patch.object(
            benchmark.transport, "stream_chat_completion", return_value=outcome
        ):
            return benchmark.execute_scenario(
                route="zai,glm-5.4",
                scenario=scenario_by_id("exact_instruction"),
                repetition=1,
                base_url="http://127.0.0.1:3456",
                api_key="test",
                temperature=0.0,
                timeout_seconds=10.0,
            )

    def test_a_successful_run_carries_its_served_tier(self) -> None:
        record = self.execute(self.outcome())
        self.assertTrue(record.passed)
        self.assertEqual(record.served_tier, "minimax")
        self.assertEqual(record.rate_limited_tiers, ["zai,glm-5.4"])

    def test_a_failed_run_carries_its_served_tier(self) -> None:
        record = self.execute(
            self.outcome(
                text="",
                error_class="http_error",
                error_detail="HTTP 429: rate limited",
                served_tier="zai",
            )
        )
        self.assertFalse(record.passed)
        self.assertEqual(record.served_tier, "zai")
        self.assertEqual(record.rate_limited_tiers, ["zai,glm-5.4"])


class RouteParsingTests(unittest.TestCase):
    def test_comma_is_part_of_the_route_id(self) -> None:
        self.assertEqual(benchmark.parse_routes(["zai,glm-5.4"]), ["zai,glm-5.4"])

    def test_semicolon_separates_routes(self) -> None:
        self.assertEqual(
            benchmark.parse_routes(["zai,glm-5.4;zai,glm-5.3-flash"]),
            ["zai,glm-5.4", "zai,glm-5.3-flash"],
        )

    def test_duplicate_routes_are_collapsed(self) -> None:
        self.assertEqual(
            benchmark.parse_routes(["zai,glm-5.4", "zai,glm-5.4"]),
            ["zai,glm-5.4"],
        )

    def test_slug_is_filesystem_safe(self) -> None:
        self.assertEqual(benchmark.route_slug("zai,glm-5.4"), "zai_glm-5.4")
        self.assertEqual(benchmark.route_slug("zai,glm-5.3-flash"), "zai_glm-5.3-flash")

    def test_unknown_scenario_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            benchmark.select_scenarios(["no_such_scenario"])

    def test_scenario_selection_filters_the_suite(self) -> None:
        selected = benchmark.select_scenarios(["exact_instruction"])
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0].scenario_id, "exact_instruction")


class SkipReasonTests(unittest.TestCase):
    def test_vision_is_skipped_for_undeclared_routes(self) -> None:
        scenario = scenario_by_id("vision_two_color")
        reason = benchmark.skip_reason(scenario, "zai,glm-5.4", [], True)
        self.assertIsNotNone(reason)
        self.assertIn("text-only", reason or "")

    def test_vision_runs_for_declared_routes(self) -> None:
        scenario = scenario_by_id("vision_two_color")
        reason = benchmark.skip_reason(scenario, "zai,glm-5.4", ["zai,glm-5.4"], True)
        self.assertIsNone(reason)

    def test_code_fix_is_skipped_when_execution_is_disabled(self) -> None:
        scenario = scenario_by_id("code_fix")
        reason = benchmark.skip_reason(scenario, "zai,glm-5.4", [], False)
        self.assertIsNotNone(reason)
        self.assertIn("--no-exec", reason or "")


class CliTests(unittest.TestCase):
    def test_missing_route_is_a_usage_error(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            benchmark.main(["--dry-run"])
        self.assertEqual(raised.exception.code, 2)

    def test_dry_run_writes_a_scorecard_and_exits_zero(self) -> None:
        with TemporaryDirectory() as directory:
            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = benchmark.main(
                    [
                        "--models",
                        "zai,glm-5.4;zai,glm-5.3-flash",
                        "--dry-run",
                        "--out-dir",
                        directory,
                    ]
                )
            self.assertEqual(exit_code, 0)
            printed = output.getvalue()
            self.assertIn("exact_instruction", printed)
            self.assertIn("code_fix", printed)
            self.assertIn("vision_two_color", printed)
            self.assertIn("text-only", printed)
            written = sorted(Path(directory).glob("*.dry-run.json"))
            self.assertEqual(len(written), 2)
            scorecards = [
                json.loads(item.read_text(encoding="utf-8")) for item in written
            ]
            routes = sorted(item["route"] for item in scorecards)
            self.assertEqual(routes, ["zai,glm-5.3-flash", "zai,glm-5.4"])
            scorecard = next(
                item for item in scorecards if item["route"] == "zai,glm-5.4"
            )
            self.assertEqual(scorecard["mode"], "dry-run")
            self.assertEqual(len(scorecard["suite"]["scenarios"]), 5)
            self.assertEqual(len(scorecard["skipped"]), 1)
            self.assertEqual(scorecard["skipped"][0]["scenario_id"], "vision_two_color")
            self.assertTrue(scorecard["non_claims"])

    def test_dry_run_json_output_is_machine_readable(self) -> None:
        with TemporaryDirectory() as directory:
            output = io.StringIO()
            with redirect_stdout(output):
                benchmark.main(
                    [
                        "--models",
                        "zai,glm-5.4",
                        "--dry-run",
                        "--json",
                        "--out-dir",
                        directory,
                    ]
                )
            payload = json.loads(output.getvalue())
            self.assertEqual(payload["schema"], benchmark.SCORECARD_SCHEMA)
            self.assertEqual(len(payload["routes"]), 1)

    def test_dry_run_scorecard_reports_the_tier_provenance(self) -> None:
        with TemporaryDirectory() as directory:
            output = io.StringIO()
            with redirect_stdout(output):
                benchmark.main(
                    [
                        "--models",
                        "zai,glm-5.4",
                        "--dry-run",
                        "--out-dir",
                        directory,
                    ]
                )
            scorecard = json.loads(
                Path(directory, "zai_glm-5.4.dry-run.json").read_text(encoding="utf-8")
            )
            block = scorecard["summary"]["tier_provenance"]
            self.assertEqual(block["requested_provider"], "zai")
            self.assertEqual(block["contaminated_runs"], 0)
            self.assertEqual(block["runs_without_served_tier"], 0)

    def test_scorecard_records_the_suite_and_non_claims(self) -> None:
        runs = [
            benchmark.RunRecord(
                scenario_id="exact_instruction",
                title="Exact instruction following",
                kind="exact_text",
                repetition=1,
                passed=True,
                skipped=False,
                skip_reason=None,
                ttft_seconds=0.2,
                wall_seconds=1.0,
                output_tokens=10,
                input_tokens=20,
                usage_source="upstream",
                error_class="none",
                error_detail=None,
                output_chars=12,
                output_preview="bench-ok-42",
            )
        ]
        scorecard = benchmark.build_scorecard(
            "zai,glm-5.4",
            runs,
            [],
            {"base_url": "http://127.0.0.1:3456"},
            "live",
            suite.build_scenarios(),
        )
        self.assertEqual(scorecard["route"], "zai,glm-5.4")
        self.assertEqual(scorecard["summary"]["runs_passed"], 1)
        self.assertTrue(scorecard["non_claims"])
        json.dumps(scorecard)


if __name__ == "__main__":
    unittest.main()
