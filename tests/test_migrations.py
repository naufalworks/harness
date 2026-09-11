#!/usr/bin/env python3
"""Offline migration-chain check. No Rust toolchain needed.

Applies migrations/001 -> 002 -> 003 -> 004 -> 005 to an in-memory SQLite DB the same way
`DbStore::init` does (execute_batch in order), then asserts the expected tables,
user_version and CHECK constraints. Also verifies a populated v3 database upgrades to v4.
"""
import pathlib
import sqlite3
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
MIG = ROOT / "migrations"
CHAIN = ["001_core.sql", "002_recording.sql", "003_agentic.sql", "004_memory_kinds.sql", "005_generation_stream.sql"]
VERSIONS = [name.split("_", 1)[0] for name in CHAIN]
LATEST_VERSION = int(VERSIONS[-1])

EXPECTED_TABLES = {
    1: {"sessions", "messages", "sources", "jobs", "candidates", "memories", "memory_revisions", "settings"},
    2: {"chat_receipts", "recording_events", "recording_outbox"},
    3: {"scopes", "turn_steps", "activity_events", "permission_requests", "file_changes", "plan_items"},
    4: {"memory_embeddings"},
    5: {"generation_events"},
}


def fresh():
    c = sqlite3.connect(":memory:")
    c.execute("PRAGMA foreign_keys=ON")
    return c


