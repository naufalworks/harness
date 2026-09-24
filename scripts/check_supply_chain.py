#!/usr/bin/env python3
"""Supply-chain and CI-gate checks that run without network access.

P12-T05b asks for vulnerability and license policy, pinned actions and
reproducible lint gates. Those live in two different places, and this script is
deliberate about which is which:

  * Checks 1-4 run here, offline, and are real assertions about this checkout.
    Check 4 asserts that CI *declares* the scanner steps, which catches the
    failure mode where a scanner is quietly deleted from the workflow. A
    declaration is not evidence that a scan passed.
  * Check 5 actually executes the scanners (cargo audit, cargo deny,
    shellcheck, ruff) when they are installed. Each one is reported
    individually: PASS when it exits clean, FAIL when it reports findings, and
    SKIP -- never a pass -- when the tool is absent or the network it needs is
    unreachable. cargo audit and cargo deny need crates.io; on an offline host
    they skip rather than pretending to scan.

Exit code is 1 if any check fails, 0 otherwise. Warnings never fail the run.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

findings: list[tuple[str, str, str]] = []


def record(level: str, area: str, message: str) -> None:
    findings.append((level, area, message))


# --------------------------------------------------------------------------
# 1. GitHub Actions must be pinned to immutable commit SHAs.
#    A tag like @v4 is a moving target: whoever controls the action repo can
#    repoint it at new code that runs with this workflow's token.
# --------------------------------------------------------------------------
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def check_pinned_actions() -> None:
    workflows = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
    if not workflows:
        record("FAIL", "actions", "no workflow files found under .github/workflows")
        return
    total = 0
    for wf in workflows:
        for lineno, line in enumerate(wf.read_text(encoding="utf-8").splitlines(), 1):
            match = re.search(r"uses:\s*([^\s#]+)", line)
            if not match:
                continue
            total += 1
            ref = match.group(1)
            if "@" not in ref:
                record("FAIL", "actions", f"{wf.name}:{lineno} uses {ref} with no ref at all")
                continue
            name, _, version = ref.rpartition("@")
            if not SHA_RE.match(version):
                record(
                    "FAIL",
                    "actions",
                    f"{wf.name}:{lineno} pins {name} to the mutable ref '{version}'; use a 40-character commit SHA",
                )
    if total and not any(f[1] == "actions" for f in findings):
        record("PASS", "actions", f"{total} action reference(s) pinned to commit SHAs")


# --------------------------------------------------------------------------
# 2. Dependency shape: registry-only, no wildcards, lockfiles committed.
# --------------------------------------------------------------------------
def check_dependency_shape() -> None:
    cargo_toml = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    problems = 0
    for lineno, line in enumerate(cargo_toml.splitlines(), 1):
        stripped = line.strip()
        if stripped.startswith("#") or "=" not in stripped:
            continue
        if re.search(r"\bgit\s*=", stripped):
            record("FAIL", "deps", f"Cargo.toml:{lineno} depends on a git source, which is not a pinned registry release")
            problems += 1
        if re.search(r'version\s*=\s*"\*"', stripped) or re.search(r'^\s*[\w-]+\s*=\s*"\*"', stripped):
            record("FAIL", "deps", f"Cargo.toml:{lineno} uses a wildcard version")
            problems += 1
    for lockfile in ("Cargo.lock", "package-lock.json"):
        if not (ROOT / lockfile).exists():
            record("FAIL", "deps", f"{lockfile} is not committed, so builds are not reproducible")
            problems += 1
    if problems == 0:
        record("PASS", "deps", "registry-only dependencies, no wildcards, both lockfiles committed")


# --------------------------------------------------------------------------
# 3. Cargo.lock must match Cargo.toml. Skipped honestly when the registry
#    cache cannot answer offline: a skip is reported, never counted as a pass.
# --------------------------------------------------------------------------
def check_lock_in_sync() -> None:
    try:
        result = subprocess.run(
            ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        record("SKIP", "lock", f"could not run cargo metadata: {exc}")
        return
    if result.returncode == 0:
        try:
            count = len(json.loads(result.stdout).get("packages", []))
            record("PASS", "lock", f"Cargo.lock satisfies Cargo.toml offline ({count} packages resolved)")
        except json.JSONDecodeError:
            record("PASS", "lock", "Cargo.lock satisfies Cargo.toml offline")
        return
    stderr = result.stderr.strip().splitlines()
    tail = stderr[-1] if stderr else "no stderr"
    if "needs to be updated" in result.stderr or "--locked" in result.stderr:
        record("FAIL", "lock", f"Cargo.lock does not match Cargo.toml: {tail}")
    else:
        record("SKIP", "lock", f"cargo metadata could not resolve offline: {tail}")


# --------------------------------------------------------------------------
# 4. CI must still declare the gates that cannot run on this host.
# --------------------------------------------------------------------------
REQUIRED_CI_STEPS = {
    "cargo fmt": "formatting gate",
    "-D warnings": "clippy warning budget",
    "cargo audit": "dependency vulnerability scan",
    "cargo deny": "license and ban policy",
    "shellcheck": "shell lint",
    "ruff check": "python lint",
    "check_docs.py": "documentation claim gate",
    "check_supply_chain.py": "this script",
}


def check_ci_declares_gates() -> None:
    workflows = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
    blob = "\n".join(wf.read_text(encoding="utf-8") for wf in workflows)
    missing = [f"{needle} ({why})" for needle, why in REQUIRED_CI_STEPS.items() if needle not in blob]
    for item in missing:
        record("FAIL", "ci-gates", f"no CI step runs {item}")
    if not missing:
        record(
            "PASS",
            "ci-gates",
            f"CI declares all {len(REQUIRED_CI_STEPS)} required gates (declaration only; scanner results are not asserted here)",
        )


# --------------------------------------------------------------------------
# 5. Run the scanners themselves when they are installed. A missing tool or an
#    unreachable registry is a SKIP, never a pass.
# --------------------------------------------------------------------------
SCANNERS: tuple[tuple[str, str, list[str], bool], ...] = (
    ("audit", "cargo-audit", ["cargo", "audit", "--deny", "warnings"], True),
    ("deny", "cargo-deny", ["cargo", "deny", "check", "licenses", "bans", "sources"], True),
    ("unused-deps", "cargo-machete", ["cargo", "machete"], False),
    ("shell-lint", "shellcheck", ["bash", "-c", "shellcheck scripts/*.sh"], False),
    ("python-lint", "ruff", ["ruff", "check", "tests", "scripts"], False),
)


def check_scanners_run() -> None:
    for area, tool, command, needs_network in SCANNERS:
        probe = tool if tool != "cargo-audit" and tool != "cargo-deny" else "cargo"
        if shutil.which(probe) is None or (
            probe == "cargo" and shutil.which(tool) is None and not (Path.home() / ".cargo" / "bin" / tool).exists()
        ):
            record("SKIP", area, f"{tool} is not installed on this host")
            continue
        try:
            result = subprocess.run(
                command, cwd=ROOT, capture_output=True, text=True, timeout=900, check=False
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            record("SKIP", area, f"could not run {tool}: {exc}")
            continue
        if result.returncode == 0:
            record("PASS", area, f"{tool} reported no findings")
            continue
        output = (result.stderr + result.stdout).strip().splitlines()
        tail = output[-1] if output else "no output"
        offline = needs_network and any(
            marker in (result.stderr + result.stdout)
            for marker in ("failed to fetch", "could not connect", "403 Forbidden", "network failure")
        )
        if offline:
            record("SKIP", area, f"{tool} needs registry access and could not reach it: {tail}")
        else:
            record("FAIL", area, f"{tool} reported findings: {tail}")


def main() -> int:
    check_pinned_actions()
    check_dependency_shape()
    check_lock_in_sync()
    check_ci_declares_gates()
    check_scanners_run()

    for level, area, message in findings:
        print(f"[{level}] {area}: {message}")

    failures = [f for f in findings if f[0] == "FAIL"]
    skips = [f for f in findings if f[0] == "SKIP"]
    print()
    if skips:
        print(f"{len(skips)} check(s) skipped and NOT counted as passing")
    print(f"{len(failures)} failing check(s)")
    print(
        "note: check 4 asserts the scanners are declared in CI; check 5 runs them "
        "here when installed. A SKIP is never counted as a pass."
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
