-- Additive upgrade. Existing messages are NOT assigned invented receipts.
BEGIN IMMEDIATE;
CREATE TABLE chat_receipts (
 request_id TEXT PRIMARY KEY REFERENCES messages(id),
 session_id TEXT NOT NULL REFERENCES sessions(id),
 scope TEXT NOT NULL,
 model TEXT NOT NULL,
 signature TEXT NOT NULL,
 redacted INTEGER NOT NULL CHECK(redacted IN (0,1)),
 state TEXT NOT NULL CHECK(state IN ('captured','generating','complete','failed','interrupted')),
 context_json TEXT,
 answer_id TEXT UNIQUE REFERENCES messages(id),
 error_code TEXT,
 captured_at TEXT NOT NULL,
 updated_at TEXT NOT NULL,
 CHECK((state='complete' AND answer_id IS NOT NULL) OR (state!='complete' AND answer_id IS NULL))
);
CREATE INDEX chat_receipts_ready ON chat_receipts(state,captured_at);
CREATE TABLE recording_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 kind TEXT NOT NULL CHECK(kind IN ('captured','generation_started','context_saved','answer_saved','generation_failed','interrupted','extraction_queued')),
 created_at TEXT NOT NULL
);
CREATE INDEX recording_events_request ON recording_events(request_id,seq);
-- Durable intent, not a model job. Queue pressure must NEVER undo a chat save.
CREATE TABLE recording_outbox (
 request_id TEXT PRIMARY KEY REFERENCES chat_receipts(request_id),
 job_id TEXT UNIQUE REFERENCES jobs(id),
 created_at TEXT NOT NULL
);
CREATE TRIGGER receipt_context_write_once BEFORE UPDATE OF context_json ON chat_receipts
 WHEN OLD.context_json IS NOT NULL AND NEW.context_json IS NOT OLD.context_json
 BEGIN SELECT RAISE(ABORT,'recorded context cannot be rewritten'); END;
PRAGMA user_version=2;
COMMIT;
