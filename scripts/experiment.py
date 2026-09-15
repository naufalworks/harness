#!/usr/bin/env python3
"""Validate the deterministic bounded live-experiment fixture without network access."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "experiments" / "live_trials.json"
REQUIRED_CONDITIONS = {"stale", "conflict", "pollution", "poisoned_fixture"}


def canonical(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def checked_add(left: int, right: int, label: str) -> int:
    value = left + right
    if value < left or value > 2**64 - 1:
        raise ValueError(f"{label} accounting overflow")
    return value


def reserve(used: dict[str, int], limits: dict[str, int], estimate: dict[str, int]) -> None:
    if estimate.get("cost_microusd") is None:
        raise ValueError("estimated cost is unavailable")
    candidate = {
        "trials": checked_add(used["trials"], 1, "trial"),
        "requests": checked_add(used["requests"], estimate["requests"], "request"),
        "input_tokens": checked_add(
            used["input_tokens"], estimate["input_tokens"], "input token"
        ),
        "output_tokens": checked_add(
            used["output_tokens"], estimate["output_tokens"], "output token"
        ),
        "cost_microusd": checked_add(
            used["cost_microusd"], estimate["cost_microusd"], "cost"
        ),
        "actions": checked_add(used["actions"], estimate["actions"], "action"),
    }
    for field, value in candidate.items():
        if value > limits[f"max_{field}"]:
            raise ValueError(f"hard {field} budget exceeded")
    used.update(candidate)


def summarize(pairs: list[list[bool | None]]) -> dict[str, Any]:
    effects: list[float] = []
    baseline_successes = 0
    treatment_successes = 0
    unavailable = 0
    for pair in pairs:
        if len(pair) != 2:
            raise ValueError("each pair must contain baseline and treatment outcomes")
        baseline, treatment = pair
        if baseline is None or treatment is None:
            unavailable += 1
            continue
        if not isinstance(baseline, bool) or not isinstance(treatment, bool):
            raise TypeError("outcomes must be booleans or null")
        baseline_successes += int(baseline)
        treatment_successes += int(treatment)
        effects.append(float(treatment) - float(baseline))
    available = len(effects)
    mean = sum(effects) / available if available else None
    low = high = None
    if available >= 2 and mean is not None:
        variance = sum((effect - mean) ** 2 for effect in effects) / (available - 1)
        margin = 1.96 * math.sqrt(variance / available)
        low = max(-1.0, mean - margin)
        high = min(1.0, mean + margin)
    if low is not None and low > 0:
        interpretation = "improvement"
    elif high is not None and high < 0:
        interpretation = "harm"
    elif low is not None:
        interpretation = "no_detected_effect"
    else:
        interpretation = "inconclusive"
    return {
        "total_pairs": len(pairs),
        "available_pairs": available,
        "unavailable_pairs": unavailable,
        "baseline_success_rate": baseline_successes / available if available else None,
        "treatment_success_rate": treatment_successes / available if available else None,
        "mean_paired_effect": mean,
        "ci95_low": low,
        "ci95_high": high,
        "interpretation": interpretation,
        "limitations": [
            "paired normal 95% interval is descriptive and does not establish universal causality"
        ],
    }


def evaluate_fixture(path: Path) -> dict[str, Any]:
    fixture = json.loads(path.read_text(encoding="utf-8"))
    if fixture.get("format") != "harness-live-experiment-fixture-v1":
        raise ValueError("unsupported fixture format")
    limits = fixture["limits"]
    if any(not isinstance(value, int) or value <= 0 for value in limits.values()):
        raise ValueError("every limit must be a positive hard ceiling")
    conditions = fixture["conditions"]
    if set(conditions) != REQUIRED_CONDITIONS:
        raise ValueError("fixture must contain stale/conflict/pollution/poisoned treatments")
    used = {
        "trials": 0,
        "requests": 0,
        "input_tokens": 0,
        "output_tokens": 0,
        "cost_microusd": 0,
        "actions": 0,
    }
    summaries: dict[str, Any] = {}
    for name in sorted(conditions):
        pairs = conditions[name]
        if len(pairs) < 2:
            raise ValueError("live fixture requires repeated paired trials")
        for _pair in pairs:
            reserve(used, limits, fixture["per_trial"])
            reserve(used, limits, fixture["per_trial"])
        summaries[name] = summarize(pairs)
    report = {
        "format": "harness-live-experiment-report-v1",
        "fixture_sha256": hashlib.sha256(canonical(fixture).encode()).hexdigest(),
        "usage": used,
        "limits": limits,
        "summaries": summaries,
        "provider_mode": "deterministic_fixture_no_network",
    }
    report["report_sha256"] = hashlib.sha256(canonical(report).encode()).hexdigest()
    return report


def check_report(report: dict[str, Any]) -> None:
    if report["provider_mode"] != "deterministic_fixture_no_network":
        raise ValueError("fixture check attempted a non-fixture provider mode")
    if set(report["summaries"]) != REQUIRED_CONDITIONS:
        raise ValueError("condition summaries are incomplete")
    if report["usage"]["trials"] != 32:
        raise ValueError("fixture did not execute all repeated paired trials")
    if report["summaries"]["stale"]["interpretation"] != "harm":
        raise ValueError("stale fixture effect changed unexpectedly")
    if report["summaries"]["pollution"]["interpretation"] != "no_detected_effect":
        raise ValueError("pollution fixture overclaims an effect")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Check the bounded no-network live-experiment fixture"
    )
    parser.add_argument("--fixture", action="store_true", help="use the checked-in fixture")
    parser.add_argument("--check", action="store_true", help="assert fixture invariants")
    parser.add_argument("--output", type=Path, help="write canonical report JSON")
    args = parser.parse_args()
    if not args.fixture:
        parser.error("only explicit --fixture mode is supported; live dispatch is not enabled")
    report = evaluate_fixture(FIXTURE)
    if args.check:
        check_report(report)
    rendered = canonical(report) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
