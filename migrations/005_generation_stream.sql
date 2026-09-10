-- 005: durable assistant generation stream. Additive only.
BEGIN IMMEDIATE;

CREATE TABLE generation_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 session_id TEXT NOT NULL REFERENCES sessions(id),
 state TEXT NOT NULL CHECK(state IN ('chunk','completed','interrupted','failed')),
 content TEXT NOT NULL DEFAULT '',
 error_code TEXT,
 created_at TEXT NOT NULL
);
CREATE INDEX generation_events_session ON generation_events(session_id, seq);
CREATE INDEX generation_events_request ON generation_events(request_id, seq);

PRAGMA user_version=5;
COMMIT;
