"""Offline contract test for src/agentic_sql.rs: every constant is executed against the
migrated schema (001->002->003) with a Python driver mirroring src/agent_loop.rs.
No Rust toolchain needed. Rust/HTTP gates remain separate."""
import json, re, sqlite3, unittest, uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUST = (ROOT / "src/agentic_sql.rs").read_text()
SQL = {m.group(1): m.group(2) for m in re.finditer(r'pub const (\w+): &str = r#"(.*?)"#;', RUST, re.S)}
NOW = "2026-09-09T00:00:00Z"
LATER = "2026-09-09T00:30:00Z"


def connect():
    c = sqlite3.connect(":memory:", isolation_level=None)
    c.execute("PRAGMA foreign_keys=ON")
    for name in ("001_core.sql", "002_recording.sql", "003_agentic.sql"):
        c.executescript((ROOT / "migrations" / name).read_text())
    return c


def seed_turn(c, request="r1", session="s1", scope="proj"):
    c.execute("INSERT INTO sessions(id,scope,created_at) VALUES(?,?,?)", (session, scope, NOW))
    c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,?,'user','hi','pending',?)", (request, session, NOW))
    c.execute(
        "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES(?,?,?,'m','sig',0,'generating',?,?)",
        (request, session, scope, NOW, NOW),
    )


