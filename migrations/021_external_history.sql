-- P19-T03: a committed row is both immutable event evidence and durable receipt.
-- No link to chat, tools, jobs or provider execution. No extraction intent is created.
BEGIN IMMEDIATE;
CREATE TABLE external_history_events (
 receipt_id TEXT PRIMARY KEY NOT NULL,
 producer_id TEXT NOT NULL,
 event_id TEXT NOT NULL,
 project_id TEXT NOT NULL,
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 producer_instance_id TEXT NOT NULL,
 producer_sequence INTEGER NOT NULL CHECK(producer_sequence>0),
 logical_session_id TEXT NOT NULL,
 event_type TEXT NOT NULL,
 occurred_at TEXT NOT NULL,
 ingested_at TEXT NOT NULL,
 envelope TEXT NOT NULL CHECK(json_valid(envelope)),
 UNIQUE(producer_id,event_id)
);
CREATE INDEX external_history_session ON external_history_events(producer_id,project_id,logical_session_id,producer_instance_id,producer_sequence);
CREATE TRIGGER external_history_no_update BEFORE UPDATE ON external_history_events BEGIN SELECT RAISE(ABORT,'external history is append-only'); END;
CREATE TRIGGER external_history_no_delete BEFORE DELETE ON external_history_events BEGIN SELECT RAISE(ABORT,'external history is append-only'); END;
PRAGMA user_version=21;
COMMIT;
