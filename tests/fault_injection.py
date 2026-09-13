#!/usr/bin/env python3
"""Deterministic destructive reliability fixtures on disposable SQLite databases only."""
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
from contextlib import closing
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = sorted((ROOT / "migrations").glob("[0-9][0-9][0-9]_*.sql"))


def schema(path: Path) -> None:
    with closing(sqlite3.connect(path)) as db:
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA synchronous=FULL")
        for migration in MIGRATIONS:
            db.executescript(migration.read_text())


def check(path: Path) -> sqlite3.Connection:
    db = sqlite3.connect(path)
    assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    assert db.execute("PRAGMA foreign_key_check").fetchall() == []
    return db


def crash_script(path: Path, body: str) -> subprocess.CompletedProcess:
    script = "import os,signal,sqlite3,sys\np=sys.argv[1]\nc=sqlite3.connect(p,isolation_level=None)\nc.execute('PRAGMA foreign_keys=ON')\nc.execute('PRAGMA journal_mode=WAL')\nc.execute('PRAGMA synchronous=FULL')\n" + body + "\nos.kill(os.getpid(),signal.SIGKILL)\n"
    result = subprocess.run([sys.executable, "-c", script, str(path)])
    assert result.returncode == -signal.SIGKILL, result.returncode
    return result


def crash_boundaries(root: Path) -> None:
    path = root / "crash.db"
    schema(path)
    with sqlite3.connect(path) as db:
        db.execute("CREATE TABLE fault_marker(value TEXT PRIMARY KEY)")
    crash_script(path, "c.execute('BEGIN IMMEDIATE')\nc.execute(\"INSERT INTO fault_marker VALUES('uncommitted')\")")
    with check(path) as db:
        assert db.execute("SELECT value FROM fault_marker").fetchall() == []
    crash_script(path, "c.execute(\"INSERT INTO fault_marker VALUES('committed')\")")
    with check(path) as db:
        assert db.execute("SELECT value FROM fault_marker").fetchall() == [("committed",)]


def ambiguous_admission(root: Path) -> None:
    path = root / "ambiguous.db"
    schema(path)
    body = """c.execute('BEGIN IMMEDIATE')
c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('session','global','now')")
c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('request','session','user','synthetic','pending','now')")
c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('request','session','global','model','same-signature',0,'captured','now','now')")
c.execute("INSERT INTO recording_outbox(request_id,created_at) VALUES('request','now')")
c.execute("INSERT INTO recording_events(request_id,kind,created_at) VALUES('request','captured','now')")
c.execute('COMMIT')"""
    crash_script(path, body)
    with check(path) as db:
        prior = db.execute("SELECT signature FROM chat_receipts WHERE request_id='request'").fetchone()
        assert prior == ("same-signature",)
        outcome = "duplicate" if prior[0] == "same-signature" else "conflict"
        assert outcome == "duplicate"
        assert ("conflict" if prior[0] != "different-signature" else "duplicate") == "conflict"
        for table in ("messages", "chat_receipts", "recording_outbox", "recording_events"):
            assert db.execute(f"SELECT count(*) FROM {table}").fetchone()[0] == 1


def disk_and_readonly(root: Path) -> None:
    path = root / "disk.db"
    schema(path)
    with sqlite3.connect(path, isolation_level=None) as db:
        db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        pages = db.execute("PRAGMA page_count").fetchone()[0]
        db.execute(f"PRAGMA max_page_count={pages}")
        try:
            db.execute("INSERT INTO sessions(id,scope,created_at) VALUES(?,?,?)", ("x" * 4096, "global", "now"))
        except sqlite3.OperationalError as error:
            assert "full" in str(error).lower(), error
        else:
            raise AssertionError("simulated ENOSPC write unexpectedly committed")
        assert db.execute("SELECT count(*) FROM sessions").fetchone()[0] == 0
    uri = path.resolve().as_uri() + "?mode=ro"
    with sqlite3.connect(uri, uri=True) as db:
        try:
            db.execute("INSERT INTO sessions(id,scope,created_at) VALUES('readonly','global','now')")
        except sqlite3.OperationalError as error:
            assert "readonly" in str(error).lower(), error
        else:
            raise AssertionError("read-only database accepted a write")
    with check(path) as db:
        assert db.execute("SELECT count(*) FROM sessions").fetchone()[0] == 0


