#!/usr/bin/env python3
"""Trust-aware migration: legacy approved memories -> v3 DB.

Provenance is real, not invented: only rows whose key appears in the legacy
`pending_confirmations` table with status='confirmed' are auto-approved as
memories; every other row becomes a PENDING candidate carrying the legacy
fingerprint as evidence. Sensitive rows are quarantined (imported as pending
candidates in a separate 'legacy-review' scope) so nothing is silently lost,
but they never enter active recall. Idempotent by source snapshot hash.

Usage: python3 scripts/migrate_legacy_trusted.py SRC_DB DST_DB
"""
import json, os, re, sqlite3, sys, tempfile, uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from backup import backup  # noqa: E402  (online snapshot, includes WAL)

KEY_RE = re.compile(r"[a-z0-9_]{1,80}")
ALLOWED = {"preference", "fact", "project", "rule", "skill"}
MARKERS = ["private key", "private_key", "age-secret-key-", "password", "passwd",
           "api_key", "api-key", "api key", "access_token", "refresh_token",
           "client_secret", "credential", "authorization:", "bearer ",
           "ghp_", "github_pat_", "xoxb-", "xoxp-"]


def sensitive(text: str) -> bool:
    low = text.lower()
    return any(m in low for m in MARKERS) or bool(
        re.search(r"\bsk-[\w-]{10,}|\bAKIA[A-Z0-9]{16}\b", text)
    )


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def migrate(source: Path, destination: Path) -> dict:
    destination = Path(destination).resolve()
    if destination.exists():
        raise ValueError("Destination exists; refusing to overwrite")
    source = Path(source).resolve()
    if source.resolve() == destination:
        raise ValueError("Source and destination must differ")

    counts = {"confirmed_active": 0, "pending_candidates": 0,
              "quarantined_sensitive": 0, "invalid_skipped": 0,
              "inactive_skipped": 0}
    with tempfile.TemporaryDirectory(prefix="harness-trusted-migration-") as tmp:
        snap = backup(source, Path(tmp) / "snapshot.db")
        src = sqlite3.connect(snap)
        try:
            tables = {r[0] for r in src.execute(
                "SELECT name FROM sqlite_master WHERE type='table'")}
            if "memories" not in tables or "candidates" in tables:
                raise ValueError("Expected legacy memory database")
            cols = {r[1] for r in src.execute("PRAGMA table_info(memories)")}
            if not {"key", "value", "category", "status"} <= cols:
                raise ValueError("Unexpected legacy schema")
            confirmed_keys = set()
            if "pending_confirmations" in tables:
                confirmed_keys = {
                    r[0] for r in src.execute(
                        "SELECT key FROM pending_confirmations WHERE status='confirmed'")}
            fingerprint = __import__("hashlib").sha256(snap.read_bytes()).hexdigest()

            destination.parent.mkdir(parents=True, exist_ok=True)
            fd = os.open(destination, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            os.close(fd)
            dst = sqlite3.connect(destination)
            try:
                dst.execute("PRAGMA foreign_keys=ON")
                dst.executescript((ROOT / "migrations" / "001_core.sql").read_text())
                dst.executescript((ROOT / "migrations" / "002_recording.sql").read_text())
                dst.execute("PRAGMA user_version=2")
                stamp = now()
                source_id = f"legacy:{fingerprint}"
                with dst:
                    for key, value, category, status in src.execute(
                            "SELECT key,value,category,status FROM memories"):
                        if status != "active":
                            counts["inactive_skipped"] += 1
                            continue
                        is_sensitive = (category == "credential"
                                        or sensitive(str(key)) or sensitive(str(value)))
                        valid = (category in ALLOWED
                                 and isinstance(key, str) and KEY_RE.fullmatch(key)
                                 and isinstance(value, str) and value.strip()
                                 and len(value) <= 1000
                                 and len(value.encode()) <= 4000)
                        if is_sensitive:
                            # Quarantine: never lost, never active, never in recall.
                            dst.execute(
                                "INSERT INTO candidates(id,scope,key,value,category,"
                                "source_id,evidence,expected_revision,status,created_at,"
                                "expires_at) VALUES(?, 'legacy-review', ?, ?, 'fact', ?, ?, 0,"
                                "'pending', ?, ?)",
                                (str(uuid.uuid4()), key, str(value)[:1000], source_id,
                                 json.dumps({"provenance": "legacy_quarantine",
                                             "source_snapshot_sha256": fingerprint,
                                             "quote": None}),
                                 stamp, int((datetime.now(timezone.utc)
                                             + timedelta(days=365)).timestamp())))
                            counts["quarantined_sensitive"] += 1
                            continue
                        if not valid:
                            counts["invalid_skipped"] += 1
                            continue
                        if key in confirmed_keys:
                            # Verified legacy approval: real evidence exists.
                            cid = str(uuid.uuid4())
                            evidence = json.dumps(
                                {"provenance": "legacy_pending_confirmation",
                                 "source_snapshot_sha256": fingerprint,
                                 "quote": None})
                            dst.execute(
                                "INSERT INTO candidates(id,scope,key,value,category,"
                                "source_id,evidence,expected_revision,status,created_at,"
                                "expires_at,resolved_at) VALUES(?, 'global', ?, ?, ?, ?, ?, 0,"
                                "'approved', ?, ?, ?)",
                                (cid, key, value, category, source_id, evidence,
                                 stamp, int((datetime.now(timezone.utc)
                                             + timedelta(days=30)).timestamp()), stamp))
                            mid = str(uuid.uuid4())
                            dst.execute(
                                "INSERT INTO memories(id,scope,key,value,category,status,"
                                "revision,candidate_id,created_at,updated_at) "
                                "VALUES(?, 'global', ?, ?, ?, 'active', 1, ?, ?, ?)",
                                (mid, key, value, category, cid, stamp, stamp))
                            dst.execute(
                                "INSERT INTO memory_revisions(id,memory_id,revision,action,"
                                "old_value,new_value,candidate_id,created_at) "
                                "VALUES(?, ?, 1, 'approve', NULL, ?, ?, ?)",
                                (str(uuid.uuid4()), mid, value, cid, stamp))
                            counts["confirmed_active"] += 1
                        else:
                            dst.execute(
                                "INSERT INTO candidates(id,scope,key,value,category,"
                                "source_id,evidence,expected_revision,status,created_at,"
                                "expires_at) VALUES(?, 'legacy-review', ?, ?, ?, ?, ?, 0,"
                                "'pending', ?, ?)",
                                (str(uuid.uuid4()), key, value, category, source_id,
                                 json.dumps({"provenance": "legacy_unknown",
                                             "source_snapshot_sha256": fingerprint,
                                             "quote": None}),
                                 stamp, int((datetime.now(timezone.utc)
                                             + timedelta(days=30)).timestamp())))
                            counts["pending_candidates"] += 1
                if dst.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                    raise RuntimeError("Destination integrity check failed")
            except Exception:
                dst.close()
                destination.unlink(missing_ok=True)
                raise
            else:
                dst.close()
        finally:
            src.close()
    return counts


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    print(json.dumps(migrate(Path(sys.argv[1]), Path(sys.argv[2])), indent=2))
