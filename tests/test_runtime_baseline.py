#!/usr/bin/env python3
"""P16-T05 deterministic runtime-baseline unit and real-binary tests."""
from __future__ import annotations

import importlib.util
import json
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("runtime_baseline", ROOT / "scripts/runtime_baseline.py")
assert SPEC and SPEC.loader
runtime_baseline = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runtime_baseline)


class RuntimeBaselineTests(unittest.TestCase):
    def test_distribution_uses_lower_median_and_nearest_rank_p95(self) -> None:
        self.assertEqual(
            runtime_baseline.distribution([100, 10, 40, 30, 20]),
            {"sample_count": 5, "median_ms": 30, "p95_ms": 100, "max_ms": 100},
        )

    def test_summary_is_bounded_and_selects_largest_median(self) -> None:
        events = []
        for provider in (20, 21, 22, 23, 24):
            durations = {name: 1 for name in runtime_baseline.STAGES}
            durations.update(total_ms=40, provider_ms=provider, verification_ms=10)
            events.append({
                "schema": runtime_baseline.RUNTIME_SCHEMA,
                "event": "turn_runtime_finished",
                "outcome": "complete",
                "durations": durations,
                "counts": {"provider_calls": 2, "tool_calls": 0},
                "unavailable": ["sqlite_queue_ms"],
            })
        report = runtime_baseline.summarize(
            events, {"scenario": "short_chat"}, "a" * 40, 1, 2, 0.5, 100, {"python": "test"}
        )
        self.assertEqual(report["largest_measured_stage"], "provider_ms")
        self.assertEqual(report["stages"]["provider_ms"]["sample_count"], 5)
        self.assertEqual(report["counts"]["provider_calls"]["median"], 2)
        serialized = json.dumps(report).lower()
        for forbidden in runtime_baseline.FORBIDDEN_FIELDS:
            self.assertNotIn(f'"{forbidden}"', serialized)

    def test_real_binary_emits_one_report_per_synthetic_turn(self) -> None:
        binary = ROOT / "target/debug/harness"
        if not binary.is_file():
            subprocess.run(["cargo", "build", "--locked"], cwd=ROOT, check=True)
        report = runtime_baseline.run(5, 5, binary)
        self.assertEqual(report["sample_count"], 5)
        self.assertEqual(report["largest_measured_stage"], "provider_ms")
        self.assertEqual(report["errors"], 0)
        self.assertEqual(report["timeouts"], 0)
        self.assertEqual(report["unavailable"], [
            "sqlite_commit_ms", "sqlite_queue_ms", "sqlite_read_ms", "sqlite_write_ms"
        ])


if __name__ == "__main__":
    unittest.main()
