#!/usr/bin/env python3
"""Offline migration-chain check. No Rust toolchain needed.

Applies migrations/001 through the latest migration to an in-memory SQLite DB the same way
`DbStore::init` does (execute_batch in order), then asserts the expected tables,
user_version and CHECK constraints. Also verifies a populated v3 database upgrades to v4.
"""
import pathlib
import sqlite3
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
MIG = ROOT / "migrations"
CHAIN = ["001_core.sql", "002_recording.sql", "003_agentic.sql", "004_memory_kinds.sql", "005_generation_stream.sql", "006_provenance_edges.sql", "007_privacy_archive.sql", "008_provider_spend.sql", "009_run_cancellation.sql", "010_retention_maintenance.sql", "011_retrieval_receipts.sql", "012_memory_governance.sql", "013_session_workflows.sql", "014_history_search.sql"]
VERSIONS = [name.split("_", 1)[0] for name in CHAIN]
LATEST_VERSION = int(VERSIONS[-1])
OPEN_CONNECTIONS = []

EXPECTED_TABLES = {
    1: {"sessions", "messages", "sources", "jobs", "candidates", "memories", "memory_revisions", "settings"},
    2: {"chat_receipts", "recording_events", "recording_outbox"},
    3: {"scopes", "turn_steps", "activity_events", "permission_requests", "file_changes", "plan_items"},
    4: {"memory_embeddings"},
    5: {"generation_events"},
    6: {"provenance_edges"},
    7: {"exact_archives", "source_privacy_state", "privacy_events"},
    8: {"provider_calls"},
    9: {"run_controls"},
    10: {"retention_policies", "maintenance_runs"},
    11: {"retrieval_receipts", "retrieval_candidates"},
    12: {"memory_branches", "memory_decisions", "memory_feedback"},
    13: set(),
    14: {"history_documents", "history_privacy_events", "export_bundles", "export_items", "import_receipts", "import_decisions"},
}


def fresh():
    c = sqlite3.connect(":memory:")
    OPEN_CONNECTIONS.append(c)
    c.execute("PRAGMA foreign_keys=ON")
    return c


def close_connections():
    while OPEN_CONNECTIONS:
        OPEN_CONNECTIONS.pop().close()


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


def test_006_provenance_constraints():
    c = fresh()
    apply(c, len(CHAIN))
    now = "2026-01-01T00:00:00Z"
    c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?)", (now,))
    c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('req','s1','user','hi','pending',?)", (now,))
    c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('req','s1','proj','m','sig',0,'generating',?,?)", (now, now))
    c.execute("INSERT INTO turn_steps(id,request_id,seq,kind,status,started_at) VALUES('step','req',0,'tool_call','running',?)", (now,))
    c.execute("INSERT INTO permission_requests(id,request_id,step_id,tool_name,summary,args_json,status,created_at,expires_at) VALUES('permit','req','step','edit','edit','{}','approved',?,?)", (now, now))
    c.execute("INSERT INTO file_changes(id,request_id,step_id,path,action,diff,applied,created_at) VALUES('change','req','step','a','modify','',1,?)", (now,))
    c.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES('source','proj','n','jsonl','fp','1','safe','[]',?)", (now,))
    c.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES('candidate','proj','k','v','fact','source','{}',0,'approved',?,2000000000)", (now,))
    c.execute("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES('memory','proj','k','v','fact','active',1,'candidate',?,?)", (now, now))
    c.execute("INSERT INTO activity_events(request_id,session_id,kind,payload_json,created_at) VALUES('req','s1','interrupted','{}',?)", (now,))
    recovery = str(c.execute("SELECT last_insert_rowid()").fetchone()[0])
    nodes = [("evidence", "source"), ("step", "step"), ("permission", "permit"),
             ("mutation", "change"), ("memory", "memory"), ("recovery", recovery)]
    for index, ((source_kind, source_id), (target_kind, target_id)) in enumerate(zip(nodes, nodes[1:] + nodes[:1])):
        c.execute("INSERT INTO provenance_edges(id,request_id,source_kind,source_id,relation,target_kind,target_id,created_at) VALUES(?,?,?,?,?,?,?,?)",
                  (f"edge-{index}", "req", source_kind, source_id, "depends_on", target_kind, target_id, now))
    assert c.execute("SELECT count(*) FROM provenance_edges").fetchone()[0] == 6
    try:
        c.execute("DELETE FROM sources WHERE id='source'")
    except sqlite3.IntegrityError:
        pass
    else:
        raise AssertionError("referenced provenance endpoint was deleted")
    bad = [
        ("bad-kind", "req", "claim", "x", "supports", "step", "step", now),
        ("bad-relation", "req", "step", "step", "caused", "mutation", "change", now),
        ("missing", "req", "step", "missing", "supports", "mutation", "change", now),
        ("self", "req", "step", "step", "depends_on", "step", "step", now),
    ]
    for row in bad:
        try:
            c.execute("INSERT INTO provenance_edges(id,request_id,source_kind,source_id,relation,target_kind,target_id,created_at) VALUES(?,?,?,?,?,?,?,?)", row)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError(f"invalid provenance edge was accepted: {row[0]}")