def tables(c):
    return {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")}


def apply(c, upto):
    for name in CHAIN[:upto]:
        c.executescript((MIG / name).read_text())


def check_fts5():
    c = fresh()
    c.execute("CREATE VIRTUAL TABLE t USING fts5(a)")


def test_full_chain():
    c = fresh()
    apply(c, len(CHAIN))
    assert c.execute("PRAGMA user_version").fetchone()[0] == LATEST_VERSION
    have = tables(c)
    for v, names in EXPECTED_TABLES.items():
        missing = names - have
        assert not missing, f"missing tables after v{v}: {missing}"
    return c


def test_v2_to_v3():
    c = fresh()
    apply(c, 2)
    assert c.execute("PRAGMA user_version").fetchone()[0] == 2
    c.executescript((MIG / CHAIN[2]).read_text())
    assert c.execute("PRAGMA user_version").fetchone()[0] == 3
    assert EXPECTED_TABLES[3] <= tables(c)


def test_populated_v3_to_v4():
    c = fresh()
    apply(c, 3)
    now = "2026-01-01T00:00:00Z"
    c.execute(
        "INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at,resolved_at)"
        " VALUES('c-old','proj','language','Rust','preference','source-old','{\"quote\":\"I prefer Rust\"}',0,'approved',?,2000000000,?)",
        (now, now),
    )
    c.execute(
        "INSERT INTO memories(rowid,id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at)"
        " VALUES(41,'m-old','proj','language','Rust','preference','active',1,'c-old',?,?)",
        (now, now),
    )
    c.execute(
        "INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at)"
        " VALUES('r-old','m-old',1,'approve',NULL,'Rust','c-old',?)",
        (now,),
    )
    c.commit()
    c.executescript((MIG / CHAIN[3]).read_text())
    assert c.execute("PRAGMA user_version").fetchone()[0] == 4
    assert c.execute("PRAGMA foreign_keys").fetchone()[0] == 1
    assert c.execute("PRAGMA foreign_key_check").fetchall() == []
    assert c.execute("SELECT rowid,id,candidate_id FROM memories WHERE id='m-old'").fetchone() == (41, "m-old", "c-old")
    assert c.execute("SELECT id,memory_id,candidate_id FROM memory_revisions").fetchone() == ("r-old", "m-old", "c-old")
    assert c.execute("SELECT rowid FROM memory_fts WHERE memory_fts MATCH 'Rust'").fetchone() == (41,)
    indexes = {row[1] for row in c.execute("PRAGMA index_list('memory_embeddings')")}
    assert "memory_embeddings_model" in indexes
    c.execute("UPDATE memories SET value='Rust systems' WHERE id='m-old'")
    assert c.execute("SELECT rowid FROM memory_fts WHERE memory_fts MATCH 'systems'").fetchone() == (41,)
    return c


def test_004_memory_categories_and_embedding_constraints():
    c = test_populated_v3_to_v4()
    now = "2026-01-01T00:00:00Z"
    categories = ("preference", "fact", "project", "rule", "skill", "decision", "episodic", "procedural")
    for index, category in enumerate(categories):
        c.execute(
            "INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at)"
            " VALUES(?,?,?,?,?,'source-kinds','{}',0,'pending',?,2000000000)",
            (f"c-{category}", "kinds", f"key_{index}", category, category, now),
        )
    bad_rows = (
        ("INSERT INTO candidates VALUES('bad','x','bad','bad','unknown','s','{}',0,'pending',?,2000000000,NULL)", (now,)),
        ("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES('bad-m','kinds','bad','bad','unknown','active',1,'c-fact',?,?)", (now, now)),
    )
    for statement, values in bad_rows:
        try:
            c.execute(statement, values)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError("unsupported memory category was accepted")
    c.execute(
        "INSERT INTO memory_embeddings(memory_id,model,dimensions,vector,content_hash,updated_at) VALUES('m-old','local',8,zeroblob(32),'hash',?)",
        (now,),
    )
    assert c.execute("SELECT dimensions,length(vector),recall_count,useful_count FROM memory_embeddings").fetchone() == (8, 32, 0, 0)


def test_003_constraints():
    c = test_full_chain()
    now = "2026-01-01T00:00:00Z"
    c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?)", (now,))
    c.execute(
        "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('m1','s1','user','hi','complete',?)",
        (now,),
    )
    c.execute(
        "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at)"
        " VALUES('m1','s1','proj','x','sig',0,'generating',?,?)",
        (now, now),
    )
    c.execute("INSERT INTO scopes(scope,created_at,updated_at) VALUES('proj',?,?)", (now, now))
    assert c.execute("SELECT permission_mode FROM scopes WHERE scope='proj'").fetchone()[0] == "ask"
    for bad in ("UPDATE scopes SET permission_mode='yolo'",):
        try:
            c.execute(bad)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError(f"CHECK not enforced: {bad}")
    c.execute(
        "INSERT INTO turn_steps(id,request_id,seq,kind,status,started_at) VALUES('st1','m1',0,'model_call','running',?)",
        (now,),
    )
    try:
        c.execute(
            "INSERT INTO turn_steps(id,request_id,seq,kind,status,started_at) VALUES('st2','m1',0,'tool_call','running',?)",
            (now,),
        )
    except sqlite3.IntegrityError:
        pass
    else:
        raise AssertionError("UNIQUE(request_id,seq) not enforced")
    c.execute(
        "INSERT INTO permission_requests(id,request_id,step_id,tool_name,summary,args_json,status,created_at,expires_at)"
        " VALUES('p1','m1','st1','edit','edit x','{}','pending',?,?)",
        (now, now),
    )
    c.execute(
        "INSERT INTO activity_events(request_id,session_id,step_id,kind,payload_json,created_at)"
        " VALUES('m1','s1','st1','tool_started','{}',?)",
        (now,),
    )
    c.execute(
        "INSERT INTO plan_items(id,session_id,seq,text,status,updated_at) VALUES('pl1','s1',0,'do it','pending',?)",
        (now,),
    )
    # recovery statements from docs/design/agentic-turn.md#recovery-additions
    c.execute("UPDATE turn_steps SET status='interrupted', finished_at=?1 WHERE status='running'", (now,))
    c.execute("UPDATE permission_requests SET status='expired', resolved_at=?1 WHERE status='pending'", (now,))
    assert c.execute("SELECT status FROM turn_steps WHERE id='st1'").fetchone()[0] == "interrupted"
    assert c.execute("SELECT status FROM permission_requests WHERE id='p1'").fetchone()[0] == "expired"


def main():
    check_fts5()
    test_full_chain()
    test_v2_to_v3()
    test_populated_v3_to_v4()
    test_004_memory_categories_and_embedding_constraints()
    test_003_constraints()
    print(
        f"migrations OK: {' -> '.join(VERSIONS)}, "
        f"user_version={LATEST_VERSION}, data/FTS/FKs preserved"
    )


class Suite(unittest.TestCase):
    """Lets `python3 -m unittest discover -s tests` pick this file up."""

    def test_all(self):
        main()


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print("FAIL:", e, file=sys.stderr)
        sys.exit(1)