class AgenticSql(unittest.TestCase):
    def test_all_constants_present(self):
        for name in ["SCOPE_GET", "SCOPE_UPSERT", "STEP_BEGIN", "STEP_FINISH", "STEP_NEXT_SEQ", "STEPS_LIST", "EVENT", "EVENTS_AFTER",
                     "PERMISSION_CREATE", "PERMISSION_GET", "PERMISSION_RESOLVE", "PERMISSION_STATUS", "PERMISSIONS_PENDING", "PERMISSION_EXPIRE",
                     "FILE_CHANGE", "FILE_CHANGES_LIST", "PLAN_CLEAR", "PLAN_INSERT", "PLAN_LIST", "SESSION_OF_REQUEST",
                     "RECOVER_STEPS", "RECOVER_PERMISSIONS", "RECOVER_ACTIVITY"]:
            self.assertIn(name, SQL)

    def test_scope_roundtrip(self):
        c = connect()
        c.execute(SQL["SCOPE_UPSERT"], ("proj", "/tmp/x", "ask", None, None, None, None, NOW))
        c.execute(SQL["SCOPE_UPSERT"], ("proj", "/tmp/y", "auto_edit", "cargo check", 50, 100000, 600, LATER))
        row = c.execute(SQL["SCOPE_GET"], ("proj",)).fetchone()
        self.assertEqual(row[:7], ("proj", "/tmp/y", "auto_edit", "cargo check", 50, 100000, 600))
        self.assertEqual((row[7], row[8]), (NOW, LATER))
        with self.assertRaises(sqlite3.IntegrityError):
            c.execute(SQL["SCOPE_UPSERT"], ("proj", None, "yolo", None, None, None, None, NOW))

    def test_step_lifecycle_and_listing(self):
        c = connect(); seed_turn(c)
        self.assertEqual(c.execute(SQL["STEP_NEXT_SEQ"], ("r1",)).fetchone()[0], 0)
        c.execute(SQL["STEP_BEGIN"], ("st0", "r1", 0, "model_call", None, None, json.dumps({"messages": []}), NOW))
        self.assertEqual(c.execute(SQL["STEP_NEXT_SEQ"], ("r1",)).fetchone()[0], 1)
        c.execute(SQL["STEP_BEGIN"], ("st1", "r1", 1, "tool_call", "read", "call_1", json.dumps({"path": "a"}), NOW))
        n = c.execute(SQL["STEP_FINISH"], ("st0", "complete", json.dumps({"tool_calls": 1}), 12, 0, 100, 20, None, LATER)).rowcount
        self.assertEqual(n, 1)
        # finishing twice is a no-op (status guard)
        self.assertEqual(c.execute(SQL["STEP_FINISH"], ("st0", "failed", None, 0, 0, None, None, "x", LATER)).rowcount, 0)
        c.execute(SQL["STEP_FINISH"], ("st1", "failed", json.dumps({"error": "not_found"}), 20, 0, None, None, "not_found", LATER))
        rows = c.execute(SQL["STEPS_LIST"], ("r1",)).fetchall()
        self.assertEqual([r[1] for r in rows], [0, 1])
        self.assertEqual(rows[0][3], "complete"); self.assertEqual(rows[1][12], "not_found")
        with self.assertRaises(sqlite3.IntegrityError):
            c.execute(SQL["STEP_BEGIN"], ("st2", "r1", 1, "tool_call", None, None, None, NOW))

    def test_events_cursor(self):
        c = connect(); seed_turn(c)
        for k in ("turn_started", "model_call_started", "answer_saved"):
            c.execute(SQL["EVENT"], ("r1", "s1", None, k, "{}", NOW))
        rows = c.execute(SQL["EVENTS_AFTER"], ("s1", 1)).fetchall()
        self.assertEqual([r[3] for r in rows], ["model_call_started", "answer_saved"])
        self.assertEqual(c.execute(SQL["SESSION_OF_REQUEST"], ("r1",)).fetchone()[0], "s1")

    def test_permission_flow(self):
        c = connect(); seed_turn(c)
        c.execute(SQL["STEP_BEGIN"], ("st1", "r1", 0, "tool_call", "edit", "call_1", "{}", NOW))
        c.execute(SQL["PERMISSION_CREATE"], ("p1", "r1", "st1", "edit", "edit a.rs (+1 -1)", json.dumps({"diff": "-a\n+b"}), NOW, LATER))
        pend = c.execute(SQL["PERMISSIONS_PENDING"], ("proj",)).fetchall()
        self.assertEqual([p[0] for p in pend], ["p1"])
        self.assertEqual(c.execute(SQL["PERMISSIONS_PENDING"], ("other",)).fetchall(), [])
        self.assertEqual(c.execute(SQL["PERMISSION_STATUS"], ("p1",)).fetchone()[0], "pending")
        self.assertEqual(c.execute(SQL["PERMISSION_RESOLVE"], ("p1", "approved", LATER)).rowcount, 1)
        self.assertEqual(c.execute(SQL["PERMISSION_RESOLVE"], ("p1", "denied", LATER)).rowcount, 0)  # idempotency guard
        row = c.execute(SQL["PERMISSION_GET"], ("p1",)).fetchone()
        self.assertEqual((row[6], row[10]), ("approved", "proj"))
        c.execute(SQL["PERMISSION_CREATE"], ("p2", "r1", "st1", "bash", "run tests", "{}", NOW, LATER))
        self.assertEqual(c.execute(SQL["PERMISSION_EXPIRE"], ("p2", LATER)).rowcount, 1)

    def test_file_changes_and_plan(self):
        c = connect(); seed_turn(c)
        c.execute(SQL["STEP_BEGIN"], ("st1", "r1", 0, "tool_call", "edit", "call_1", "{}", NOW))
        c.execute(SQL["FILE_CHANGE"], ("fc1", "r1", "st1", "src/a.rs", "modify", "aaaa", "bbbb", "-x\n+y", 1, NOW))
        with self.assertRaises(sqlite3.IntegrityError):
            c.execute(SQL["FILE_CHANGE"], ("fc2", "r1", "st1", "src/a.rs", "rename", None, None, "", 1, NOW))
        rows = c.execute(SQL["FILE_CHANGES_LIST"], ("r1",)).fetchall()
        self.assertEqual(rows[0][2:4], ("src/a.rs", "modify"))
        c.execute("BEGIN IMMEDIATE")
        c.execute(SQL["PLAN_CLEAR"], ("s1",))
        for i, (t, s) in enumerate([("read code", "done"), ("write test", "in_progress"), ("run suite", "pending")]):
            c.execute(SQL["PLAN_INSERT"], (str(uuid.uuid4()), "s1", i, t, s, NOW))
        c.execute("COMMIT")
        plan = c.execute(SQL["PLAN_LIST"], ("s1",)).fetchall()
        self.assertEqual([p[2] for p in plan], ["done", "in_progress", "pending"])
        c.execute("BEGIN IMMEDIATE"); c.execute(SQL["PLAN_CLEAR"], ("s1",)); c.execute("COMMIT")
        self.assertEqual(c.execute(SQL["PLAN_LIST"], ("s1",)).fetchall(), [])

    def test_recovery(self):
        c = connect(); seed_turn(c)
        c.execute(SQL["STEP_BEGIN"], ("st1", "r1", 0, "tool_call", "bash", "call_1", "{}", NOW))
        c.execute(SQL["PERMISSION_CREATE"], ("p1", "r1", "st1", "bash", "x", "{}", NOW, LATER))
        c.execute("BEGIN IMMEDIATE")
        c.execute(SQL["RECOVER_STEPS"], (LATER,))
        c.execute(SQL["RECOVER_PERMISSIONS"], (LATER,))
        c.execute(SQL["RECOVER_ACTIVITY"], (LATER,))
        c.execute("UPDATE chat_receipts SET state='interrupted',error_code='process_restarted',updated_at=?1 WHERE state='generating'", (LATER,))
        c.execute("COMMIT")
        self.assertEqual(c.execute("SELECT status FROM turn_steps WHERE id='st1'").fetchone()[0], "interrupted")
        self.assertEqual(c.execute("SELECT status FROM permission_requests WHERE id='p1'").fetchone()[0], "expired")
        ev = c.execute(SQL["EVENTS_AFTER"], ("s1", 0)).fetchall()
        self.assertEqual([e[3] for e in ev], ["interrupted"])


if __name__ == "__main__":
    unittest.main()
