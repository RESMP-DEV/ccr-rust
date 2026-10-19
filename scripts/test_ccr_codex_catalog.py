import copy
import json
import tempfile
import unittest
from pathlib import Path

import ccr_codex_catalog as catalog


class CodexCatalogTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.path = self.root / "catalog.json"
        self.source = {
            "slug": "glm-5.3",
            "id": "glm-5.3",
            "model": "glm-5.3",
            "displayName": "GLM 5.3",
            "display_name": "GLM 5.3",
            "description": "source",
            "context_window": 1048576,
            "supports_parallel_tool_calls": True,
        }
        self.write_catalog([self.source])

    def write_catalog(self, models: list[dict]) -> None:
        self.path.write_text(json.dumps({"models": models}))

    def test_sync_adds_routes_and_removes_retired_entries(self) -> None:
        retired = [
            {**self.source, "slug": slug, "id": slug} for slug in catalog.RETIRED_SLUGS
        ]
        self.write_catalog([self.source, *retired])
        result = catalog.catalog_sync(
            self.path,
            apply=True,
            snapshot_root=self.root / "snapshots",
            take_snapshot=False,
        )
        document = json.loads(self.path.read_text())
        slugs = [entry["slug"] for entry in document["models"]]
        self.assertIn("ccr-primary", slugs)
        self.assertIn("minimax,MiniMax-M3.1-Flash", slugs)
        self.assertIn("qwen,qwen3.8-flash", slugs)
        self.assertIn("qwen,qwen3.8-max", slugs)
        self.assertIn("qwen,deepseek-v4.1-flash-1017", slugs)
        self.assertIn("qwen,deepseek-v4.1-flash", slugs)
        self.assertNotIn("glm-5.3-flashx", slugs)
        self.assertNotIn("MiniMax-M3.1-Flash-Preview", slugs)
        self.assertNotIn("minimax,MiniMax-M3.1-Flash-Preview", slugs)
        self.assertTrue(result["ok"])

    def test_check_reports_drift_without_writing(self) -> None:
        before = self.path.read_text()
        result = catalog.catalog_sync(self.path, apply=False)
        after = self.path.read_text()
        self.assertFalse(result["ok"])
        self.assertTrue(result["changes"])
        self.assertEqual(before, after)

    def test_second_run_is_stable(self) -> None:
        first = catalog.catalog_sync(self.path, apply=True, take_snapshot=False)
        second = catalog.catalog_sync(self.path, apply=False)
        self.assertTrue(first["ok"])
        self.assertTrue(second["ok"])
        self.assertEqual(second["changes"], [])

    def test_missing_source_is_an_error(self) -> None:
        self.write_catalog([])
        result = catalog.catalog_sync(self.path, apply=True, take_snapshot=False)
        self.assertFalse(result["ok"])
        self.assertIn("source model metadata is absent", result["errors"][0])

    def test_duplicate_slug_is_rejected(self) -> None:
        duplicate = copy.deepcopy(self.source)
        duplicate["slug"] = catalog.ENTRIES[0]["slug"]
        self.write_catalog([self.source, duplicate, dict(duplicate)])
        result = catalog.catalog_sync(self.path, apply=True, take_snapshot=False)
        self.assertFalse(result["ok"])
        self.assertIn("duplicate catalog slug: ccr-primary", result["errors"][0])

    def test_malformed_model_entry_is_a_structured_error(self) -> None:
        self.write_catalog([self.source, "not-an-object"])
        result = catalog.catalog_sync(self.path, apply=True, take_snapshot=False)
        self.assertFalse(result["ok"])
        self.assertIn("every catalog model entry", result["errors"][0])


if __name__ == "__main__":
    unittest.main()
