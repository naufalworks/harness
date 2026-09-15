#!/usr/bin/env python3
"""Validate fail-closed remote experiment guardrails without making remote calls."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "experiments" / "remote_runner.json"
PLAN_FORMAT = "harness-remote-run-plan-v1"
FIXTURE_FORMAT = "harness-remote-runner-fixture-v1"
REPORT_FORMAT = "harness-remote-runner-check-v1"
MAX_TTL_SECONDS = 3_600
MAX_SPEND_MICROUSD = 1_000_000
PINNED_FIELDS = ("image", "region", "size")


def canonical(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def digest(value: Any) -> str:
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def require_positive_int(value: Any, label: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 0 < value <= maximum:
        raise ValueError(f"{label} must be a positive bounded integer")
    return value


def validate_plan(plan: dict[str, Any], *, explicit_opt_in: bool) -> dict[str, Any]:
    if plan.get("format") != PLAN_FORMAT:
        raise ValueError("unsupported remote run plan format")
    if not explicit_opt_in:
        raise PermissionError("remote run refused: explicit opt-in is required")
    if plan.get("execution_mode") != "fixture_simulation_no_network":
        raise ValueError("fixture checker refuses every non-fixture execution mode")
    for field in PINNED_FIELDS:
        value = plan.get(field)
        if not isinstance(value, str) or not value or value != value.strip():
            raise ValueError(f"remote {field} must be pinned")
    if "@sha256:" not in plan["image"] or not is_sha256(plan["image"].rsplit(":", 1)[1]):
        raise ValueError("remote image must be pinned by SHA-256 digest")
    ttl = require_positive_int(plan.get("ttl_seconds"), "TTL", MAX_TTL_SECONDS)
    spend = require_positive_int(
        plan.get("spend_cap_microusd"), "spend cap", MAX_SPEND_MICROUSD
    )
    kill_switch = plan.get("kill_switch")
    if not isinstance(kill_switch, dict) or kill_switch.get("enabled") is not True:
        raise ValueError("remote run requires an enabled kill switch")
    command = kill_switch.get("command")
    if not isinstance(command, str) or not command.startswith("fixture://destroy/"):
        raise ValueError("fixture kill switch must be an explicit destroy command")
    require_positive_int(kill_switch.get("timeout_seconds"), "kill-switch timeout", 300)
    source = plan.get("sanitized_input")
    if not isinstance(source, dict) or source.get("sanitizer") != "harness-sanitize-v1":
        raise ValueError("remote run accepts only input marked with the shared sanitizer")
    if not is_sha256(source.get("content_sha256")):
        raise ValueError("sanitized remote input must be content addressed")
    cleanup = plan.get("cleanup")
    if not isinstance(cleanup, dict) or cleanup.get("required") is not True:
        raise ValueError("remote run requires confirmed cleanup")
    if cleanup.get("verify_state") != "destroyed":
        raise ValueError("cleanup must verify the instance is destroyed")
    if not isinstance(cleanup.get("confirmation_field"), str) or not cleanup["confirmation_field"]:
        raise ValueError("cleanup requires a confirmation field")
    return {
        "ttl_seconds": ttl,
        "spend_cap_microusd": spend,
        "pinned": {field: plan[field] for field in PINNED_FIELDS},
        "input_sha256": source["content_sha256"],
        "kill_switch": command,
    }


def expect_refusal(plan: dict[str, Any], explicit_opt_in: bool, expected: str) -> None:
    try:
        validate_plan(plan, explicit_opt_in=explicit_opt_in)
    except (PermissionError, ValueError) as error:
        if expected not in str(error):
            raise AssertionError(f"expected refusal containing {expected!r}, got {error!s}") from error
    else:
        raise AssertionError(f"invalid remote plan was accepted; expected {expected!r}")


def evaluate_fixture(path: Path) -> dict[str, Any]:
    fixture = json.loads(path.read_text(encoding="utf-8"))
    if fixture.get("format") != FIXTURE_FORMAT:
        raise ValueError("unsupported remote runner fixture format")
    plan = fixture.get("plan")
    if not isinstance(plan, dict):
        raise TypeError("fixture plan must be an object")

    expect_refusal(plan, False, "explicit opt-in")
    accepted = validate_plan(plan, explicit_opt_in=True)

    refusal_checks = 1
    mutations: tuple[tuple[str, Any, str], ...] = (
        ("image", "unpinned-image", "SHA-256"),
        ("region", "", "region"),
        ("size", "", "size"),
        ("ttl_seconds", 0, "TTL"),
        ("spend_cap_microusd", None, "spend cap"),
        ("kill_switch", {"enabled": False}, "kill switch"),
        ("sanitized_input", {"sanitizer": "unknown"}, "shared sanitizer"),
        ("cleanup", {"required": False}, "confirmed cleanup"),
    )
    for field, value, expected in mutations:
        changed = copy.deepcopy(plan)
        changed[field] = value
        expect_refusal(changed, True, expected)
        refusal_checks += 1

    lifecycle = ["planned", "fixture_provisioned", "fixture_running", "collecting", "destroyed"]
    cleanup = fixture.get("cleanup_confirmation")
    if not isinstance(cleanup, dict):
        raise TypeError("fixture cleanup confirmation must be an object")
    if cleanup.get("state") != "destroyed" or not cleanup.get("confirmation_id"):
        raise ValueError("fixture cleanup was not confirmed destroyed")
    if cleanup.get("instance_id") != fixture.get("fixture_instance_id"):
        raise ValueError("cleanup confirmation belongs to a different fixture instance")

    report = {
        "format": REPORT_FORMAT,
        "fixture_sha256": digest(fixture),
        "plan_sha256": digest(plan),
        "admission": accepted,
        "lifecycle": lifecycle,
        "cleanup_confirmation": cleanup,
        "refusal_checks": refusal_checks,
        "remote_execution": False,
        "network_calls": 0,
        "provider_calls": 0,
        "cloud_api_calls": 0,
        "note": "Deterministic fixture simulation only; no VM or remote resource was created.",
    }
    report["report_sha256"] = digest(report)
    return report


def check_report(report: dict[str, Any]) -> None:
    if report["remote_execution"] is not False:
        raise ValueError("fixture must not claim a real remote run")
    for field in ("network_calls", "provider_calls", "cloud_api_calls"):
        if report[field] != 0:
            raise ValueError(f"fixture made forbidden {field}")
    if report["lifecycle"][-1] != "destroyed":
        raise ValueError("fixture lifecycle did not end in destroyed state")
    if report["cleanup_confirmation"]["state"] != "destroyed":
        raise ValueError("cleanup destruction is not confirmed")
    if report["refusal_checks"] != 9:
        raise ValueError("not every remote guardrail was tested fail-closed")
    if not is_sha256(report["report_sha256"]):
        raise ValueError("fixture report is not content addressed")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Check remote experiment guardrails in deterministic no-network fixture mode"
    )
    parser.add_argument("--fixture", action="store_true", help="use the checked-in fixture")
    parser.add_argument("--check", action="store_true", help="assert every guardrail")
    parser.add_argument("--output", type=Path, help="write canonical check report JSON")
    args = parser.parse_args()
    if not args.fixture:
        parser.error(
            "remote execution is disabled; only explicit --fixture mode is implemented in this build"
        )
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
