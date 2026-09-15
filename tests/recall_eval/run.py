#!/usr/bin/env python3
"""Recall evaluation gate for P15-T01.

This script does not reimplement retrieval. Ranking lives in Rust
(`DbStore::recall`), and a Python copy of it would measure the copy rather than
the shipped code. Instead the Rust test
`storage::tests::recall_eval_fixtures_meet_labeled_budgets` runs the real recall
path over `fixtures.json` and writes `metrics.json`; this script validates those
metrics against the thresholds declared in `fixtures.json`.

That is why the task's verify command is ordered:

    cargo test --locked recall && python3 tests/recall_eval/run.py --check

The first command produces the evidence, the second refuses to accept it if it
is missing, stale relative to the fixtures, incomplete, or below budget.

Usage:
  run.py            Print the measured report.
  run.py --check    Print the report and exit non-zero if any budget is missed.
"""
import argparse
import hashlib
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FIXTURES = HERE / "fixtures.json"
METRICS = HERE / "metrics.json"
EMBEDDINGS = ROOT / "src" / "embeddings.rs"


def fail(problems: list[str]) -> None:
    for problem in problems:
        print(f"FAIL: {problem}")
    print(f"FAIL: {len(problems)} recall budget problem(s)")
    sys.exit(1)


def declared_model() -> str | None:
    """Read MODEL out of src/embeddings.rs so a model change cannot be silent."""
    if not EMBEDDINGS.exists():
        return None
    for line in EMBEDDINGS.read_text(encoding="utf-8").splitlines():
        if line.startswith("pub const MODEL"):
            return line.split('"')[1]
    return None


def main() -> None:
    parser = argparse.ArgumentParser(description="Validate measured recall quality.")
    parser.add_argument("--check", action="store_true", help="exit non-zero when a budget is missed")
    args = parser.parse_args()

    if not FIXTURES.exists():
        print(f"FAIL: fixtures are missing at {FIXTURES}")
        sys.exit(1)
    fixture_bytes = FIXTURES.read_bytes()
    fixtures = json.loads(fixture_bytes)
    fixture_digest = hashlib.sha256(fixture_bytes).hexdigest()
    thresholds = fixtures["thresholds"]
    case_ids = [case["id"] for case in fixtures["cases"]]

    if not METRICS.exists():
        # Never pass silently when the evidence was never produced: that is the
        # failure mode this gate exists to prevent.
        print(f"FAIL: {METRICS.name} is missing; run `cargo test --locked recall` first to measure it")
        sys.exit(1)
    metrics = json.loads(METRICS.read_text(encoding="utf-8"))

    problems: list[str] = []

    # 1. The metrics must describe these exact fixtures, not an older set.
    if metrics.get("fixture_sha256") != fixture_digest:
        problems.append(
            "metrics.json was measured against different fixtures "
            f"({metrics.get('fixture_sha256', 'absent')[:12]} != {fixture_digest[:12]}); re-run the Rust test"
        )

    # 2. The embedding model identity must match what the binary ships.
    shipped = declared_model()
    if shipped and metrics.get("model") != shipped:
        problems.append(f"metrics model {metrics.get('model')!r} does not match src/embeddings.rs {shipped!r}")
    if fixtures.get("model") != metrics.get("model"):
        problems.append(
            f"fixtures expect model {fixtures.get('model')!r} but metrics report {metrics.get('model')!r}"
        )

    # 3. Every declared case must have been measured.
    measured = {case["id"]: case for case in metrics.get("cases", [])}
    for case_id in case_ids:
        if case_id not in measured:
            problems.append(f"case {case_id!r} was never measured")
    for case_id in measured:
        if case_id not in case_ids:
            problems.append(f"case {case_id!r} is in metrics but not in fixtures")

    totals = metrics.get("totals", {})
    report_rows = []
    for case_id in case_ids:
        case = measured.get(case_id)
        if not case:
            continue
        report_rows.append(case)
        if case.get("stale_recalled", 0) > 0:
            problems.append(
                f"case {case_id!r} recalled {case['stale_recalled']} archived/stale memory(ies); "
                "archived rows must never reach the context"
            )
        if case.get("context_bytes", 0) > thresholds["max_context_bytes"]:
            problems.append(
                f"case {case_id!r} context cost {case['context_bytes']}B exceeds "
                f"{thresholds['max_context_bytes']}B"
            )
        if case.get("latency_ms", 0) > thresholds["max_case_latency_ms"]:
            problems.append(
                f"case {case_id!r} latency {case['latency_ms']}ms exceeds "
                f"{thresholds['max_case_latency_ms']}ms"
            )

    # 4. Aggregate budgets.
    checks = [
        ("macro_precision", thresholds["min_macro_precision"], "at least"),
        ("relevant_coverage", thresholds["min_relevant_coverage"], "at least"),
    ]
    for name, bound, direction in checks:
        value = totals.get(name)
        if value is None:
            problems.append(f"totals.{name} is missing from metrics.json")
        elif value < bound:
            problems.append(f"{name} {value:.3f} is below the required {direction} {bound:.3f}")
    stale_rate = totals.get("stale_use_rate")
    if stale_rate is None:
        problems.append("totals.stale_use_rate is missing from metrics.json")
    elif stale_rate > thresholds["max_stale_use_rate"]:
        problems.append(
            f"stale_use_rate {stale_rate:.3f} exceeds the allowed {thresholds['max_stale_use_rate']:.3f}"
        )

    print(f"recall eval · model {metrics.get('model')} · {len(report_rows)} case(s)")
    print(f"{'case':28} {'prec':>6} {'rel':>7} {'stale':>6} {'bytes':>7} {'ms':>6}")
    for case in report_rows:
        print(
            f"{case['id'][:28]:28} {case.get('precision', 0):6.2f} "
            f"{case.get('relevant_recalled', 0)}/{case.get('relevant_available', 0):<5} "
            f"{case.get('stale_recalled', 0):6} {case.get('context_bytes', 0):7} {case.get('latency_ms', 0):6}"
        )
    if totals:
        print(
            f"totals: macro_precision={totals.get('macro_precision', 0):.3f} "
            f"relevant_coverage={totals.get('relevant_coverage', 0):.3f} "
            f"stale_use_rate={totals.get('stale_use_rate', 0):.3f} "
            f"max_context_bytes={totals.get('max_context_bytes', 0)} "
            f"max_latency_ms={totals.get('max_latency_ms', 0)}"
        )

    # Report the other arm so the strategy choice is an informed one rather than a default.
    comparison = metrics.get("comparison", {}).get("lexical_only")
    if comparison:
        env = metrics.get("comparison", {}).get("opt_out_env", "")
        print(
            f"measured strategy: {metrics.get('strategy', 'hybrid')} "
            f"(set {env}=0 for lexical only)"
        )
        print(
            f"lexical_only arm: macro_precision={comparison.get('macro_precision', 0):.3f} "
            f"relevant_coverage={comparison.get('relevant_coverage', 0):.3f} "
            f"returned={comparison.get('returned', 0)}"
        )

    if problems:
        if args.check:
            fail(problems)
        for problem in problems:
            print(f"WARN: {problem}")
        return
    print(
        "PASS: recall precision, relevant coverage, stale use, latency and context cost "
        "all within the declared budgets"
    )


if __name__ == "__main__":
    main()
