BEGIN IMMEDIATE;
CREATE TABLE sessions (id TEXT PRIMARY KEY, scope TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE messages (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
 session_id TEXT NOT NULL REFERENCES sessions(id), role TEXT NOT NULL CHECK(role IN ('user','assistant')),
 content TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending','complete','failed')),
 created_at TEXT NOT NULL
);
CREATE INDEX messages_session ON messages(session_id,seq);
CREATE TABLE sources (
 id TEXT PRIMARY KEY, scope TEXT NOT NULL, name TEXT NOT NULL, format TEXT NOT NULL,
 fingerprint TEXT NOT NULL, parser_version TEXT NOT NULL, content TEXT NOT NULL,
 warnings TEXT NOT NULL, created_at TEXT NOT NULL,
 UNIQUE(scope,fingerprint,parser_version)
);
CREATE TABLE jobs (
 id TEXT PRIMARY KEY, job_key TEXT NOT NULL UNIQUE, scope TEXT NOT NULL,
 source_id TEXT NOT NULL, payload TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','running','done','failed')),
 attempts INTEGER NOT NULL DEFAULT 0, available_at INTEGER NOT NULL,
 last_error TEXT, created_at TEXT NOT NULL
);
CREATE INDEX jobs_ready ON jobs(status,available_at);
CREATE TABLE candidates (
 id TEXT PRIMARY KEY, scope TEXT NOT NULL, key TEXT NOT NULL CHECK(length(key) BETWEEN 1 AND 80),
 value TEXT NOT NULL CHECK(length(value) BETWEEN 1 AND 1000),
 category TEXT NOT NULL CHECK(category IN ('preference','fact','project','rule','skill')),
 source_id TEXT NOT NULL, evidence TEXT NOT NULL,
 expected_revision INTEGER NOT NULL CHECK(expected_revision>=0),
 status TEXT NOT NULL CHECK(status IN ('pending','approved','rejected','conflict','expired')),
 created_at TEXT NOT NULL, expires_at INTEGER NOT NULL, resolved_at TEXT,
 UNIQUE(scope,key,value,source_id)
);
CREATE INDEX candidates_pending ON candidates(status,scope,created_at);
CREATE TABLE memories (
 rowid INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
 scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
 category TEXT NOT NULL CHECK(category IN ('preference','fact','project','rule','skill')),
 status TEXT NOT NULL CHECK(status IN ('active','archived')),
 revision INTEGER NOT NULL CHECK(revision>0),
 candidate_id TEXT NOT NULL REFERENCES candidates(id),
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 UNIQUE(scope,key)
);
CREATE TABLE memory_revisions (
 id TEXT PRIMARY KEY, memory_id TEXT NOT NULL REFERENCES memories(id),
 revision INTEGER NOT NULL, action TEXT NOT NULL CHECK(action IN ('approve','archive')),
 old_value TEXT, new_value TEXT NOT NULL,
 candidate_id TEXT NOT NULL REFERENCES candidates(id), created_at TEXT NOT NULL,
 UNIQUE(memory_id,revision)
);
CREATE VIRTUAL TABLE memory_fts USING fts5(key,value,content='memories',content_rowid='rowid',tokenize='unicode61');
CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
 INSERT INTO memory_fts(rowid,key,value) VALUES(new.rowid,new.key,new.value);
END;
CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
 INSERT INTO memory_fts(memory_fts,rowid,key,value) VALUES('delete',old.rowid,old.key,old.value);
END;
CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
 INSERT INTO memory_fts(memory_fts,rowid,key,value) VALUES('delete',old.rowid,old.key,old.value);
 INSERT INTO memory_fts(rowid,key,value) VALUES(new.rowid,new.key,new.value);
END;
CREATE TABLE settings (key TEXT PRIMARY KEY,value TEXT NOT NULL);
PRAGMA user_version=1;
COMMIT;
