"""P21-T01: fail closed when route-to-UI coverage becomes stale or misleading."""

from __future__ import annotations

import json
import re
import sys
import unittest
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from check_docs import router_operations

MATRIX = ROOT / "docs" / "design" / "p21-ui-coverage.json"
INDEX = ROOT / "static" / "index.html"
APP = ROOT / "static" / "app.js"
API_CLIENT = ROOT / "static" / "api.js"
OPENAPI = ROOT / "docs" / "api.yaml"


def openapi_operations() -> set[tuple[str, str]]:
    """Match the documented OpenAPI path/method indentation without a YAML dependency."""
    operations: set[tuple[str, str]] = set()
    current: str | None = None
    for line in OPENAPI.read_text(encoding="utf-8").splitlines():
        path = re.fullmatch(r"  (/[^:]*):\s*", line)
        if path:
            current = path.group(1)
            continue
        if current is None:
            continue
        method = re.fullmatch(r"    (get|post|put|patch|delete):\s*", line)
        if method:
            operations.add((method.group(1).upper(), current))
        elif line and not line.startswith(" "):
            current = None
    return operations


class UiCoverageContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.matrix = json.loads(MATRIX.read_text(encoding="utf-8"))
        cls.index = INDEX.read_text(encoding="utf-8")
        cls.frontend = APP.read_text(encoding="utf-8") + API_CLIENT.read_text(encoding="utf-8")

    def test_every_router_and_openapi_operation_is_classified_once(self) -> None:
        self.assertEqual(self.matrix["schema"], "harness.ui-coverage/v1")
        listed = [
            tuple(operation.split(" ", 1))
            for group in self.matrix["groups"]
            for operation in group["operations"]
        ]
        duplicates = [item for item, count in Counter(listed).items() if count > 1]
        self.assertFalse(duplicates, f"duplicate UI classifications: {duplicates}")
        self.assertEqual(set(listed), router_operations(), "UI coverage misses/newly invents routes")
        self.assertEqual(openapi_operations(), router_operations(), "OpenAPI and router drift")

    def test_group_classifications_are_truthful_and_auditable(self) -> None:
        allowed = {"ui_supported", "ui_partial", "api_only"}
        for group in self.matrix["groups"]:
            with self.subTest(surface=group["surface"]):
                self.assertIn(group["status"], allowed)
                self.assertGreater(len(group["detail"]), 35, "API-only and partial coverage require explanation")
                self.assertTrue(group["operations"])
                if group["status"] == "api_only":
                    self.assertIsNone(group["anchor"], "API-only routes must not pretend to have a UI control")
                else:
                    self.assertIn(
                        f'id="{group["anchor"]}"',
                        self.index,
                        "UI surface marker no longer exists",
                    )

    def test_configuration_coverage_matches_current_ui(self) -> None:
        groups = self.matrix["groups"]
        classified = {
            op: group
            for group in groups
            for op in group["operations"]
        }
        for op in ("GET /models", "GET /config", "POST /config"):
            with self.subTest(operation=op):
                self.assertEqual(classified[op]["status"], "ui_supported")
                self.assertEqual(classified[op]["anchor"], "settingsform")
        for op in ("GET /scopes/{scope}", "POST /scopes/{scope}"):
            with self.subTest(operation=op):
                self.assertEqual(classified[op]["status"], "ui_partial")
        self.assertIn("id=\"loadmodels\"", self.index)
        self.assertIn("id=\"provider-model-options\"", self.index)
        self.assertIn("id=\"mainmodel-origin\"", self.index)
        self.assertIn("id=\"rootpath\"", self.index)
        self.assertIn("id=\"p21-capabilities\"", self.index)
        self.assertIn("Not yet available in the UI", self.index)
        self.assertIn("model's private reasoning", self.index)

    def test_provider_management_is_ui_supported(self) -> None:
        classified = {
            op: group
            for group in self.matrix["groups"]
            for op in group["operations"]
        }
        for op in (
            "GET /providers",
            "POST /providers",
            "DELETE /providers/{id}",
            "POST /providers/{id}/select",
            "POST /providers/{id}/test",
        ):
            with self.subTest(operation=op):
                self.assertEqual(classified[op]["status"], "ui_supported")
                self.assertEqual(classified[op]["anchor"], "provider-list")
        for marker in ("provider-list", "providerform", "providerkey", "providerpaste"):
            self.assertIn(f'id="{marker}"', self.index)
        self.assertIn("write-only", self.index)
        self.assertIn("clearProviderEditor", self.frontend)
        self.assertIn("parseProviderPaste", self.frontend)

    def test_no_provider_credentials_in_matrix_or_UI_inventory(self) -> None:
        text = MATRIX.read_text(encoding="utf-8")
        self.assertNotRegex(text, r"sk-[A-Za-z0-9_-]{12,}")
        self.assertNotRegex(text, r"ak_[A-Za-z0-9_-]{12,}")
        self.assertNotIn("HARNESS_API_KEY=", text)


if __name__ == "__main__":
    unittest.main()
