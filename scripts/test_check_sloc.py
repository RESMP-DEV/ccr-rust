from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest import mock

import check_sloc


class SlocRatchetTests(unittest.TestCase):
    def test_current_tree_is_within_the_ratchet(self) -> None:
        result = check_sloc.check()
        self.assertTrue(result["ok"], result["errors"])
        self.assertLessEqual(result["total"], result["total_ceiling"])

    def test_hotspot_file_and_total_growth_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "router").mkdir(parents=True)
            (root / "router/mod.rs").write_text("one\ntwo\n", encoding="utf-8")
            with mock.patch.object(
                check_sloc, "HOTSPOT_CEILINGS", {"router/mod.rs": 1}
            ):
                result = check_sloc.check(root)
        self.assertFalse(result["ok"])
        self.assertIn(
            "router/mod.rs has 2 nonblank lines; ceiling is 1", result["errors"]
        )

    def test_any_large_new_file_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            large = root / "new.rs"
            large.write_text("x\n" * 2_501, encoding="utf-8")
            result = check_sloc.check(root)
        self.assertFalse(result["ok"])
        self.assertIn(
            "new.rs has 2501 nonblank lines; ceiling is 2500", result["errors"]
        )


if __name__ == "__main__":
    unittest.main()