def test_007_privacy_archive_constraints():
    c = fresh()
    apply(c, len(CHAIN))
    now = "2026-01-01T00:00:00Z"
    c.execute("INSERT INTO exact_archives(id,source_id,relative_path,format_version,algorithm,key_id,plaintext_sha256,byte_length,created_at) VALUES('a','s','a.har',1,'AES-256-GCM','0123456789abcdef',?,4,?)", ("0" * 64, now))
    for index, (action, archive_id) in enumerate((("forget", None), ("delete_source", None), ("purge_index", None), ("delete_archive", "a"))):
        c.execute("INSERT INTO privacy_events(id,source_id,archive_id,action,created_at) VALUES(?,?,?,?,?)", (f"e{index}", "s", archive_id, action, now))
    assert [row[0] for row in c.execute("SELECT action FROM privacy_events ORDER BY seq")] == ["forget", "delete_source", "purge_index", "delete_archive"]
    try:
        c.execute("DELETE FROM privacy_events WHERE id='e0'")
    except sqlite3.IntegrityError:
        pass
    else:
        raise AssertionError("privacy audit event was deleted")


def test_009_run_cancellation_constraints():
    c = fresh()
    apply(c, len(CHAIN))
    now = "2026-01-01T00:00:00Z"
    c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?)", (now,))
    for request_id in ("req", "retry"):
        c.execute(
            "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,'s1','user','hi','pending',?)",
            (request_id, now),
        )
        c.execute(
            "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES(?,'s1','proj','m',?,0,'captured',?,?)",
            (request_id, f"sig-{request_id}", now, now),
        )
    c.execute(
        "INSERT INTO run_controls(request_id,cancel_requested_at,safe_boundary_seq,retried_by) VALUES('req',?,3,'retry')",
        (now,),
    )
    c.execute("INSERT INTO run_controls(request_id,retry_of) VALUES('retry','req')")
    assert c.execute("SELECT safe_boundary_seq,retried_by FROM run_controls WHERE request_id='req'").fetchone() == (3, "retry")
    for statement in (
        "INSERT INTO run_controls(request_id,cancelled_at) VALUES('missing-request','2026-01-01T00:00:00Z')",
        "UPDATE run_controls SET retry_of='retry' WHERE request_id='retry'",
        "UPDATE run_controls SET retried_by='req' WHERE request_id='req'",
    ):
        try:
            c.execute(statement)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError(f"run cancellation constraint was not enforced: {statement}")


