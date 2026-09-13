#!/usr/bin/env python3
"""Read-only contract tests for docs/TASKS.md and the planning documents.

These tests never mutate files, run a service, or touch the network. They unit-test
``scripts/check_plan.py`` against synthetic fixtures and then assert the real
repository still satisfies the plan contract.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO))

from scripts import check_plan  # noqa: E402

VALID = """\
# TASKS
## Safe-daily-use milestone (SDU)

| gate | task |
|---|---|
| gate | P1-T01 |

---
## P1

### P1-T01 · Root
- status: todo
- priority: high
- lane: a
- parallel: no
- depends: P1-T02
- files: x
- done-when: y
- verify: run x

### P1-T02 · Child
- status: todo
- priority: high
- lane: b
- parallel: yes
- depends: —
- files: z
- done-when: y
- verify: run z
"""


class ParseTests(unittest.TestCase):
    def test_parse_collects_ids_and_fields(self):
        tasks = check_plan.parse_tasks(VALID)
        self.assertEqual(set(tasks), {"P1-T01", "P1-T02"})
        self.assertEqual(tasks["P1-T01"]["depends"], "P1-T02")
        self.assertEqual(tasks["P1-T02"]["status"], "todo")

    def test_depends_extraction(self):
        self.assertEqual(
            check_plan.depends_of({"depends": "P1-T01, P2-T03"}), ["P1-T01", "P2-T03"]
        )
        self.assertEqual(check_plan.depends_of({"depends": "—"}), [])


class CheckerTests(unittest.TestCase):
    def test_duplicate_ids(self):
        text = VALID + "\n### P1-T01 · Duplicate\n- status: todo\n"
        self.assertEqual(check_plan.find_duplicate_ids(text), ["P1-T01"])

    def test_unknown_dependency(self):
        tasks = check_plan.parse_tasks(VALID)
        tasks["P1-T01"]["depends"] = "P9-T99"
        problems = check_plan.check_dependencies(tasks)
        self.assertTrue(any("P9-T99" in p for p in problems))

    def test_cycle_detected(self):
        tasks = check_plan.parse_tasks(VALID)
        tasks["P1-T02"]["depends"] = "P1-T01"
        problems = check_plan.check_no_cycles(tasks, check_plan.depends_of, "depends")
        self.assertTrue(any("cycle" in p for p in problems))

    def test_acyclic_ok(self):
        tasks = check_plan.parse_tasks(VALID)
        self.assertEqual(
            check_plan.check_no_cycles(tasks, check_plan.depends_of, "depends"), []
        )

    def test_missing_parent(self):
        tasks = check_plan.parse_tasks(VALID)
        tasks["P1-T02"]["parent"] = "P9-T99"
        problems = check_plan.check_children(tasks)
        self.assertTrue(any("P9-T99" in p for p in problems))

    def test_open_task_live_deploy_flagged(self):
        tasks = check_plan.parse_tasks(VALID)
        tasks["P1-T02"]["verify"] = "bash scripts/deploy.sh"
        problems = check_plan.check_open_verify(tasks)
        self.assertTrue(any("P1-T02" in p for p in problems))

    def test_done_task_live_deploy_exempt(self):
        tasks = check_plan.parse_tasks(VALID)
        tasks["P1-T02"]["status"] = "done"
        tasks["P1-T02"]["verify"] = "bash scripts/deploy.sh"
        self.assertEqual(check_plan.check_open_verify(tasks), [])

    def test_milestone_requires_known_tasks(self):
        text = VALID.replace("P1-T01 |", "P9-T99 |")
        problems = check_plan.check_milestone(text, check_plan.parse_tasks(VALID))
        self.assertTrue(any("P9-T99" in p for p in problems))


class RepoContractTests(unittest.TestCase):
    def test_repository_plan_contract_holds(self):
        problems = check_plan.run(REPO)
        self.assertEqual(problems, [], "\n".join(problems))


if __name__ == "__main__":
    unittest.main()
