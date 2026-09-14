#!/usr/bin/env python3
"""Repeatable storage benchmark gate for Harness.

The gate validates required indexes and representative SQLite query plans.
"""

from __future__ import annotations

import argparse
import sqlite3
import sys
import time
from contextlib import closing
from pathlib import Path

DEFAULT_SESSIONS = 10000
DEFAULT_EVENTS = 100000


REQUIRED_INDEXES = {
    "messages_session",
    "jobs_ready",
    "turn_steps_request",
    "activity_events_session",
    "generation_events_request",
    "recording_events_request",
}


def load_schema(db: sqlite3.Connection) -> None:
    root = Path(__file__).resolve().parents[1]
    for migration in sorted((root / "migrations").glob("*.sql")):
        db.executescript(migration.read_text())


def check_indexes(db: sqlite3.Connection) -> None:
    found = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='index'")}
    missing = REQUIRED_INDEXES - found
    if missing:
        raise SystemExit(f"missing indexes: {sorted(missing)}")


def check_query_plans(db: sqlite3.Connection) -> None:
    plan = " ".join(
        row[3]
        for row in db.execute(
            "EXPLAIN QUERY PLAN SELECT id FROM messages WHERE session_id=? ORDER BY seq LIMIT 100",
            ("benchmark-session",),
        )
    )
    if "messages_session" not in plan and "USING INDEX" not in plan:
        raise SystemExit(f"messages query plan is not indexed: {plan}")


def run_scale_smoke(db: sqlite3.Connection, sessions: int, events: int) -> None:
    """Run a small deterministic storage budget check."""
    started = time.monotonic()
    db.executemany(
        "INSERT INTO sessions(id,scope,created_at) VALUES(?,?,?)",
        [(f"bench-{i}", "global", "2026-01-01") for i in range(sessions)],
    )
    db.executemany(
        "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,?,?,?,?,?)",
        [(f"msg-{i}", f"bench-{i % sessions}", "user", "benchmark", "complete", "2026-01-01") for i in range(events)],
    )
    rows = db.execute(
        "SELECT id FROM messages WHERE session_id=? ORDER BY seq LIMIT 100",
        ("bench-1",),
    ).fetchall()
    if len(rows) == 0:
        raise SystemExit(f"unexpected benchmark rows: {len(rows)}")
    if time.monotonic() - started > 1:
        raise SystemExit("storage smoke budget exceeded")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--sessions", type=int, default=DEFAULT_SESSIONS)
    parser.add_argument("--events", type=int, default=DEFAULT_EVENTS)
    args = parser.parse_args()

    with closing(sqlite3.connect(":memory:")) as db:
        load_schema(db)

        if args.check:
            check_indexes(db)
            check_query_plans(db)
            run_scale_smoke(db, args.sessions, args.events)
            print("benchmark gate OK: indexed plans available")
            return 0

    print("Use --check for the storage benchmark gate")
    return 0


if __name__ == "__main__":
    sys.exit(main())
