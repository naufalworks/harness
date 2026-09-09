-- 003: agentic turn. Additive only. See docs/design/agentic-turn.md#schema.
-- Existing tables are untouched; memories.category widening is deferred to 004.
BEGIN IMMEDIATE;

CREATE TABLE scopes (
 scope TEXT PRIMARY KEY,
 root_path TEXT,
 permission_mode TEXT NOT NULL DEFAULT 'ask' CHECK(permission_mode IN ('ask','auto_edit','auto_all')),
 diagnostics_cmd TEXT CHECK(diagnostics_cmd IS NULL OR length(diagnostics_cmd) <= 512),
 max_steps INTEGER CHECK(max_steps IS NULL OR max_steps BETWEEN 1 AND 500),
 max_tool_bytes INTEGER CHECK(max_tool_bytes IS NULL OR max_tool_bytes BETWEEN 1024 AND 50000000),
 max_wall_seconds INTEGER CHECK(max_wall_seconds IS NULL OR max_wall_seconds BETWEEN 10 AND 86400),
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);

CREATE TABLE turn_steps (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 parent_step_id TEXT REFERENCES turn_steps(id),
 seq INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('model_call','tool_call','permission_wait','compaction','verification','subagent')),
 status TEXT NOT NULL CHECK(status IN ('running','complete','failed','denied','interrupted')),
 tool_name TEXT,
 tool_call_id TEXT,
 input_json TEXT,
 output_json TEXT,
 output_bytes INTEGER NOT NULL DEFAULT 0,
 truncated INTEGER NOT NULL DEFAULT 0 CHECK(truncated IN (0,1)),
 tokens_in INTEGER,
 tokens_out INTEGER,
 error_code TEXT,
 started_at TEXT NOT NULL,
 finished_at TEXT,
 UNIQUE(request_id, seq)
);
CREATE INDEX turn_steps_request ON turn_steps(request_id, seq);
CREATE INDEX turn_steps_running ON turn_steps(status) WHERE status='running';

-- Open vocabulary feed for the UI / SSE. recording_events keeps its fixed CHECK.
CREATE TABLE activity_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 session_id TEXT NOT NULL REFERENCES sessions(id),
 step_id TEXT REFERENCES turn_steps(id),
 kind TEXT NOT NULL CHECK(length(kind) BETWEEN 1 AND 64),
 payload_json TEXT NOT NULL DEFAULT '{}',
 created_at TEXT NOT NULL
);
CREATE INDEX activity_events_session ON activity_events(session_id, seq);

CREATE TABLE permission_requests (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 step_id TEXT NOT NULL REFERENCES turn_steps(id),
 tool_name TEXT NOT NULL,
 summary TEXT NOT NULL,
 args_json TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','approved','denied','expired')),
 created_at TEXT NOT NULL,
 expires_at TEXT NOT NULL,
 resolved_at TEXT
);
CREATE INDEX permission_requests_pending ON permission_requests(status) WHERE status='pending';

CREATE TABLE file_changes (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 step_id TEXT NOT NULL REFERENCES turn_steps(id),
 path TEXT NOT NULL,
 action TEXT NOT NULL CHECK(action IN ('create','modify','delete')),
 before_hash TEXT,
 after_hash TEXT,
 diff TEXT NOT NULL,
 applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1)),
 reverted_at TEXT,
 created_at TEXT NOT NULL
);
CREATE INDEX file_changes_request ON file_changes(request_id);

CREATE TABLE plan_items (
 id TEXT PRIMARY KEY,
 session_id TEXT NOT NULL REFERENCES sessions(id),
 seq INTEGER NOT NULL,
 text TEXT NOT NULL CHECK(length(text) BETWEEN 1 AND 200),
 status TEXT NOT NULL CHECK(status IN ('pending','in_progress','done','failed')),
 updated_at TEXT NOT NULL,
 UNIQUE(session_id, seq)
);

PRAGMA user_version=3;
COMMIT;
