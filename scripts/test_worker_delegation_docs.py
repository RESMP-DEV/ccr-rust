#!/usr/bin/env python3
"""Tests for the worker-delegation instruction and operator guide."""

from __future__ import annotations

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class WorkerDelegationDocumentationTests(unittest.TestCase):
    def read(self, relative_path: str) -> str:
        return " ".join((ROOT / relative_path).read_text(encoding="utf-8").split())

    def test_canonical_client_guide_is_a_symlink_to_agents(self) -> None:
        client_guide = ROOT / "CLAUDE.md"
        agents = ROOT / "AGENTS.md"
        self.assertTrue(client_guide.is_symlink())
        self.assertEqual(client_guide.resolve(), agents.resolve())

    def test_repository_guide_defines_an_actionable_delegation_trigger(self) -> None:
        guide = self.read("AGENTS.md")
        self.assertIn("### Delegation-first triage", guide)
        self.assertIn("enumerate its pieces before touching the work", guide)
        self.assertIn("reading only what the briefs need", guide)
        self.assertIn("Send every piece", guide)
        self.assertIn("a one-piece task is still delegated", guide)
        self.assertIn("tools the worker infrastructure cannot reach", guide)
        self.assertIn("own scope, owned files or surface, acceptance check", guide)
        self.assertIn("State the split before launching", guide)
        self.assertIn("Launch the workers before doing parent work", guide)
        self.assertIn("Do not collapse several delegable pieces", guide)
        self.assertIn("effort already spent are not the criterion", guide)
        self.assertIn("final integration", guide)
        self.assertIn("ccr-rust workers list --limit 5", guide)

    def test_worker_guide_requires_bounded_briefs_and_low_context_inspection(
        self,
    ) -> None:
        guide = self.read("docs/worker-inspection.md")
        self.assertIn("by decomposing before exploring", guide)
        self.assertIn("reading only what the briefs need", guide)
        self.assertIn("send every piece", guide)
        self.assertIn("A one-piece task is still delegated", guide)
        self.assertIn("tools the worker infrastructure cannot reach", guide)
        self.assertIn("State the split before launching", guide)
        self.assertIn("send independent pieces concurrently", guide)
        self.assertIn("Do not collapse several delegable pieces", guide)
        self.assertIn(
            "Do not use size, file count, elapsed time, or effort already spent as the trigger",
            guide,
        )
        self.assertIn("one bounded outcome", guide)
        self.assertIn("<report>", guide)
        self.assertIn("separate foreground tool sessions", guide)
        self.assertIn("ccr-worker-usage --days 7", guide)
        self.assertIn("Evidence, not automatic approval", guide)

    def test_changelog_records_the_operator_workflow_change(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("bounded worker delegation", changelog)
        self.assertIn("12-worker ceiling", changelog)
        self.assertIn("scripts/ccr_worker_usage.py", changelog)


if __name__ == "__main__":
    unittest.main()
