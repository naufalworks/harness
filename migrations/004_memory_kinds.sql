-- 004: widen durable memory kinds and add storage for deterministic local embeddings.
-- The three related tables are rebuilt together so every existing id, rowid, revision, and
-- evidence link survives. Foreign-key enforcement is disabled only around this one transaction
-- and is restored before the migration returns.
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

DROP TRIGGER memories_ai;
DROP TRIGGER memories_ad;
DROP TRIGGER memories_au;
DROP TABLE memory_fts;
DROP INDEX candidates_pending;

ALTER TABLE candidates RENAME TO candidates_v3;
ALTER TABLE memories RENAME TO memories_v3;
ALTER TABLE memory_revisions RENAME TO memory_revisions_v3;

CREATE TABLE candidates (
 id TEXT PRIMARY KEY, scope TEXT NOT NULL, key TEXT NOT NULL CHECK(length(key) BETWEEN 1 AND 80),
 value TEXT NOT NULL CHECK(length(value) BETWEEN 1 AND 1000),
 category TEXT NOT NULL CHECK(category IN ('preference','fact','project','rule','skill','decision','episodic','procedural')),
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
 category TEXT NOT NULL CHECK(category IN ('preference','fact','project','rule','skill','decision','episodic','procedural')),
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

INSERT INTO candidates SELECT * FROM candidates_v3;
INSERT INTO memories(rowid,id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at)
 SELECT rowid,id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at FROM memories_v3;
INSERT INTO memory_revisions SELECT * FROM memory_revisions_v3;

DROP TABLE memory_revisions_v3;
DROP TABLE memories_v3;
DROP TABLE candidates_v3;

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
INSERT INTO memory_fts(memory_fts) VALUES('rebuild');

CREATE TABLE memory_embeddings (
 memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
 model TEXT NOT NULL,
 dimensions INTEGER NOT NULL CHECK(dimensions BETWEEN 8 AND 4096),
 vector BLOB NOT NULL,
 content_hash TEXT NOT NULL,
 recall_count INTEGER NOT NULL DEFAULT 0 CHECK(recall_count>=0),
 useful_count INTEGER NOT NULL DEFAULT 0 CHECK(useful_count>=0),
 updated_at TEXT NOT NULL
);
CREATE INDEX memory_embeddings_model ON memory_embeddings(model,dimensions);

PRAGMA user_version=4;
COMMIT;
PRAGMA foreign_keys=ON;
