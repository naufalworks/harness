#!/usr/bin/env python3
"""Operator entry point for P11-T04 storage maintenance.

Mirrors `DbStore` maintenance behaviour for offline/cron use:

  status      show policies, WAL/freelist counters and the last maintenance runs
  policy      configure a retention window (disabled by default)
  retention   delete expired derived rows for enabled policies only
  compact     collapse generation chunk rows of finished turns into one row
  checkpoint  WAL checkpoint (TRUNCATE) + optimize + ANALYZE + incremental vacuum
  run         retention, then compact, then checkpoint
  --check     self-test on a temporary database; no production database touched

Safety rules enforced here and in src/storage.rs:
- receipts (`chat_receipts`), provenance edges, memories, privacy/archive rows are never deleted;
- only turns that already reached a terminal receipt state are eligible;
- terminal generation rows ('completed' | 'interrupted' | 'failed') are always preserved,
  so a compacted or trimmed turn still replays its outcome;
- every action writes an evidence row into `maintenance_runs`.
"""
import argparse
import datetime as dt
import json
import pathlib
import sqlite3
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
MIGRATIONS = ROOT / "migrations"
DEFAULT_DATABASE = ROOT / "data" / "harness_v2.db"
TARGETS = ("generation_chunks", "activity_events")
# P18-T03: this was pinned to an exact version (10, where the maintenance tables landed) and the
# equality check made every later migration silently break the tool: by schema 18 `--check`
# asserted itself into an AssertionError and `connect()` refused every real database. What
# maintenance actually needs are the tables migration 010 introduced, and additive migrations after
# it cannot take those away, so the floor stays a floor instead of becoming a pin. The ceiling is
# the head of *this* checkout's migration chain: a database newer than the code in hand may have
# changed something this script cannot see, and refusing that is the honest outcome.
MIN_SCHEMA_VERSION = 10


def chain_head_version() -> int:
    versions = [int(p.name[:3]) for p in MIGRATIONS.glob("[0-9][0-9][0-9]_*.sql")]
    if not versions:
        raise SystemExit(f"BLOCKED: no migrations found in {MIGRATIONS}")
    return max(versions)
FINISHED_RECEIPTS = "SELECT request_id FROM chat_receipts WHERE state NOT IN ('captured','generating')"


def now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def cutoff(keep_days: int) -> str:
    return (dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=keep_days)).isoformat()


def connect(database: pathlib.Path) -> sqlite3.Connection:
    if not pathlib.Path(database).exists():
        raise SystemExit(f"BLOCKED: database not found: {database}")
    connection = sqlite3.connect(str(database), isolation_level=None)
    connection.execute("PRAGMA foreign_keys=ON")
    connection.execute("PRAGMA busy_timeout=5000")
    version = connection.execute("PRAGMA user_version").fetchone()[0]
    head = chain_head_version()
    if version < MIN_SCHEMA_VERSION:
        raise SystemExit(
            f"BLOCKED: schema_version={version}, maintenance requires at least {MIN_SCHEMA_VERSION}"
        )
    if version > head:
        raise SystemExit(
            f"BLOCKED: schema_version={version} is newer than this checkout's migration chain (head {head});"
            " update the checkout before running maintenance"
        )
    return connection


def record(connection, action, target, rows, started, *, wal=None, checkpointed=None, freelist=None, detail=None):
    connection.execute(
        "INSERT INTO maintenance_runs(action,target,rows_affected,wal_pages,checkpointed_pages,freelist_pages,detail,started_at,finished_at)"
        " VALUES(?,?,?,?,?,?,?,?,?)",
        (action, target, rows, wal, checkpointed, freelist, detail, started, now()),
    )


def set_policy(connection, name, keep_days, enabled):
    if name not in TARGETS:
        raise SystemExit(f"BLOCKED: unknown retention target {name}; allowed: {', '.join(TARGETS)}")
    if keep_days is not None and keep_days < 1:
        raise SystemExit("BLOCKED: keep-days must be at least 1")
    current = connection.execute("SELECT keep_days,enabled FROM retention_policies WHERE name=?", (name,)).fetchone()
    keep = keep_days if keep_days is not None else (current[0] if current else 30)
    flag = current[1] if (current and enabled is None) else int(bool(enabled))
    connection.execute(
        "INSERT INTO retention_policies(name,keep_days,enabled,updated_at) VALUES(?,?,?,?)"
        " ON CONFLICT(name) DO UPDATE SET keep_days=excluded.keep_days,enabled=excluded.enabled,updated_at=excluded.updated_at",
        (name, keep, flag, now()),
    )
    return {"name": name, "keep_days": keep, "enabled": bool(flag)}


