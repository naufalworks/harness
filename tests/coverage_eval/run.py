#!/usr/bin/env python3
"""Causal coverage metrics gate for P16-T03.

This script does not reimplement coverage. The metrics are computed in Rust
(`DbStore::causal_coverage`, over the single `incident_view` projection), and a
Python copy of that arithmetic would measure the copy rather than the shipped
code — the same reasoning `tests/recall_eval/run.py` records for P15-T01. The
Rust test `storage::tests::causal_coverage_metrics_meet_declared_budgets` runs
the real coverage path over `fixtures.json` and writes `metrics.json`; this
script validates those metrics against the budgets declared in the fixtures.

That is why the task's verify command is ordered:

    cargo test --locked coverage && python3 tests/coverage_eval/run.py --check

The first command produces the evidence; the second refuses to accept it if it
is missing, stale relative to the fixtures, incomplete, internally inconsistent,
or outside a declared budget.

What this gate deliberately does NOT check: that coverage is high. A low
coverage number is a true measurement of a sparsely-recorded run, and failing on
it would pressure a future change into labelling proximity as dependency to make
the gate pass. What it checks instead is that the measurement is honest: that the
evidence classes partition the node set, that no proximity row was counted as
covered, that an unrecorded break is reported as unknown, and that the numbers
describe these exact fixtures.

Usage:
  run.py            Print the measured report.
  run.py --check    Print the report and exit non-zero if any check fails.
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
# The projection identity the metrics must have been measured against. Read out of the Rust
# source so a projection change cannot be silent, exactly as the recall gate reads MODEL out of
# src/embeddings.rs.
PROVENANCE = ROOT / "src" / "storage" / "provenance.rs"


def fail(problems: list[str]) -> None:
    for problem in problems:
        print(f"FAIL: {problem}")
    print(f"FAIL: {len(problems)} causal coverage problem(s)")
    sys.exit(1)


def declared_projection() -> str | None:
    """Read PROJECTION out of src/storage/provenance.rs."""
    if not PROVENANCE.exists():
        return None
    for line in PROVENANCE.read_text(encoding="utf-8").splitlines():
        if line.startswith("pub const PROJECTION"):
            return line.split('"')[1]
    return None


def main() -> None:
    parser = argparse.ArgumentParser(description="Validate measured causal coverage.")
    parser.add_argument("--check", action="store_true", help="exit non-zero when a check fails")
    args = parser.parse_args()

    if not FIXTURES.exists():
        print(f"FAIL: fixtures are missing at {FIXTURES}")
        sys.exit(1)
    fixture_bytes = FIXTURES.read_bytes()
    fixtures = json.loads(fixture_bytes)
    fixture_digest = hashlib.sha256(fixture_bytes).hexdigest()
    budgets = fixtures["budgets"]
    case_ids = [case["id"] for case in fixtures["cases"]]

    if not METRICS.exists():
        # Never pass silently when the evidence was never produced: that is the failure mode
        # this gate exists to prevent.
        print(f"FAIL: {METRICS.name} is missing; run `cargo test --locked coverage` first to measure it")
        sys.exit(1)
    metrics = json.loads(METRICS.read_text(encoding="utf-8"))

    problems: list[str] = []

    # 1. The metrics must describe these exact fixtures, not an older set.
    if metrics.get("fixture_sha256") != fixture_digest:
        problems.append(
            "metrics.json was measured against different fixtures "
            f"({str(metrics.get('fixture_sha256', 'absent'))[:12]} != {fixture_digest[:12]}); "
            "re-run the Rust test"
        )

    # 2. The projection identity must match what the binary ships. A coverage figure measured
    #    against a different projection describes a different graph.
    shipped = declared_projection()
    if shipped and metrics.get("projection") != shipped:
        problems.append(
            f"metrics projection {metrics.get('projection')!r} does not match "
            f"src/storage/provenance.rs {shipped!r}"
        )
    if fixtures.get("projection") != metrics.get("projection"):
        problems.append(
            f"fixtures expect projection {fixtures.get('projection')!r} but metrics report "
            f"{metrics.get('projection')!r}"
        )

    # 3. Every declared case must have been measured, and no extra case invented.
    measured = {case["id"]: case for case in metrics.get("cases", [])}
    for case_id in case_ids:
        if case_id not in measured:
            problems.append(f"case {case_id!r} was never measured")
    for case_id in measured:
        if case_id not in case_ids:
            problems.append(f"case {case_id!r} is in metrics but not in fixtures")

    report_rows = []
    for case in fixtures["cases"]:
        case_id = case["id"]
        got = measured.get(case_id)
        if not got:
            continue
        report_rows.append(got)

        nodes = got.get("nodes", 0)
        recorded = got.get("recorded_dependency", 0)
        proximity = got.get("temporal_proximity", 0)
        unknown = got.get("unknown", 0)

        # 4. The evidence classes must partition the node set. If they do not, some row escaped
        #    classification and every ratio below it is meaningless.
        if recorded + proximity + unknown != nodes:
            problems.append(
                f"case {case_id!r} evidence classes do not partition its nodes: "
                f"{recorded}+{proximity}+{unknown} != {nodes}"
            )

        # 5. missing_edges must be exactly the non-recorded remainder. This is the check that
        #    makes counting a proximity row as covered impossible to ship quietly.
        if got.get("missing_edges") != nodes - recorded:
            problems.append(
                f"case {case_id!r} reports {got.get('missing_edges')} missing edge(s) but "
                f"{nodes - recorded} node(s) carry no recorded edge; proximity must never count "
                "as coverage"
            )

        # 6. The declared expectations for this fixture must hold.
        if got.get("break_recorded") != case["expect_break_recorded"]:
            problems.append(
                f"case {case_id!r} break_recorded is {got.get('break_recorded')} but the fixture "
                f"declares {case['expect_break_recorded']}"
            )
        if not case["expect_break_recorded"] and got.get("break_kind") != "unknown":
            problems.append(
                f"case {case_id!r} has no recorded break but reports break_kind "
                f"{got.get('break_kind')!r} instead of 'unknown'"
            )
        if got.get("recorded_dependency", -1) != case["expect_recorded_dependency"]:
            problems.append(
                f"case {case_id!r} recorded {got.get('recorded_dependency')} dependency node(s) "
                f"but the fixture declares {case['expect_recorded_dependency']}"
            )
        if got.get("temporal_proximity", -1) != case["expect_temporal_proximity"]:
            problems.append(
                f"case {case_id!r} recorded {got.get('temporal_proximity')} proximity node(s) "
                f"but the fixture declares {case['expect_temporal_proximity']}"
            )

        # 7. Graph size must stay inside the projection's own bound: a coverage report over an
        #    unbounded graph would not be the graph a reviewer can open.
        if nodes > budgets["max_graph_nodes"]:
            problems.append(
                f"case {case_id!r} graph has {nodes} nodes, above the {budgets['max_graph_nodes']} bound"
            )
        coverage = got.get("edge_coverage")
        if coverage is not None and not 0.0 <= coverage <= 1.0:
            problems.append(f"case {case_id!r} edge_coverage {coverage} is not a ratio")
        if nodes == 0 and coverage is not None:
            problems.append(
                f"case {case_id!r} reports a coverage ratio over zero nodes; that would be a "
                "false reassurance rather than a measurement"
            )

    # 8. Reviewer time must be measured, never assumed.
    review = metrics.get("reviewer_time", {})
    if review.get("closed_reviews", 0) < budgets["min_measured_reviews"]:
        problems.append(
            f"only {review.get('closed_reviews', 0)} closed review(s) were measured, below the "
            f"declared minimum {budgets['min_measured_reviews']}"
        )
    if review.get("closed_reviews", 0) == 0 and review.get("mean_ms") is not None:
        problems.append("reviewer mean_ms is reported with no closed review to measure it from")
    if review.get("closed_reviews", 0) > 0 and review.get("mean_ms") is None:
        problems.append("closed reviews were measured but no mean duration was reported")

    # 9. Deployment provenance must be a recorded chain with resolvable parents, and anomaly
    #    flags must keep null ("never asked") distinct from false ("asked, answered no").
    deployment = metrics.get("deployment", {})
    phases = deployment.get("phases", [])
    if len(phases) < budgets["min_deployment_phases"]:
        problems.append(
            f"only {len(phases)} deployment phase(s) recorded, below the declared minimum "
            f"{budgets['min_deployment_phases']}"
        )
    ids = {phase.get("id") for phase in phases}
    for phase in phases:
        parent = phase.get("parent_id")
        if parent is not None and parent not in ids:
            problems.append(
                f"deployment phase {phase.get('id')!r} names parent {parent!r}, which is not a "
                "recorded phase; a trail must not reference a phase that does not exist"
            )
        if phase.get("parent_id") == phase.get("id"):
            problems.append(f"deployment phase {phase.get('id')!r} is its own parent")
        for flag, value in (phase.get("anomalies") or {}).items():
            if value not in (True, False, None):
                problems.append(
                    f"deployment phase {phase.get('id')!r} anomaly {flag!r} is {value!r}; only "
                    "true, false and null (never asked) are meaningful"
                )
    if deployment.get("anomalies_flagged", 0) != sum(
        1
        for phase in phases
        if any(value is True for value in (phase.get("anomalies") or {}).values())
    ):
        problems.append("deployment anomalies_flagged does not match the phases carrying a true flag")

    print(f"causal coverage · projection {metrics.get('projection')} · {len(report_rows)} case(s)")
    print(f"{'case':34} {'nodes':>6} {'dep':>5} {'prox':>5} {'unk':>5} {'miss':>5} {'cover':>6} {'break':>6}")
    for case in report_rows:
        cover = case.get("edge_coverage")
        print(
            f"{case['id'][:34]:34} {case.get('nodes', 0):6} {case.get('recorded_dependency', 0):5} "
            f"{case.get('temporal_proximity', 0):5} {case.get('unknown', 0):5} "
            f"{case.get('missing_edges', 0):5} {('n/a' if cover is None else f'{cover:.3f}'):>6} "
            f"{case.get('break_recorded')!s:>6}"
        )
    print(
        f"reviewer time: closed={review.get('closed_reviews', 0)} "
        f"open={review.get('open_reviews', 0)} abandoned={review.get('abandoned_reviews', 0)} "
        f"mean_ms={review.get('mean_ms')}"
    )
    print(
        f"deployment provenance: {len(phases)} phase(s), "
        f"{deployment.get('anomalies_flagged', 0)} anomaly flag(s), "
        f"{deployment.get('unresolved_or_unknown', 0)} unresolved/unknown"
    )

    if problems:
        if args.check:
            fail(problems)
        for problem in problems:
            print(f"WARN: {problem}")
        return
    print(
        "PASS: evidence classes partition every graph, no proximity row is counted as coverage, "
        "an unrecorded break reports unknown, reviewer time is measured, and the deployment trail "
        "resolves every parent it names"
    )


if __name__ == "__main__":
    main()
