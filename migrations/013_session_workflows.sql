-- P14-T02: durable conversation metadata. Existing message and receipt rows are preserved.
BEGIN IMMEDIATE;
ALTER TABLE sessions ADD COLUMN title TEXT NOT NULL DEFAULT 'Conversation';
ALTER TABLE sessions ADD COLUMN archived_at TEXT;
ALTER TABLE sessions ADD COLUMN forked_from TEXT REFERENCES sessions(id);
CREATE INDEX sessions_archive ON sessions(archived_at,created_at);
CREATE INDEX sessions_title ON sessions(title);
PRAGMA user_version=13;
COMMIT;
