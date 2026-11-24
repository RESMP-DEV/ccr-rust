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
        self.assertIn("enumerate its pieces before touching the work", guide)
        self.assertIn("reading only what the briefs need", guide)
        self.assertIn("Send every piece", guide)
        self.assertIn("a one-piece task is still delegated", guide)
        self.assertIn("own scope, owned files or surface, acceptance check", guide)
        self.assertIn(
            "Path, symbol, and entry-point discovery is a bounded read-only"
            " worker brief, not parent exploration",
            guide,
        )
        self.assertIn(
            "Source audit, patch authoring, test running, log and evidence"
            " collection, and drafting findings are worker work",
            guide,
        )
        self.assertIn("Parent-only work is a closed list", guide)
        self.assertIn("parent-harness-only capability", guide)
        self.assertIn("final accept/reject/commit/merge", guide)
        self.assertIn("cross-piece conflict resolution", guide)
        self.assertNotIn("tools the worker infrastructure cannot reach", guide)
        self.assertIn("State the split before launching", guide)
        self.assertIn("Launch the workers before doing parent work", guide)
        self.assertIn("send independent pieces concurrently", guide)
        self.assertIn("do not ask permission to launch a worker", guide)
        self.assertIn("Do not collapse several delegable pieces", guide)
        self.assertNotIn("five parent tool calls or five minutes", guide)
        self.assertNotIn("Delegate by default", guide)
        self.assertIn("effort already spent are not the criterion", guide)
        self.assertIn("ccr-rust workers list --limit 5", guide)

    def test_both_guides_ban_native_codex_child_tasks(self) -> None:
        prefix = "`create_thread` and native Codex child tasks"
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn(prefix, guide)
                self.assertIn("never a delegation mechanism", guide)
                self.assertIn(
                    "`Workers:` means a `codex-ccr-worker` run or the named"
                    " judgment tier",
                    guide,
                )
        self.assertEqual(
            self.paragraph_starting_with("AGENTS.md", prefix),
            self.paragraph_starting_with("docs/worker-inspection.md", prefix),
        )

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

    def test_both_guides_define_the_judgment_tier(self) -> None:
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn("Judgment-tier sub-agents", guide)
                self.assertIn("Gemini 4 Argon", guide)
                self.assertIn("Antigravity CLI", guide)
                self.assertIn("not a bulk work tier", guide)
                self.assertIn("never enters `Router.tiers`", guide)
                self.assertIn("shared fallback policy", guide)
                self.assertIn("gemini-4-argon-high", guide)
                self.assertIn("the parent keeps the decision", guide)
                self.assertIn("Gemini 3.8 Flash", guide)
                self.assertIn("code-review bot", guide)
        self.assertEqual(
            self.paragraph_starting_with(
                "AGENTS.md", "Gemini 4 Argon through the Antigravity CLI"
            ),
            self.paragraph_starting_with(
                "docs/worker-inspection.md",
                "Gemini 4 Argon through the Antigravity CLI",
            ),
        )

    def test_both_guides_define_the_qoder_primary_lane(self) -> None:
        prefix = "On the maintainer's Mac, fresh offline worker briefs"
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn(prefix, guide)
                self.assertIn("Qoder CLI `DeepSeek-Flash`", guide)
                self.assertIn("`dfmodel`, DeepSeek V4.1 Flash", guide)
                self.assertIn("maximum reasoning", guide)
                self.assertIn("no synthetic attempt-count budget", guide)
                self.assertIn("offline read-only resumes", guide)
                self.assertIn("Read-only briefs expose only Read/Glob/Grep", guide)
                self.assertIn("`--owned-path`", guide)
                self.assertIn("isolated git worktree", guide)
                self.assertIn("only the declared paths are applied back", guide)
                self.assertIn("out-of-scope changes are rejected", guide)
                self.assertIn("bypasses the Responses listener", guide)
                self.assertIn("MiniMax-first CCR chain", guide)
                self.assertIn("never enters `Router.tiers`", guide)
                self.assertIn("never parent execution", guide)
                self.assertIn(
                    "Qoder ineligibility changes the selected worker lane,"
                    " never the decision to delegate",
                    guide,
                )
                self.assertIn("command-requiring verify or repair briefs pass", guide)
                self.assertIn("`--no-qoder-primary`", guide)
                self.assertIn(
                    "offline source audits and scoped patch generation use Qoder first",
                    guide,
                )
        self.assertEqual(
            self.paragraph_starting_with("AGENTS.md", prefix),
            self.paragraph_starting_with("docs/worker-inspection.md", prefix),
        )

    def test_both_guides_define_the_decomposition_ledger(self) -> None:
        prefixes = (
            "Write a fresh visible decomposition ledger",
            "Every parent terminal action maps",
        )
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                guide = self.read(relative_path)
                self.assertIn("decomposition ledger", guide)
                self.assertIn("at the start of every actionable user turn", guide)
                self.assertIn("before any progress-making tool", guide)
                self.assertIn("`Workers:`", guide)
                self.assertIn("`Parent-only:`", guide)
                self.assertIn("`Terminal budget:`", guide)
                self.assertIn("the closed list of live conversation", guide)
                self.assertIn("parent-harness-only capability", guide)
                self.assertIn("final accept/reject/commit/merge", guide)
                self.assertIn("cross-piece conflict resolution", guide)
                self.assertIn("acceptance evidence", guide)
                self.assertIn("finish line", guide)
                self.assertIn("work contract, not narration", guide)
                self.assertIn("may not silently take over a delegated piece", guide)
                self.assertIn(
                    "record the consolidated result in the project receipt", guide
                )
                self.assertNotIn("the narrow live-conversation", guide)
                self.assertNotIn("state the concrete parent-only reason", guide)
                self.assertNotIn("add the missing worker brief or state", guide)
        for prefix in prefixes:
            with self.subTest(prefix=prefix):
                self.assertEqual(
                    self.paragraph_starting_with("AGENTS.md", prefix),
                    self.paragraph_starting_with("docs/worker-inspection.md", prefix),
                )

    def test_decomposition_ledger_precedes_launcher_details(self) -> None:
        launcher_prefix = "On the maintainer's Mac, fresh offline worker briefs"
        for relative_path in ("AGENTS.md", "docs/worker-inspection.md"):
            with self.subTest(relative_path=relative_path):
                text = (ROOT / relative_path).read_text(encoding="utf-8")
                self.assertLess(
                    text.index("decomposition ledger"), text.index(launcher_prefix)
                )

    def test_guides_and_policy_name_the_deepseek_qoder_identity(self) -> None:
        for relative_path in (
            "AGENTS.md",
            "docs/worker-inspection.md",
            "docs/shared-fallback-policy.md",
        ):
            with self.subTest(relative_path=relative_path):
                text = self.read(relative_path)
                self.assertIn("DeepSeek-Flash", text)
                self.assertIn("`dfmodel`", text)
                self.assertIn("DeepSeek V4.1 Flash", text)
                self.assertNotIn("Qwen4-Max-Preview", text)
                self.assertNotIn("qmodel_latest", text)

    def test_policy_explains_mixed_task_splitting(self) -> None:
        policy = self.read("docs/shared-fallback-policy.md")
        self.assertIn("mixed task", policy)
        self.assertIn("three-line decomposition ledger", policy)
        self.assertIn("`Workers:`", policy)
        self.assertIn("`Parent-only:`", policy)
        self.assertIn("`Terminal budget:`", policy)
        self.assertIn(
            "The parent or a separately authorized network worker fetches"
            " the primary evidence",
            policy,
        )
        self.assertIn("offline Qoder worker", policy)
        self.assertIn("exact task-owned paths", policy)
        self.assertIn(
            "parent runs the acceptance command and inspects the diff", policy
        )
        self.assertIn(
            "Path, symbol, and entry-point discovery is a bounded read-only"
            " worker brief, not parent exploration",
            policy,
        )
        self.assertIn(
            "source audit, patch authoring, test running, log and evidence"
            " collection, and drafting findings are worker work",
            policy,
        )
        self.assertIn(
            "Every actionable user turn starts a fresh"
            " `Workers:`/`Parent-only:`/`Terminal budget:` ledger",
            policy,
        )
        self.assertIn(
            "Qoder ineligibility changes the selected worker lane, never"
            " the decision to delegate",
            policy,
        )
        self.assertIn("`--no-qoder-primary`", policy)
        self.assertIn(
            "`create_thread` and native Codex child tasks are never a"
            " delegation mechanism",
            policy,
        )

    def test_worker_guide_requires_bounded_briefs_and_low_context_inspection(
        self,
    ) -> None:
        guide = self.read("docs/worker-inspection.md")
        self.assertIn("by decomposing before exploring", guide)
        self.assertIn("reading only what the briefs need", guide)
        self.assertIn("send every piece", guide)
        self.assertIn("A one-piece task is still delegated", guide)
        self.assertIn(
            "Path, symbol, and entry-point discovery is a bounded read-only"
            " worker brief, not parent exploration",
            guide,
        )
        self.assertIn(
            "Source audit, patch authoring, test running, log and evidence"
            " collection, and drafting findings are worker work",
            guide,
        )
        self.assertIn("Parent-only work is a closed list", guide)
        self.assertIn("cross-piece conflict resolution", guide)
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

    def test_changelog_records_the_operator_workflow_change(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("decomposition first", changelog)
        self.assertIn("including a one-piece task", changelog)
        self.assertNotIn("five parent tool calls/five minutes", changelog)
        self.assertIn("12-worker ceiling", changelog)
        self.assertIn("scripts/ccr_worker_usage.py", changelog)

    def test_changelog_records_the_judgment_tier(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("Gemini 4 Argon", changelog)
        self.assertIn("judgment tier", changelog)
        self.assertIn("codex-ccr-worker", changelog)
        self.assertIn("Gemini 3.8 Flash", changelog)

    def test_changelog_records_the_decomposition_ledger(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("decomposition ledger", changelog)
        self.assertIn("`Workers:`", changelog)
        self.assertIn("`Parent-only:`", changelog)
        self.assertIn("`Terminal budget:`", changelog)

    def test_changelog_records_the_closed_delegation_contract(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("Closed the parent-only escape hatches", changelog)
        self.assertIn("bounded read-only worker brief", changelog)
        self.assertIn("`Parent-only:` is an explicit closed list", changelog)
        self.assertIn("`create_thread`", changelog)
        self.assertIn("`--no-qoder-primary`", changelog)
        self.assertIn(
            "offline source audits and scoped patch generation use Qoder first",
            changelog,
        )

    def test_changelog_and_policy_docs_record_the_qoder_lane(self) -> None:
        changelog = self.read("CHANGELOG.md")
        self.assertIn("launcher-level Qoder primary lane", changelog)
        self.assertIn("DeepSeek-Flash", changelog)
        self.assertIn("`dfmodel`", changelog)
        self.assertIn("offline read-only resumes", changelog)
        self.assertIn("maximum reasoning", changelog)
        self.assertIn("no synthetic attempt-count budget", changelog)
        self.assertIn("MiniMax-first", changelog)
        self.assertIn("outside shared fallback policy", changelog)
        self.assertIn("isolated git worktree", changelog)
        self.assertIn("`--owned-path`", changelog)

        policy = self.read("docs/shared-fallback-policy.md")
        self.assertIn("Qoder lane before this HTTP chain", policy)
        self.assertIn("no `Router.tiers` entry", policy)
        self.assertIn("owned by `ccr-worker-launcher`", policy)
        self.assertIn("isolated git worktree", policy)
        self.assertIn("only the declared paths are applied back", policy)

        local = self.read("docs/local-operation.md")
        self.assertIn("launcher-level Qoder lane", local)
        self.assertIn("every eligible task", local)
        self.assertIn("scoped writes with declared", local)


if __name__ == "__main__":
    unittest.main()