def apply_retention(connection):
    report = {}
    for name, keep_days, enabled in connection.execute(
        "SELECT name,keep_days,enabled FROM retention_policies ORDER BY name"
    ).fetchall():
        if not enabled:
            report[name] = {"status": "disabled", "rows_deleted": 0}
            continue
        started = now()
        edge = cutoff(keep_days)
        if name == "generation_chunks":
            deleted = connection.execute(
                "DELETE FROM generation_events WHERE state='chunk' AND created_at<?"
                f" AND request_id IN ({FINISHED_RECEIPTS})"
                " AND request_id IN (SELECT request_id FROM generation_events WHERE state IN ('completed','interrupted','failed'))",
                (edge,),
            ).rowcount
        else:
            deleted = connection.execute(
                f"DELETE FROM activity_events WHERE created_at<? AND request_id IN ({FINISHED_RECEIPTS})",
                (edge,),
            ).rowcount
        record(connection, "retention", name, deleted, started, detail=f"keep_days={keep_days}")
        report[name] = {"status": "applied", "rows_deleted": deleted, "cutoff": edge}
    return report


def compact_generation_chunks(connection, older_than_days=1):
    started = now()
    edge = cutoff(max(older_than_days, 0))
    requests = [
        row[0]
        for row in connection.execute(
            "SELECT g.request_id FROM generation_events g"
            f" WHERE g.state='chunk' AND g.created_at<? AND g.request_id IN ({FINISHED_RECEIPTS})"
            " GROUP BY g.request_id HAVING count(*)>1",
            (edge,),
        ).fetchall()
    ]
    compacted = 0
    removed = 0
    for request_id in requests:
        rows = connection.execute(
            "SELECT seq,content FROM generation_events WHERE request_id=? AND state='chunk' ORDER BY seq",
            (request_id,),
        ).fetchall()
        if len(rows) < 2:
            continue
        keep = rows[0][0]
        merged = "".join(row[1] or "" for row in rows)
        connection.execute(
            "UPDATE generation_events SET content=?,compacted_chunks=? WHERE seq=?",
            (merged, len(rows), keep),
        )
        removed += connection.execute(
            "DELETE FROM generation_events WHERE request_id=? AND state='chunk' AND seq<>?",
            (request_id, keep),
        ).rowcount
        compacted += 1
    record(connection, "compaction", "generation_chunks", removed, started, detail=f"{compacted} request(s) compacted")
    return {"requests_compacted": compacted, "chunk_rows_removed": removed, "cutoff": edge}


def checkpoint(connection):
    started = now()
    journal = connection.execute("PRAGMA journal_mode").fetchone()[0]
    try:
        busy, wal_pages, checkpointed = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
    except sqlite3.Error:
        busy, wal_pages, checkpointed = 0, -1, -1
    connection.execute("PRAGMA optimize")
    connection.execute("ANALYZE")
    auto_vacuum = connection.execute("PRAGMA auto_vacuum").fetchone()[0]
    freelist_before = connection.execute("PRAGMA freelist_count").fetchone()[0]
    if auto_vacuum == 2:
        connection.execute("PRAGMA incremental_vacuum")
        vacuum = "incremental_vacuum_ran"
    else:
        vacuum = "incremental_vacuum_unavailable_auto_vacuum_off"
    freelist_after = connection.execute("PRAGMA freelist_count").fetchone()[0]
    record(
        connection,
        "wal_checkpoint",
        "database",
        0,
        started,
        wal=wal_pages,
        checkpointed=checkpointed,
        freelist=freelist_after,
        detail=f"journal_mode={journal}; busy={busy}; {vacuum}",
    )
    return {
        "journal_mode": journal,
        "busy": busy,
        "wal_pages": wal_pages,
        "checkpointed_pages": checkpointed,
        "freelist_before": freelist_before,
        "freelist_after": freelist_after,
        "incremental_vacuum": vacuum,
    }


def status(connection):
    policies = [
        {"name": r[0], "keep_days": r[1], "enabled": bool(r[2]), "updated_at": r[3]}
        for r in connection.execute("SELECT name,keep_days,enabled,updated_at FROM retention_policies ORDER BY name")
    ]
    recent = [
        {"action": r[0], "target": r[1], "rows_affected": r[2], "finished_at": r[3]}
        for r in connection.execute(
            "SELECT action,target,rows_affected,finished_at FROM maintenance_runs ORDER BY id DESC LIMIT 10"
        )
    ]
    return {
        "schema_version": connection.execute("PRAGMA user_version").fetchone()[0],
        "journal_mode": connection.execute("PRAGMA journal_mode").fetchone()[0],
        "freelist_pages": connection.execute("PRAGMA freelist_count").fetchone()[0],
        "page_count": connection.execute("PRAGMA page_count").fetchone()[0],
        "generation_events": connection.execute("SELECT count(*) FROM generation_events").fetchone()[0],
        "activity_events": connection.execute("SELECT count(*) FROM activity_events").fetchone()[0],
        "chat_receipts": connection.execute("SELECT count(*) FROM chat_receipts").fetchone()[0],
        "retention_policies": policies,
        "recent_runs": recent,
    }


