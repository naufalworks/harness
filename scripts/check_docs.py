#!/usr/bin/env python3
"""Fail when documentation or deployment claims drift from the code.

P12-T07b. This checker only asserts things that can be mechanically compared
against the code, the task ledger or the running binary. Prose is deliberately
left alone: a documentation gate that flags style becomes noise, and a noisy
gate gets disabled, which is worse than no gate.

Checks
    1. routes      - the router and the HTTP surface inventory in
                   docs/ARCHITECTURE.md agree exactly, in both directions.
  2. verify-cmds - every `verify:` command in docs/TASKS.md refers to scripts
                   that exist. A task cannot be honestly `done` if its own
                   verification command cannot run.
  3. ledger      - task statuses are from the known set, every `depends:` id
                   exists, and no task is `done` while a dependency is not.
  4. counts      - the living docs (README/AGENTS/ARCHITECTURE) carry no
                   hand-written test counts. Those change every commit. The
                   dated journals are exempt: a journal entry is evidence about
                   one run, not a claim about now.
  5. deployment  - if the server is reachable, /health commit must match HEAD,
                   or differ only by commits that touch documentation.

Exit status is 0 only when no check FAILs. Unreachable server or missing
credentials SKIPs check 5 rather than failing it: absence of evidence is not
evidence of drift.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROUTES_RS = ROOT / "src" / "api" / "routes.rs"
TASKS = ROOT / "docs" / "TASKS.md"
# Documentation that describes the system as it is now. Journals are excluded
# on purpose; they record what was true on a date.
LIVING_DOCS = [
    ROOT / "README.md",
    ROOT / "AGENTS.md",
    ROOT / "docs" / "ARCHITECTURE.md",
]

KNOWN_STATUSES = {"todo", "doing", "done", "blocked"}

results: list[tuple[str, str, str]] = []


def record(level: str, check: str, message: str) -> None:
    results.append((level, check, message))


def git(*args: str) -> str:
    out = subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=False
    )
    return out.stdout.strip()


# --------------------------------------------------------------------------
# 1. routes
# --------------------------------------------------------------------------
def router_routes() -> set[str]:
    src = ROUTES_RS.read_text(encoding="utf-8")
    start = src.find("pub(crate) fn router")
    if start < 0:
        record("FAIL", "routes", f"no router function found in {ROUTES_RS}")
        return set()
    body = src[start:]
    # .route("/path", ...) including the multi-line form where the path sits on
    # its own line after the opening paren.
    return set(re.findall(r"\.route\(\s*\"([^\"]+)\"", body))


def inventory_routes() -> set[str]:
    """Routes listed in the HTTP surface inventory in docs/ARCHITECTURE.md.

    An explicit inventory is compared exactly against the router, in both
    directions. README prose is deliberately not used as the source of truth:
    it is a quickstart, and treating every omission as an error produces the
    kind of noise that gets a gate switched off.
    """
    doc = ROOT / "docs" / "ARCHITECTURE.md"
    if not doc.exists():
        record("FAIL", "routes", "docs/ARCHITECTURE.md is missing")
        return set()
    text = doc.read_text(encoding="utf-8")
    match = re.search(r"^## HTTP surface$(.*?)(?=^## |\Z)", text, re.S | re.M)
    if not match:
        record(
            "FAIL",
            "routes",
            "docs/ARCHITECTURE.md has no '## HTTP surface' inventory section",
        )
        return set()
    return set(
        re.findall(r"`(?:GET|POST|PUT|DELETE|PATCH)(?:\|(?:GET|POST|PUT|DELETE|PATCH))* ([^`]+)`", match.group(1))
    )


def check_routes() -> None:
    actual = router_routes()
    if not actual:
        return
    listed = inventory_routes()
    if not listed:
        return

    missing = sorted(actual - listed)
    for path in missing:
        record(
            "FAIL",
            "routes",
            f"router serves {path}, absent from the HTTP surface inventory",
        )
    ghosts = sorted(listed - actual)
    for path in ghosts:
        record(
            "FAIL",
            "routes",
            f"inventory lists {path}, which the router does not serve",
        )
    if not missing and not ghosts:
        record(
            "PASS",
            "routes",
            f"{len(actual)} router routes match the HTTP surface inventory exactly",
        )


# --------------------------------------------------------------------------
# 2 + 3. task ledger
# --------------------------------------------------------------------------
def parse_tasks() -> dict[str, dict[str, str]]:
    tasks: dict[str, dict[str, str]] = {}
    current: str | None = None
    for line in TASKS.read_text(encoding="utf-8").splitlines():
        heading = re.match(r"^### ([A-Z0-9\-]+[a-z]?)\s*·", line)
        if heading:
            current = heading.group(1)
            tasks[current] = {}
            continue
        if current:
            field = re.match(r"^- (status|depends|verify|priority|lane):\s*(.*)$", line)
            if field and field.group(1) not in tasks[current]:
                tasks[current][field.group(1)] = field.group(2).strip()
    return tasks


def check_verify_commands(tasks: dict[str, dict[str, str]]) -> None:
    missing = 0
    for name, fields in tasks.items():
        verify = fields.get("verify", "")
        for script in re.findall(r"(?:python3?\s+|bash\s+|sh\s+|\./)?(scripts/[A-Za-z0-9_.\-]+)", verify):
            if not (ROOT / script).exists():
                missing += 1
                level = "FAIL" if fields.get("status") == "done" else "WARN"
                record(
                    level,
                    "verify-cmds",
                    f"{name} (status: {fields.get('status', '?')}) verifies with "
                    f"{script}, which does not exist",
                )
    if not missing:
        record("PASS", "verify-cmds", "every verify: command refers to scripts that exist")


def check_ledger(tasks: dict[str, dict[str, str]]) -> None:
    problems = 0
    for name, fields in tasks.items():
        status = fields.get("status")
        if status is None:
            problems += 1
            record("FAIL", "ledger", f"{name} has no status")
            continue
        if status not in KNOWN_STATUSES:
            problems += 1
            record("FAIL", "ledger", f"{name} has unknown status {status!r}")
        deps = [
            d.strip()
            for d in fields.get("depends", "").split(",")
            if d.strip() and d.strip().lower() not in ("none", "-", chr(8212), chr(8211))
        ]
        for dep in deps:
            if dep not in tasks:
                problems += 1
                record("FAIL", "ledger", f"{name} depends on {dep}, which is not a task")
            elif status == "done" and tasks[dep].get("status") != "done":
                problems += 1
                record(
                    "FAIL",
                    "ledger",
                    f"{name} is done but its dependency {dep} is "
                    f"{tasks[dep].get('status', '?')}",
                )
    done = sum(1 for f in tasks.values() if f.get("status") == "done")
    todo = sum(1 for f in tasks.values() if f.get("status") == "todo")
    if not problems:
        record(
            "PASS",
            "ledger",
            f"{len(tasks)} tasks consistent ({done} done, {todo} todo)",
        )


# --------------------------------------------------------------------------
# 4. volatile counts in living docs
# --------------------------------------------------------------------------
def check_counts() -> None:
    pattern = re.compile(r"\b\d{2,4}\s+(?:tests?\s+)?(?:passed|passing|tests)\b")
    hits = 0
    for doc in LIVING_DOCS:
        if not doc.exists():
            continue
        for number, line in enumerate(doc.read_text(encoding="utf-8").splitlines(), 1):
            if pattern.search(line):
                hits += 1
                record(
                    "FAIL",
                    "counts",
                    f"{doc.name}:{number} states a test count, which drifts every "
                    "commit; cite the gate instead of a number",
                )
    if not hits:
        record("PASS", "counts", "living docs assert no hand-written test counts")


# --------------------------------------------------------------------------
# 5. deployment identity
# --------------------------------------------------------------------------
def env_from_dotenv() -> dict[str, str]:
    env: dict[str, str] = {}
    dotenv = ROOT / ".env"
    if dotenv.exists():
        for line in dotenv.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            env[key.strip()] = value.strip()
    env.update({k: v for k, v in os.environ.items() if k.startswith("HARNESS_")})
    return env


def is_docs_only(commit_range: str) -> tuple[bool, list[str]]:
    changed = [p for p in git("diff", "--name-only", commit_range).splitlines() if p]
    code = [
        p
        for p in changed
        if not (p.startswith("docs/") or p.endswith(".md"))
    ]
    return (not code, code)


def check_deployment() -> None:
    env = env_from_dotenv()
    addr = env.get("HARNESS_ADDR")
    token = env.get("HARNESS_AUTH_TOKEN")
    if not addr or not token:
        record("SKIP", "deployment", "no HARNESS_ADDR/HARNESS_AUTH_TOKEN available")
        return
    request = urllib.request.Request(
        "http://" + addr + "/health",
        headers=dict(Authorization="Bearer " + token),
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            health = json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, OSError, json.JSONDecodeError) as exc:
        record("SKIP", "deployment", f"server not reachable for identity check ({exc})")
        return

    live = str(health.get("commit", ""))
    head = git("rev-parse", "HEAD")
    if not live:
        record("FAIL", "deployment", "/health does not report a commit")
        return
    if live == head:
        record("PASS", "deployment", f"live binary matches HEAD ({head[:7]})")
        return
    if not git("cat-file", "-t", live):
        record(
            "FAIL",
            "deployment",
            f"live commit {live[:7]} is not in this repository",
        )
        return
    docs_only, code = is_docs_only(f"{live}..HEAD")
    if docs_only:
        record(
            "PASS",
            "deployment",
            f"live {live[:7]} trails HEAD {head[:7]} by documentation only",
        )
    else:
        record(
            "FAIL",
            "deployment",
            f"live {live[:7]} trails HEAD {head[:7]} and code differs: "
            + ", ".join(sorted(code)[:5]),
        )


def main() -> int:
    check_routes()
    tasks = parse_tasks()
    if not tasks:
        record("FAIL", "ledger", f"no tasks parsed from {TASKS}")
    else:
        check_verify_commands(tasks)
        check_ledger(tasks)
    check_counts()
    check_deployment()

    for level, check, message in results:
        print(f"[{level}] {check}: {message}")
    failures = sum(1 for level, _, _ in results if level == "FAIL")
    print(f"\n{failures} failing check(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
