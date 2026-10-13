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

    def test_status_paths_handles_all_change_and_rename_shapes(self) -> None:
        status = "M  staged.txt\0 M unstaged.txt\0?? new.txt\0R  new.txt\0old.txt\0"
        self.assertEqual(
            quality.status_paths(status),
            ["staged.txt", "unstaged.txt", "new.txt", "old.txt -> new.txt"],
        )

    def test_dirty_files_preserves_leading_porcelain_status_column(self) -> None:
        response = SimpleNamespace(stdout=" M .gitignore\0", returncode=0)
        with mock.patch.object(quality.subprocess, "run", return_value=response):
            self.assertEqual(quality.dirty_files(), [".gitignore"])

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


if __name__ == "__main__":
    unittest.main()