def _seed(connection):
    """Minimal finished turn plus an unfinished one, used only by --check."""
    old = "2020-01-01T00:00:00+00:00"
    connection.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?)", (old,))
    for request_id, state in (("done-1", "complete"), ("live-1", "generating")):
        connection.execute(
            "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,'s1','user','hi','complete',?)",
            (request_id, old),
        )
        answer_id = None
        if state == "complete":
            answer_id = f"{request_id}-answer"
            connection.execute(
                "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,'s1','assistant','hello','complete',?)",
                (answer_id, old),
            )
        connection.execute(
            "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,answer_id,captured_at,updated_at)"
            " VALUES(?,'s1','proj','m',?,0,?,?,?,?)",
            (request_id, f"sig-{request_id}", state, answer_id, old, old),
        )
        for index, piece in enumerate(("he", "ll", "o")):
            connection.execute(
                "INSERT INTO generation_events(request_id,session_id,state,content,created_at) VALUES(?,'s1','chunk',?,?)",
                (request_id, piece, old),
            )
        connection.execute(
            "INSERT INTO activity_events(request_id,session_id,kind,payload_json,created_at) VALUES(?,'s1','tool_started','{}',?)",
            (request_id, old),
        )
    connection.execute(
        "INSERT INTO generation_events(request_id,session_id,state,content,created_at) VALUES('done-1','s1','completed','',?)",
        (old,),
    )


def self_check() -> int:
    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "check.db"
        connection = sqlite3.connect(str(path), isolation_level=None)
        connection.execute("PRAGMA foreign_keys=ON")
        for name in sorted(p.name for p in MIGRATIONS.glob("*.sql")):
            connection.executescript((MIGRATIONS / name).read_text())
        assert connection.execute("PRAGMA user_version").fetchone()[0] == chain_head_version()
        _seed(connection)

        disabled = apply_retention(connection)
        assert all(entry["status"] == "disabled" for entry in disabled.values()), disabled
        assert connection.execute("SELECT count(*) FROM activity_events").fetchone()[0] == 2

        compaction = compact_generation_chunks(connection, older_than_days=1)
        assert compaction["requests_compacted"] == 1, compaction
        assert connection.execute(
            "SELECT content,compacted_chunks FROM generation_events WHERE request_id='done-1' AND state='chunk'"
        ).fetchall() == [("hello", 3)]
        assert connection.execute(
            "SELECT count(*) FROM generation_events WHERE request_id='live-1' AND state='chunk'"
        ).fetchone()[0] == 3, "unfinished turn must not be compacted"

        set_policy(connection, "generation_chunks", 1, True)
        set_policy(connection, "activity_events", 1, True)
        applied = apply_retention(connection)
        assert applied["generation_chunks"]["status"] == "applied", applied
        assert connection.execute(
            "SELECT count(*) FROM generation_events WHERE request_id='done-1' AND state='completed'"
        ).fetchone()[0] == 1, "terminal generation row must survive retention"
        assert connection.execute(
            "SELECT count(*) FROM generation_events WHERE request_id='live-1' AND state='chunk'"
        ).fetchone()[0] == 3, "unfinished turn must not be trimmed"
        assert connection.execute("SELECT count(*) FROM chat_receipts").fetchone()[0] == 2, "receipts must survive"
        assert connection.execute(
            "SELECT count(*) FROM activity_events WHERE request_id='live-1'"
        ).fetchone()[0] == 1

        result = checkpoint(connection)
        assert "incremental_vacuum" in result
        assert connection.execute("SELECT count(*) FROM maintenance_runs WHERE action='wal_checkpoint'").fetchone()[0] == 1
        assert connection.execute("PRAGMA foreign_key_check").fetchall() == []
        assert status(connection)["schema_version"] == chain_head_version()
        connection.close()
    print("maintenance gate OK: retention, compaction and checkpoint preserve receipts and live turns")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="Retention, WAL and compaction maintenance")
    parser.add_argument("command", nargs="?", default="status", choices=["status", "policy", "retention", "compact", "checkpoint", "run"])
    parser.add_argument("--database", default=str(DEFAULT_DATABASE))
    parser.add_argument("--name", choices=list(TARGETS))
    parser.add_argument("--keep-days", type=int)
    parser.add_argument("--enable", action="store_true")
    parser.add_argument("--disable", action="store_true")
    parser.add_argument("--older-than-days", type=int, default=1)
    parser.add_argument("--check", action="store_true", help="run the offline self-test and exit")
    args = parser.parse_args(argv)

    if args.check:
        return self_check()

    connection = connect(pathlib.Path(args.database))
    try:
        if args.command == "status":
            payload = status(connection)
        elif args.command == "policy":
            if not args.name:
                raise SystemExit("BLOCKED: --name is required for policy")
            if args.enable and args.disable:
                raise SystemExit("BLOCKED: choose either --enable or --disable")
            enabled = True if args.enable else (False if args.disable else None)
            payload = set_policy(connection, args.name, args.keep_days, enabled)
        elif args.command == "retention":
            payload = apply_retention(connection)
        elif args.command == "compact":
            payload = compact_generation_chunks(connection, args.older_than_days)
        elif args.command == "checkpoint":
            payload = checkpoint(connection)
        else:
            payload = {
                "retention": apply_retention(connection),
                "compaction": compact_generation_chunks(connection, args.older_than_days),
                "checkpoint": checkpoint(connection),
            }
    finally:
        connection.close()
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
