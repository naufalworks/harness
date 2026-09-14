-- 009: durable cancellation intent and safe-boundary retry lineage.
BEGIN IMMEDIATE;
CREATE TABLE run_controls (
 request_id TEXT PRIMARY KEY REFERENCES chat_receipts(request_id),
 cancel_requested_at TEXT,
 cancelled_at TEXT,
 safe_boundary_seq INTEGER,
 retry_of TEXT REFERENCES chat_receipts(request_id),
 retried_by TEXT UNIQUE REFERENCES chat_receipts(request_id),
 CHECK(cancelled_at IS NULL OR cancel_requested_at IS NOT NULL),
 CHECK(retry_of IS NULL OR retry_of <> request_id),
 CHECK(retried_by IS NULL OR retried_by <> request_id)
);
CREATE INDEX run_controls_retry_of ON run_controls(retry_of);
PRAGMA user_version=9;
COMMIT;
