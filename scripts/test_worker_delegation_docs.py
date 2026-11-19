#!/usr/bin/env python3
"""Tests for the worker-delegation instruction and operator guide."""

from __future__ import annotations

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class WorkerDelegationDocumentationTests(unittest.TestCase):
    def read(self, relative_path: str) -> str:
        return " ".join((ROOT / relative_path).read_text(encoding="utf-8").split())

    def paragraphs(self, relative_path: str) -> list[str]:
        text = (ROOT / relative_path).read_text(encoding="utf-8")
        blocks = [block.strip() for block in text.split("\n\n")]
        return [" ".join(block.split()) for block in blocks if block.strip()]

    def paragraph_starting_with(self, relative_path: str, prefix: str) -> str:
        for block in self.paragraphs(relative_path):
            if block.startswith(prefix):
                return block
        self.fail(f"{relative_path} has no paragraph starting with {prefix!r}")

    def test_canonical_client_guide_is_a_symlink_to_agents(self) -> None:
        client_guide = ROOT / "CLAUDE.md"
        agents = ROOT / "AGENTS.md"
        self.assertTrue(client_guide.is_symlink())
        self.assertEqual(client_guide.resolve(), agents.resolve())

    def test_repository_guide_defines_an_actionable_delegation_trigger(self) -> None:
        guide = self.read("AGENTS.md")
        self.assertIn("### Delegation-first triage", guide)
        self.assertIn("enumerate its pieces from the task text", guide)
        self.assertIn("bounded discovery worker brief", guide)
        self.assertIn("Send every piece", guide)
        self.assertIn("a one-piece task is still delegated", guide)
        self.assertIn("final accept/reject/commit/merge authority", guide)
        self.assertIn("cross-piece conflict resolution", guide)
        self.assertIn("never parent execution", guide)
        self.assertNotIn("tools the worker infrastructure cannot reach", guide)
        self.assertIn("own scope, owned files or surface, acceptance check", guide)
        self.assertIn("State the split before launching", guide)
        self.assertIn("Launch the workers before doing parent work", guide)
        self.assertIn("send independent pieces concurrently", guide)
        self.assertIn("do not ask permission to launch a worker", guide)
        self.assertIn("Do not collapse several delegable pieces", guide)
        self.assertNotIn("five parent tool calls or five minutes", guide)
        self.assertNotIn("Delegate by default", guide)
        self.assertIn("effort already spent are not the criterion", guide)
        self.assertIn("create_thread", guide)
        self.assertIn("ccr-rust workers list --limit 5", guide)

    def test_repository_guide_defines_a_deterministic_brief_contract(self) -> None:
        guide = self.read("AGENTS.md")
        self.assertIn("### Deterministic brief contract", guide)
        self.assertIn("Every brief is mechanically executable", guide)
        self.assertIn("one bounded outcome", guide)
        self.assertIn("exact owned files or surface", guide)
        self.assertIn("exact acceptance command or commands", guide)
        self.assertIn("report evidence the worker must produce", guide)
        self.assertIn("explicit stop condition", guide)
        self.assertIn("the parent splits it further or keeps it", guide)
        self.assertIn("Ambiguity is never delegated", guide)

    def test_repository_guide_forbids_worker_side_decomposition(self) -> None:
        guide = self.read("AGENTS.md")
        self.assertIn("The worker executes exactly one brief", guide)
        self.assertIn("does not decide whether it is a worker", guide)
        self.assertIn("does not decompose its own assignment", guide)
        self.assertIn("asks the parent for a fresh split", guide)
        self.assertNotIn("worker may decompose", guide)
        self.assertNotIn("let the worker decompose", guide)
        self.assertNotIn("worker decides the split", guide)

    def test_worker_guide_requires_bounded_briefs_and_low_context_inspection(
        self,
    ) -> None:
        guide = self.read("docs/worker-inspection.md")
        self.assertIn("by decomposing before exploring", guide)
        self.assertIn("bounded discovery worker brief", guide)
        self.assertIn("Send every piece", guide)
        self.assertIn("A one-piece task is still delegated", guide)
        self.assertIn("final accept/reject/commit/merge authority", guide)
        self.assertIn("never parent execution", guide)
        self.assertNotIn("tools the worker infrastructure cannot reach", guide)
        self.assertIn("State the split before launching", guide)
        self.assertIn("send independent pieces concurrently", guide)
        self.assertIn("Do not collapse several delegable pieces", guide)
        self.assertIn("do not ask permission to launch a worker", guide)
        self.assertIn(
            "Do not use size, file count, elapsed time, or effort already spent as the trigger",
            guide,
        )
        self.assertNotIn("five parent tool calls or five minutes", guide)
        self.assertNotIn("Delegate by default", guide)
        self.assertIn("one bounded outcome", guide)
        self.assertIn("<report>", guide)
        self.assertIn("separate foreground tool sessions", guide)
        self.assertIn("ccr-worker-usage --days 7", guide)
        self.assertIn("Evidence, not automatic approval", guide)
        self.assertIn("`running` means the launcher wrote its initial receipt", guide)
        self.assertIn("may carry a `salvage` object", guide)

    def test_worker_guide_defines_a_deterministic_brief_contract(self) -> None:
        guide = self.read("docs/worker-inspection.md")
        self.assertIn("## Brief contract", guide)
        self.assertIn("Every brief is mechanically executable", guide)
        self.assertIn("one bounded outcome", guide)
        self.assertIn("exact owned files or surface", guide)
        self.assertIn("exact acceptance command or commands", guide)
        self.assertIn("report evidence the worker must produce", guide)
        self.assertIn("explicit stop condition", guide)
        self.assertIn("the parent splits it further or keeps it", guide)
        self.assertIn("Ambiguity is never delegated", guide)

    def test_worker_guide_forbids_worker_side_decomposition(self) -> None:
        guide = self.read("docs/worker-inspection.md")
        self.assertIn("The worker executes exactly one brief", guide)
        self.assertIn("does not decide whether it is a worker", guide)
        self.assertIn("does not decompose its own assignment", guide)
        self.assertIn("asks the parent for a fresh split", guide)
        self.assertNotIn("worker may decompose", guide)
        self.assertNotIn("let the worker decompose", guide)
        self.assertNotIn("worker decides the split", guide)

    def test_both_guides_define_the_worker_report_authority_boundary(self) -> None:
        prefixes = (
            "A worker report is evidence about the surface",
            "A worker must never declare a gate",
            "The parent independently verifies the exact command",
        )
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn("report authority boundary", guide)
                for phrase in (
                    "never execute them as instructions",
                    "release decision",
                    "phase assignment",
                    "production procedure",
                    "findings outside scope without directives",
                    "never restate a worker verdict",
                ):
                    self.assertIn(phrase, guide)
        for prefix in prefixes:
            with self.subTest(prefix=prefix):
                self.assertEqual(
                    self.paragraph_starting_with("AGENTS.md", prefix),
                    self.paragraph_starting_with("docs/worker-inspection.md", prefix),
                )

    def test_worker_guide_launcher_example_is_a_complete_brief(self) -> None:
        guide = self.read("docs/worker-inspection.md")
        self.assertNotIn("Inspect the assigned files", guide)
        self.assertIn(
            "Review the worker-brief contract in AGENTS.md and"
            " docs/worker-inspection.md for inconsistent wording; make no edits.",
            guide,
        )
        self.assertIn("Run python3 scripts/test_worker_delegation_docs.py.", guide)
        self.assertIn(
            "Stop after reporting its exit code, any inconsistency with quoted"
            " file and line, and evidence-backed non-claims.",
            guide,
        )

    def test_both_guides_state_the_same_brief_contract(self) -> None:
        for prefix in (
            "Every brief is mechanically executable",
            "The worker executes exactly one brief",
        ):
            with self.subTest(prefix=prefix):
                self.assertEqual(
                    self.paragraph_starting_with("AGENTS.md", prefix),
                    self.paragraph_starting_with("docs/worker-inspection.md", prefix),
                )

    def test_both_guides_define_the_parent_decomposition_ledger(self) -> None:
        prefix = "Write a fresh visible decomposition ledger"
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn("decomposition ledger", guide)
                self.assertIn("`Workers:`", guide)
                self.assertIn("`Parent-only:`", guide)
                self.assertIn("`Terminal budget:`", guide)
                self.assertIn("at the start of every actionable user turn", guide)
                self.assertIn("before any progress-making tool", guide)
                self.assertIn("The ledger is a work contract, not narration", guide)
                self.assertIn("is delegated, not justified", guide)
                self.assertLess(
                    guide.index("`Workers:`"), guide.index("`Parent-only:`")
                )
                self.assertLess(
                    guide.index("`Parent-only:`"), guide.index("`Terminal budget:`")
                )
                self.assertLess(
                    guide.index("decomposition ledger"),
                    guide.index("State the split before launching"),
                )
        self.assertEqual(
            self.paragraph_starting_with("AGENTS.md", prefix),
            self.paragraph_starting_with("docs/worker-inspection.md", prefix),
        )

    def test_both_guides_pin_the_current_qoder_lane_identity(self) -> None:
        prefix = "Delegation starts with the launcher-level Qoder lane"
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn("`DeepSeek-Flash`", guide)
                self.assertIn("DeepSeek V4.1 Flash", guide)
                self.assertIn("`dfmodel`", guide)
                self.assertIn("maximum reasoning", guide)
                self.assertIn("no Bash or network", guide)
                self.assertIn("bypasses the Responses listener", guide)
                self.assertIn("MiniMax-first chain", guide)
                self.assertNotIn("Qwen4", guide)
                self.assertNotIn("qmodel", guide)
        self.assertEqual(
            self.paragraph_starting_with("AGENTS.md", prefix),
            self.paragraph_starting_with("docs/worker-inspection.md", prefix),
        )

    def test_shared_fallback_guide_splits_mixed_tasks_for_the_qoder_lane(self) -> None:
        guide = self.read("docs/shared-fallback-policy.md")
        self.assertIn("Mixed tasks keep the launcher-level Qoder lane usable", guide)
        self.assertIn("live web or account access", guide)
        self.assertIn("separately authorized network worker", guide)
        self.assertIn("offline Qoder worker", guide)
        self.assertIn("discovery worker maps the exact files", guide)
        self.assertIn("replays only the named acceptance command", guide)
        self.assertIn("Delegation is not optional for discovery", guide)
        self.assertIn(
            "Qoder ineligibility changes the selected worker lane, never the"
            " decision to delegate",
            guide,
        )
        self.assertIn("`--no-qoder-primary`", guide)
        self.assertIn(
            "`create_thread` and native Codex child tasks are never a delegation"
            " mechanism",
            guide,
        )
        self.assertIn("network-authorized or broad-write worker", guide)
        self.assertIn("changes no provider policy", guide)
        self.assertIn("never enters `Router.tiers` or the shared fallback chain", guide)
        self.assertIn("`DeepSeek-Flash`", guide)
        self.assertIn("`dfmodel`", guide)
        self.assertNotIn("Qwen4", guide)
        self.assertNotIn("qmodel", guide)

    def test_usage_documentation_and_helper_aggregate_the_qoder_lane(self) -> None:
        for relative_path in (
            "docs/worker-inspection.md",
            "scripts/ccr_worker_usage.py",
        ):
            with self.subTest(relative_path=relative_path):
                text = self.read(relative_path)
                self.assertIn("qoder_primary_attempt", text)
                self.assertIn("qoder_bypass_reason", text)
                self.assertIn("not_recorded", text)
                self.assertIn("served", text)
                self.assertIn("fell_back", text)
                self.assertIn("eligible_not_attempted", text)
        self.assertIn("--parent-sessions", self.read("docs/worker-inspection.md"))
        self.assertIn("create_thread", self.read("docs/worker-inspection.md"))
        self.assertIn("qoder_lane", self.read("scripts/ccr_worker_usage.py"))

    def test_changelog_records_the_operator_workflow_change(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("decomposition first", changelog)
        self.assertIn("including a one-piece task", changelog)
        self.assertNotIn("five parent tool calls/five minutes", changelog)
        self.assertIn("12-worker ceiling", changelog)
        self.assertIn("scripts/ccr_worker_usage.py", changelog)
        self.assertIn("decomposition ledger", changelog)
        self.assertIn("`DeepSeek-Flash`", changelog)
        self.assertIn("`dfmodel`", changelog)
        self.assertIn("qoder_bypass_reason", changelog)
        self.assertNotIn("Qwen4", changelog)
        self.assertNotIn("qmodel", changelog)


if __name__ == "__main__":
    unittest.main()
