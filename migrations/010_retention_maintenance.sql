-- 010: configured retention, generation-chunk compaction and maintenance evidence.
-- Additive only. Receipts (chat_receipts), provenance_edges, memories and privacy/archive
-- rows are never targets of retention; only derived chunk/activity rows can be trimmed.
BEGIN IMMEDIATE;

CREATE TABLE retention_policies (
 name TEXT PRIMARY KEY CHECK(name IN ('generation_chunks','activity_events')),
 keep_days INTEGER NOT NULL CHECK(keep_days >= 1),
 enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
 updated_at TEXT NOT NULL
);

CREATE TABLE maintenance_runs (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 action TEXT NOT NULL CHECK(action IN ('retention','compaction','wal_checkpoint')),
 target TEXT NOT NULL,
 rows_affected INTEGER NOT NULL DEFAULT 0 CHECK(rows_affected >= 0),
 wal_pages INTEGER,
 checkpointed_pages INTEGER,
 freelist_pages INTEGER,
 detail TEXT,
 started_at TEXT NOT NULL,
 finished_at TEXT NOT NULL
);
CREATE INDEX maintenance_runs_action ON maintenance_runs(action, id);

-- How many original chunk rows a surviving chunk row now represents. 0 means never compacted.
ALTER TABLE generation_events ADD COLUMN compacted_chunks INTEGER NOT NULL DEFAULT 0;

-- Disabled by default: retention only deletes after an owner enables a policy.
INSERT INTO retention_policies(name,keep_days,enabled,updated_at) VALUES
 ('generation_chunks',30,0,'1970-01-01T00:00:00Z'),
 ('activity_events',90,0,'1970-01-01T00:00:00Z');

PRAGMA user_version=10;
COMMIT;