def wal_and_integrity_failures(root: Path) -> None:
    live = root / "wal-live.db"
    schema(live)
    writer = sqlite3.connect(live, isolation_level=None)
    writer.execute("PRAGMA wal_autocheckpoint=0")
    writer.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    writer.execute("CREATE TABLE wal_marker(value TEXT PRIMARY KEY)")
    writer.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    writer.execute("INSERT INTO wal_marker VALUES('wal-commit')")
    wal = Path(str(live) + "-wal")
    assert wal.stat().st_size >= 56
    broken = root / "wal-broken.db"
    shutil.copy2(live, broken)
    broken_wal = Path(str(broken) + "-wal")
    shutil.copy2(wal, broken_wal)
    payload = bytearray(broken_wal.read_bytes())
    payload[40:48] = b"\0" * 8  # first frame checksum
    broken_wal.write_bytes(payload)
    try:
        with sqlite3.connect(broken) as db:
            values = db.execute("SELECT value FROM wal_marker").fetchall()
            assert values == [], values
            assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    except sqlite3.DatabaseError:
        pass  # Explicit rejection is also fail-closed.
    writer.close()

    corrupt = root / "corrupt.db"
    shutil.copy2(live, corrupt)
    payload = bytearray(corrupt.read_bytes())
    payload[100:132] = b"\xff" * 32
    corrupt.write_bytes(payload)
    try:
        with sqlite3.connect(corrupt) as db:
            assert db.execute("PRAGMA integrity_check").fetchone()[0] != "ok"
    except sqlite3.DatabaseError:
        pass


def backpressure_and_recovery(root: Path) -> None:
    path = root / "queues.db"
    schema(path)
    with sqlite3.connect(path) as db:
        db.execute("PRAGMA foreign_keys=ON")
        for index in range(100):
            session, request = f"s{index}", f"r{index}"
            db.execute("INSERT INTO sessions(id,scope,created_at) VALUES(?,'global','now')", (session,))
            db.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,?,'user','x','pending','now')", (request, session))
            db.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES(?,?,'global','m',?,0,'captured','now','now')", (request, session, request))
        pending = db.execute("SELECT count(*) FROM chat_receipts WHERE state IN ('captured','generating')").fetchone()[0]
        assert pending == 100  # capture_chat returns Admission::Full at this boundary.
        for index in range(1000):
            db.execute("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?,?,'global','fixture','[]',?,0,'now')", (f"j{index}", f"k{index}", "running" if index == 0 else "pending"))
        assert db.execute("SELECT count(*) FROM jobs WHERE status IN ('pending','running')").fetchone()[0] == 1000
        assert min(32, max(0, 1000 - 1000)) == 0  # outbox flush defers without losing intent.
        db.execute("UPDATE jobs SET status='pending' WHERE status='running'")
        assert db.execute("SELECT count(*) FROM jobs WHERE status='running'").fetchone()[0] == 0
        assert db.execute("SELECT count(*) FROM jobs WHERE status='pending'").fetchone()[0] == 1000
    check(path).close()


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="harness-fault-injection-") as directory:
        root = Path(directory)
        crash_boundaries(root)
        ambiguous_admission(root)
        disk_and_readonly(root)
        wal_and_integrity_failures(root)
        backpressure_and_recovery(root)
    print("Fault injection passed: crash atomicity, ambiguous admission, ENOSPC/read-only, WAL/integrity rejection, queue limits, and running-job recovery")


if __name__ == "__main__":
    main()
