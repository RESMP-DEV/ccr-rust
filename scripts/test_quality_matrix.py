#!/usr/bin/env python3
"""Tests for the quality dispatcher."""

from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest import mock

import quality


class QualityMatrixTests(unittest.TestCase):
    def test_fast_mode_targets_property_invariants(self) -> None:
        checks = quality.selected_checks(fast=True, coverage=False, mutation=False)
        self.assertEqual(len(checks), 1)
        self.assertEqual(checks[0].name, "property-invariants")
        self.assertIn("property_invariants", checks[0].command)

    def test_default_matrix_covers_all_offline_boundaries(self) -> None:
        names = [
            check.name
            for check in quality.selected_checks(
                fast=False, coverage=False, mutation=False
            )
        ]
        self.assertEqual(
            names,
            [
                "format",
                "clippy",
                "rust-tests",
                "no-default-features",
                "python-helpers",
                "vendored-gp-routing",
            ],
        )

    def test_optional_modes_replace_or_add_expensive_checks(self) -> None:
        coverage_names = [
            check.name
            for check in quality.selected_checks(
                fast=False, coverage=True, mutation=False
            )
        ]
        mutation_names = [
            check.name
            for check in quality.selected_checks(
                fast=False, coverage=True, mutation=True
            )
        ]
        self.assertIn("llvm-coverage", coverage_names)
        self.assertNotIn("rust-tests", coverage_names)
        self.assertIn("mutation-testing", mutation_names)

    def test_mutation_can_extend_default_matrix_without_coverage(self) -> None:
        checks = quality.selected_checks(fast=False, coverage=False, mutation=True)
        names = [check.name for check in checks]
        self.assertIn("rust-tests", names)
        self.assertIn("mutation-testing", names)
        self.assertNotIn("llvm-coverage", names)

    def test_coverage_and_mutation_flags_compose(self) -> None:
        arguments = quality.parse_args(["--coverage", "--mutation"])
        self.assertTrue(arguments.coverage)
        self.assertTrue(arguments.mutation)

    def test_fast_remains_exclusive_with_coverage(self) -> None:
        with self.assertRaises(SystemExit):
            quality.parse_args(["--fast", "--coverage"])

    def test_selecting_coverage_for_listing_has_no_filesystem_side_effect(self) -> None:
        with TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with mock.patch.object(quality, "ROOT", root):
                quality.selected_checks(fast=False, coverage=True, mutation=False)
            self.assertFalse((root / "build" / "reports" / "coverage").exists())

    def test_status_paths_handles_all_change_and_rename_shapes(self) -> None:
        status = "M  staged.txt\0 M unstaged.txt\0?? new.txt\0R  new.txt\0old.txt\0"
        self.assertEqual(
            quality.status_paths(status),
            ["staged.txt", "unstaged.txt", "new.txt", "old.txt -> new.txt"],
        )

    def test_dirty_files_preserves_leading_porcelain_status_column(self) -> None:
        response = SimpleNamespace(stdout=" M .gitignore\0", stderr="", returncode=0)
        with mock.patch.object(quality.subprocess, "run", return_value=response):
            self.assertEqual(quality.dirty_files(), [".gitignore"])

    def test_prepare_coverage_report_removes_only_stale_html_index(self) -> None:
        with TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            index = root / "build" / "reports" / "coverage" / "html" / "index.html"
            index.parent.mkdir(parents=True)
            index.write_text("old report", encoding="utf-8")

            with mock.patch.object(quality, "ROOT", root):
                quality.prepare_coverage_report()

            self.assertTrue(index.parent.exists())
            self.assertFalse(index.exists())

    def test_coverage_summary_parses_totals_by_label(self) -> None:
        html = (
            "<tr><td>Filename</td><td>Function Coverage</td>"
            "<td>Line Coverage</td><td>Region Coverage</td></tr>"
            "<tr class='light-row-bold'><td><pre>Totals</pre></td>"
            "<td><pre>77.00% (7/10)</pre></td>"
            "<td><pre>78.00% (78/100)</pre></td>"
            "<td><pre>79.00% (79/100)</pre></td></tr>"
        )
        self.assertEqual(
            quality.parse_coverage_summary(html),
            {
                "functions": {"percent": 77.0, "covered": 7, "coverable": 10},
                "lines": {"percent": 78.0, "covered": 78, "coverable": 100},
                "regions": {"percent": 79.0, "covered": 79, "coverable": 100},
            },
        )

    def test_unreadable_coverage_summary_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "unreadable"):
            quality.parse_coverage_summary("<html></html>")

    def test_coverage_check_removes_new_root_profiles(self) -> None:
        with TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            report_directory = root / "reports"
            report_directory.mkdir()
            stale_profile = root / "default_stale.profraw"
            stale_profile.write_text("existing", encoding="utf-8")

            def create_profile(command, cwd, env, stdout, stderr, text, check):
                (root / "default_new.profraw").write_text("new", encoding="utf-8")
                return SimpleNamespace(returncode=0)

            check = quality.Check("llvm-coverage", ("true",), cwd=root)
            with (
                mock.patch.object(quality, "ROOT", root),
                mock.patch.object(quality, "REPORT_DIR", report_directory),
                mock.patch.object(
                    quality.subprocess, "run", side_effect=create_profile
                ),
            ):
                result = quality.run_check(check)

            self.assertEqual(result["exit_code"], 0)
            self.assertTrue(stale_profile.exists())
            self.assertFalse((root / "default_new.profraw").exists())

    def test_run_check_records_start_failure_and_continues_to_receipt(self) -> None:
        with TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            report_directory = root / "reports"
            report_directory.mkdir()
            check = quality.Check("blocked", ("/no/such/executable",), cwd=root)
            with (
                mock.patch.object(quality, "ROOT", root),
                mock.patch.object(quality, "REPORT_DIR", report_directory),
                mock.patch.object(
                    quality.subprocess,
                    "run",
                    side_effect=PermissionError("not executable"),
                ),
            ):
                result = quality.run_check(check)

            self.assertEqual(result["exit_code"], 127)
            self.assertIn(
                "could not run command",
                (report_directory / "blocked.log").read_text(encoding="utf-8"),
            )


if __name__ == "__main__":
    unittest.main()
