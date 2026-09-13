#!/usr/bin/env python3
"""Read-only plan/task contract check for the Harness repository.

Validates that ``docs/TASKS.md`` and the planning documents stay internally
consistent so task status and documentation cannot drift silently. It only reads
files: it never writes, never runs a service, and never touches the network.

Usage: ``python3 scripts/check_plan.py [repo_root]``
Exit code 0 when the contract holds, 1 when any problem is found.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

TASK_HEADING = re.compile(r"^### (P\d+-T[0-9]+[a-z]?) \u00b7 ", re.M)
DEP_TOKEN = re.compile(r"P\d+-T[0-9]+[a-z]?")
FIELD = re.compile(r"^- ([a-z][a-z0-9-]*):\s*(.*)$")
SDU_SECTION = re.compile(r"^## Safe-daily-use milestone.*?(?=^## )", re.M | re.S)

OPEN_STATUSES = {"todo", "doing", "needs-verify", "blocked"}

# (relative path, literal, must_contain). A stale phrase must be absent.
DOC_CONTRACT = [
    ("docs/PLAN.md", "tracked as `P7-T06`", False),
    ("docs/PLAN.md", "Two things remain", False),
    ("docs/PLAN.md", "safe-daily-use", True),
    ("AGENTS.md", "needs-verify)", False),
    ("AGENTS.md", "edit/bash/meta pending", False),
    ("AGENTS.md", "Milestone precedence", True),
    ("README.md", "one LLM call per chat", False),
    ("README.md", "multi-step", True),
    ("docs/ROADMAP.md", "Safe-daily-use milestone", True),
    ("docs/TASKS.md", "Milestone precedence", True),
    ("docs/design/memory-wind-tunnel.md", "deterministic-pipeline", True),
    ("docs/design/memory-wind-tunnel.md", "unavailable", True),
]


def parse_tasks(text):
    """Return {task_id: {field: value}} parsed from a TASKS.md document."""
    tasks = {}
    current = None
    for line in text.splitlines():
        heading = TASK_HEADING.match(line)
        if heading:
            current = heading.group(1)
            tasks[current] = {}
            continue
        if line.startswith("## "):
            current = None
            continue
        if current is not None:
            field = FIELD.match(line)
            if field:
                tasks[current].setdefault(field.group(1), field.group(2).strip())
    return tasks


def depends_of(task):
    """Task IDs named in a task's ``depends`` field (``—`` yields none)."""
    return DEP_TOKEN.findall(task.get("depends", ""))


def find_duplicate_ids(text):
    ids = TASK_HEADING.findall(text)
    seen, duplicates = set(), []
    for task_id in ids:
        if task_id in seen:
            duplicates.append(task_id)
        seen.add(task_id)
    return duplicates


def check_dependencies(tasks):
    known = set(tasks)
    return [
        f"{tid}: depends on unknown task {dep}"
        for tid, task in tasks.items()
        for dep in depends_of(task)
        if dep not in known
    ]


def check_no_cycles(tasks, edge_fn, label):
    problems = []
    graph = {tid: [d for d in edge_fn(task) if d in tasks] for tid, task in tasks.items()}
    WHITE, GREY, BLACK = 0, 1, 2
    color = {tid: WHITE for tid in tasks}

    def visit(node, stack):
        color[node] = GREY
        for nxt in graph.get(node, []):
            if color[nxt] == GREY:
                problems.append(f"{label} cycle: {' -> '.join(stack + [node, nxt])}")
            elif color[nxt] == WHITE:
                visit(nxt, stack + [node])
        color[node] = BLACK

    for tid in tasks:
        if color[tid] == WHITE:
            visit(tid, [])
    return problems


def check_children(tasks):
    problems = []
    for tid, task in tasks.items():
        parent = task.get("parent")
        if parent and parent not in tasks:
            problems.append(f"{tid}: parent {parent} does not exist")
    return problems


def check_milestone(text, tasks):
    section = SDU_SECTION.search(text)
    if not section:
        return ["TASKS.md: missing 'Safe-daily-use milestone' section"]
    problems = []
    required = DEP_TOKEN.findall(section.group(0))
    if not required:
        problems.append("SDU section names no required tasks")
    for tid in required:
        if tid not in tasks:
            problems.append(f"SDU requires unknown task {tid}")
        elif tasks[tid].get("status") == "dropped":
            problems.append(f"SDU requires dropped task {tid}")
    return problems


def check_open_verify(tasks):
    return [
        f"{tid}: open task must not require live deploy in verify: {task.get('verify', '')}"
        for tid, task in tasks.items()
        if task.get("status") in OPEN_STATUSES and "scripts/deploy.sh" in task.get("verify", "")
    ]


def check_doc_contract(root):
    problems = []
    for rel, needle, must_contain in DOC_CONTRACT:
        path = root / rel
        if not path.exists():
            problems.append(f"missing document {rel}")
            continue
        present = needle in path.read_text(encoding="utf-8")
        if must_contain and not present:
            problems.append(f"{rel}: expected to contain {needle!r}")
        if not must_contain and present:
            problems.append(f"{rel}: stale phrase still present {needle!r}")
    if not (root / "docs/design/safe-daily-use-scorecard.md").exists():
        problems.append("missing docs/design/safe-daily-use-scorecard.md")
    return problems


def run(root):
    root = Path(root)
    tasks_path = root / "docs/TASKS.md"
    if not tasks_path.exists():
        return [f"missing {tasks_path}"]
    text = tasks_path.read_text(encoding="utf-8")
    tasks = parse_tasks(text)
    problems = [f"duplicate task ID {i}" for i in find_duplicate_ids(text)]
    problems += check_dependencies(tasks)
    problems += check_no_cycles(tasks, depends_of, "depends")
    problems += check_no_cycles(
        tasks, lambda t: [t["parent"]] if t.get("parent") else [], "parent"
    )
    problems += check_children(tasks)
    problems += check_milestone(text, tasks)
    problems += check_open_verify(tasks)
    problems += check_doc_contract(root)
    return problems


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    root = Path(argv[0]).resolve() if argv else Path(__file__).resolve().parent.parent
    problems = run(root)
    for problem in problems:
        print(f"FAIL: {problem}")
    if problems:
        print(f"plan contract: {len(problems)} problem(s)")
        return 1
    print("plan contract: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
