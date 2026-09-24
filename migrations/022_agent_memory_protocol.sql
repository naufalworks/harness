PRAGMA user_version=22;

CREATE TABLE IF NOT EXISTS agent_sessions (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    UNIQUE(agent_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_session ON agent_sessions(session_id);