def test_010_retention_maintenance_constraints():
    c = fresh()
    apply(c, len(CHAIN))
    now = "2026-01-01T00:00:00Z"
    assert {r[0] for r in c.execute("SELECT name FROM retention_policies")} == {"generation_chunks", "activity_events"}
    assert c.execute("SELECT count(*) FROM retention_policies WHERE enabled=1").fetchone()[0] == 0, "retention must be disabled until an owner enables it"
    columns = {row[1] for row in c.execute("PRAGMA table_info('generation_events')")}
    assert "compacted_chunks" in columns
    assert c.execute("SELECT keep_days FROM retention_policies WHERE name='generation_chunks'").fetchone()[0] == 30
    c.execute(
        "INSERT INTO maintenance_runs(action,target,rows_affected,wal_pages,checkpointed_pages,freelist_pages,detail,started_at,finished_at)"
        " VALUES('wal_checkpoint','database',0,4,4,0,'journal_mode=wal',?,?)",
        (now, now),
    )
    assert c.execute("SELECT action,rows_affected FROM maintenance_runs").fetchone() == ("wal_checkpoint", 0)
    for statement in (
        "INSERT INTO retention_policies(name,keep_days,enabled,updated_at) VALUES('chat_receipts',30,1,'2026-01-01T00:00:00Z')",
        "UPDATE retention_policies SET keep_days=0 WHERE name='activity_events'",
        "UPDATE retention_policies SET enabled=2 WHERE name='activity_events'",
        "INSERT INTO maintenance_runs(action,target,rows_affected,started_at,finished_at) VALUES('vacuum','database',0,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        "INSERT INTO maintenance_runs(action,target,rows_affected,started_at,finished_at) VALUES('retention','activity_events',-1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
    ):
        try:
            c.execute(statement)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError(f"maintenance constraint was not enforced: {statement}")


def test_014_history_search_constraints():
    """P15-T04. The four invariants migration 014 exists to hold:

    the FTS projection stays in sync through the same three-trigger pattern memory_fts uses;
    forget and source-delete are different columns with different observable outcomes;
    the privacy audit is append-only; and an export bundle cannot reach 'released' without a
    reviewed checksum.
    """
    c = fresh()
    apply(c, len(CHAIN))
    now = "2026-01-01T00:00:00Z"
    c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?)", (now,))
    c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('m1','s1','user','we chose SQLite for durability','complete',?)", (now,))
    # Asserted here by name rather than only through EXPECTED_TABLES, which is a subset check:
    # removing an entry from that dict weakens it silently, while this fails.
    created = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    missing = {"history_documents", "history_fts", "history_privacy_events", "export_bundles",
               "export_items", "import_receipts", "import_decisions"} - created
    assert not missing, f"migration 014 did not create: {missing}"
    c.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES('src1','proj','notes','jsonl','fp','1','sanitized artifact body','[]',?)", (now,))
    c.execute(
        "INSERT INTO history_documents(id,kind,source_id,scope,session_id,revision,role,title,body,sanitizer,content_sha256,source_created_at,indexed_at)"
        " VALUES('d1','turn','m1','proj','s1',1,'user','user turn','we chose SQLite for durability','harness-sanitize-v1',?,?,?)",
        ("a" * 64, now, now),
    )
    c.execute(
        "INSERT INTO history_documents(id,kind,source_id,scope,session_id,revision,role,title,body,sanitizer,content_sha256,source_created_at,indexed_at)"
        " VALUES('d2','artifact','src1','proj',NULL,1,NULL,'notes','sanitized artifact body','harness-sanitize-v1',?,?,?)",
        ("b" * 64, now, now),
    )
    # The FTS projection is populated by the insert trigger, and follows an update.
    assert c.execute("SELECT count(*) FROM history_fts WHERE history_fts MATCH 'sqlite'").fetchone()[0] == 1
    c.execute("UPDATE history_documents SET body='we chose Postgres instead' WHERE id='d1'")
    assert c.execute("SELECT count(*) FROM history_fts WHERE history_fts MATCH 'sqlite'").fetchone()[0] == 0
    assert c.execute("SELECT count(*) FROM history_fts WHERE history_fts MATCH 'postgres'").fetchone()[0] == 1

    # forget: suppression only. The row, its citation and its body survive.
    c.execute("UPDATE history_documents SET forgotten_at=? WHERE id='d1'", (now,))
    c.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,created_at) VALUES('e1','d1','turn','m1','forget',1,?)", (now,))
    row = c.execute("SELECT forgotten_at,source_deleted_at,body FROM history_documents WHERE id='d1'").fetchone()
    assert row[0] == now and row[1] is None and row[2] != "", "forget must not destroy content"

    # source delete: the body must be empty, enforced by CHECK rather than by convention.
    try:
        c.execute("UPDATE history_documents SET source_deleted_at=? WHERE id='d2'", (now,))
    except sqlite3.IntegrityError:
        pass
    else:
        raise AssertionError("a source-deleted document kept its body")
    c.execute("UPDATE history_documents SET body='',source_deleted_at=? WHERE id='d2'", (now,))
    c.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,created_at) VALUES('e2','d2','artifact','src1','delete_source',1,?)", (now,))
    assert c.execute("SELECT count(*) FROM history_fts WHERE history_fts MATCH 'artifact'").fetchone()[0] == 0

    # Deleting the source row from the source side reaches the same state.
    c.execute(
        "INSERT INTO history_documents(id,kind,source_id,scope,session_id,revision,role,title,body,sanitizer,content_sha256,source_created_at,indexed_at)"
        " VALUES('d3','artifact','src2','proj',NULL,1,NULL,'other','another body','harness-sanitize-v1',?,?,?)",
        ("c" * 64, now, now),
    )
    c.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES('src2','proj','other','jsonl','fp2','1','another body','[]',?)", (now,))
    c.execute("DELETE FROM sources WHERE id='src2'")
    row = c.execute("SELECT body,source_deleted_at FROM history_documents WHERE id='d3'").fetchone()
    assert row[0] == "" and row[1] is not None, "deleting a source must empty its indexed body"
    assert c.execute("SELECT count(*) FROM history_documents WHERE id='d3'").fetchone()[0] == 1, "the citation must survive a source delete"

    # The audit is append-only in both directions.
    for statement in ("DELETE FROM history_privacy_events WHERE id='e1'", "UPDATE history_privacy_events SET action='restore' WHERE id='e1'"):
        try:
            c.execute(statement)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError(f"history privacy audit was mutable: {statement}")
    assert [r[0] for r in c.execute("SELECT action FROM history_privacy_events ORDER BY seq")] == ["forget", "delete_source"]

    # Export bundles cannot claim a review they do not carry.
    c.execute("INSERT INTO export_bundles(id,kind,scope,audience,state,format_version,item_count,created_at,updated_at) VALUES('b1','continuation_packet','proj','team','draft',1,1,?,?)", (now, now))
    for statement, values in (
        ("UPDATE export_bundles SET state='reviewed' WHERE id='b1'", ()),
        ("UPDATE export_bundles SET state='released',content_sha256=?,reviewed_at=? WHERE id='b1'", ("d" * 64, now)),
        ("INSERT INTO export_bundles(id,kind,scope,audience,state,format_version,item_count,created_at,updated_at) VALUES('b2','unknown_kind','proj','team','draft',1,0,?,?)", (now, now)),
        ("INSERT INTO export_bundles(id,kind,scope,audience,state,format_version,item_count,created_at,updated_at) VALUES('b3','continuation_packet','proj','everyone','draft',1,0,?,?)", (now, now)),
        ("INSERT INTO export_items(bundle_id,kind,stable_id,revision,payload_json,content_sha256,sanitized,created_at) VALUES('b1','memory','m',0,'{}',?,1,?)", ("e" * 64, now)),
        ("INSERT INTO import_receipts(id,bundle_id,origin,scope,kind,content_sha256,accepted,unchanged,skipped,created_at) VALUES('r1','b1','o','proj','continuation_packet',?,-1,0,0,?)", ("f" * 64, now)),
    ):
        try:
            c.execute(statement, values)
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError(f"export/import constraint was not enforced: {statement}")
    c.execute("UPDATE export_bundles SET state='reviewed',content_sha256=?,reviewed_at=? WHERE id='b1'", ("d" * 64, now))
    c.execute("UPDATE export_bundles SET state='released',released_at=? WHERE id='b1'", (now,))
    assert c.execute("SELECT state FROM export_bundles WHERE id='b1'").fetchone()[0] == "released"
    c.execute("INSERT INTO import_receipts(id,bundle_id,origin,scope,kind,content_sha256,accepted,unchanged,skipped,created_at) VALUES('r1','b1','remote','proj','continuation_packet',?,1,0,0,?)", ("f" * 64, now))
    c.execute("INSERT INTO import_decisions(receipt_id,kind,stable_id,revision,outcome) VALUES('r1','history','d1',1,'unchanged')")
    try:
        c.execute("INSERT INTO import_decisions(receipt_id,kind,stable_id,revision,outcome) VALUES('r1','history','d1',1,'invented')")
    except sqlite3.IntegrityError:
        pass
    else:
        raise AssertionError("an unknown import outcome was accepted")


def main():
    try:
        check_fts5()
        test_full_chain()
        test_v2_to_v3()
        test_populated_v3_to_v4()
        test_004_memory_categories_and_embedding_constraints()
        test_003_constraints()
        test_006_provenance_constraints()
        test_007_privacy_archive_constraints()
        test_009_run_cancellation_constraints()
        test_010_retention_maintenance_constraints()
        test_014_history_search_constraints()
        print(
            f"migrations OK: {' -> '.join(VERSIONS)}, "
            f"user_version={LATEST_VERSION}, data/FTS/FKs preserved"
        )
    finally:
        close_connections()


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
