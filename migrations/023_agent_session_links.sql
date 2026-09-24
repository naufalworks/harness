PRAGMA user_version=23;

CREATE TABLE agent_session_links (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL CHECK(length(agent_id) BETWEEN 1 AND 64),
    agent_session_id TEXT NOT NULL CHECK(length(agent_session_id) BETWEEN 1 AND 128),
    harness_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 128),
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    UNIQUE(agent_id, agent_session_id)
);

CREATE INDEX agent_session_links_harness
    ON agent_session_links(harness_session_id, last_seen_at);
CREATE INDEX agent_session_links_scope
    ON agent_session_links(scope, last_seen_at);

-- Preserve links produced by the initial P20 schema. The original table did not carry
-- a distinct external agent-session id, so its session_id is the least-surprising
-- backfill for both sides of the link.
INSERT OR IGNORE INTO agent_session_links(
    id, agent_id, agent_session_id, harness_session_id, scope, created_at, last_seen_at
)
SELECT a.id, a.agent_id, a.session_id, a.session_id, s.scope, a.created_at, a.last_seen_at
FROM agent_sessions a
JOIN sessions s ON s.id = a.session_id;
