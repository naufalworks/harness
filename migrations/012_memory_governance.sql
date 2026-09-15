-- 012: memory governance and timelines (P15-T03).
--
-- Everything here is additive to the *evidence*: no existing row is deleted and no historical
-- revision is rewritten. `memories` and `memory_revisions` are rebuilt because two invariants
-- had to change shape:
--   * UNIQUE(scope,key) becomes UNIQUE(scope,key,branch), so a branch can hold an alternative
--     value for the same key without overwriting the reviewed one.
--   * memory_revisions.action gains governance actions, and its UNIQUE(memory_id,revision)
--     becomes a PARTIAL unique index over the two value-changing actions only. Governance acts
--     (pin, expire, merge, branch, supersede) do not mint a new value revision, so several of
--     them can legitimately share one revision; the original invariant is preserved exactly
--     where it applied.
--
-- Foreign keys are disabled only around this one transaction and restored before it returns,
-- the same pattern migration 004 used for the same three-table rebuild.
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

DROP TRIGGER memories_ai;
DROP TRIGGER memories_ad;
DROP TRIGGER memories_au;
DROP TABLE memory_fts;

-- Migration 006 added two triggers that name `memories` in their bodies. Modern SQLite rewrites
-- such references when a table is renamed, so an unguarded rename would silently repoint them at
-- the temporary `memories_v11` and leave them dangling once it is dropped. They are dropped here
-- and recreated verbatim below.
DROP TRIGGER provenance_edges_validate;
DROP TRIGGER provenance_memory_delete;

ALTER TABLE memories RENAME TO memories_v11;
ALTER TABLE memory_revisions RENAME TO memory_revisions_v11;

CREATE TABLE memories (
 rowid INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
 scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
 -- Alternative reviewed value for the same key. 'main' is the reviewed default.
 branch TEXT NOT NULL DEFAULT 'main' CHECK(length(branch) BETWEEN 1 AND 60),
 category TEXT NOT NULL CHECK(category IN ('preference','fact','project','rule','skill','decision','episodic','procedural')),
 -- 'superseded' and 'expired' are retained, never deleted: a lapsed or replaced memory keeps
 -- its value and its revision chain and simply stops being recalled.
 status TEXT NOT NULL CHECK(status IN ('active','archived','superseded','expired')),
 revision INTEGER NOT NULL CHECK(revision>0),
 candidate_id TEXT NOT NULL REFERENCES candidates(id),
 -- Explicitly pinned profile entry. Pinning is an owner act, never inferred from usage.
 pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0,1)),
 -- Temporary memory: unix seconds after which recall must stop using it. NULL means durable.
 expires_at INTEGER CHECK(expires_at IS NULL OR expires_at>0),
 -- Conflict/decision group this row belongs to; joins memory_decisions.group_id.
 conflict_group TEXT,
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 -- Deduplication key. A generated column rather than a stored hash so legacy rows are covered
 -- immediately and no backfill can claim a value it never computed. SQLite lower() is
 -- ASCII-only, which is why dedup is offered as a reviewable suggestion, not an auto-merge.
 dedup_key TEXT GENERATED ALWAYS AS (lower(trim(value))) VIRTUAL,
 UNIQUE(scope,key,branch)
);

INSERT INTO memories(rowid,id,scope,key,value,branch,category,status,revision,candidate_id,pinned,expires_at,conflict_group,created_at,updated_at)
 SELECT rowid,id,scope,key,value,'main',category,status,revision,candidate_id,0,NULL,NULL,created_at,updated_at FROM memories_v11;

CREATE INDEX memories_dedup ON memories(scope,dedup_key);
CREATE INDEX memories_expiry ON memories(status,expires_at);
CREATE INDEX memories_pinned ON memories(pinned,scope);

CREATE TABLE memory_revisions (
 id TEXT PRIMARY KEY, memory_id TEXT NOT NULL REFERENCES memories(id),
 revision INTEGER NOT NULL,
 action TEXT NOT NULL CHECK(action IN ('approve','archive','branch','supersede','expire','merge','pin','unpin')),
 old_value TEXT, new_value TEXT NOT NULL,
 candidate_id TEXT NOT NULL REFERENCES candidates(id), created_at TEXT NOT NULL,
 -- Free-text evidence for governance acts (which row absorbed a duplicate, why a memory
 -- lapsed). NULL for the two original value actions.
 detail TEXT
);

INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,detail)
 SELECT id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,NULL FROM memory_revisions_v11;

-- The original UNIQUE(memory_id,revision), narrowed to the actions it was actually about.
CREATE UNIQUE INDEX memory_revisions_value ON memory_revisions(memory_id,revision)
 WHERE action IN ('approve','archive');
CREATE INDEX memory_revisions_timeline ON memory_revisions(memory_id,created_at);

DROP TABLE memory_revisions_v11;
DROP TABLE memories_v11;

-- Renaming `memories` also rewrote the foreign key in `memory_embeddings` to point at the
-- temporary name, which is about to stop existing. Rebuild the table with the original
-- definition so the cascade keeps working; rows and counters are copied verbatim.
DROP INDEX memory_embeddings_model;
ALTER TABLE memory_embeddings RENAME TO memory_embeddings_v11;
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
INSERT INTO memory_embeddings SELECT * FROM memory_embeddings_v11;
DROP TABLE memory_embeddings_v11;
CREATE INDEX memory_embeddings_model ON memory_embeddings(model,dimensions);

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

-- Recreated byte-for-byte from migration 006, now bound to the rebuilt `memories`.
CREATE TRIGGER provenance_edges_validate BEFORE INSERT ON provenance_edges BEGIN
 SELECT CASE NEW.source_kind
  WHEN 'evidence' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM sources s JOIN chat_receipts r ON r.request_id=NEW.request_id
   WHERE s.id=NEW.source_id AND s.scope=r.scope) THEN RAISE(ABORT,'unknown provenance source') END
  WHEN 'step' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM turn_steps WHERE id=NEW.source_id AND request_id=NEW.request_id) THEN RAISE(ABORT,'unknown provenance source') END
  WHEN 'permission' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM permission_requests WHERE id=NEW.source_id AND request_id=NEW.request_id) THEN RAISE(ABORT,'unknown provenance source') END
  WHEN 'mutation' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM file_changes WHERE id=NEW.source_id AND request_id=NEW.request_id) THEN RAISE(ABORT,'unknown provenance source') END
  WHEN 'memory' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM memories m JOIN chat_receipts r ON r.request_id=NEW.request_id
   WHERE m.id=NEW.source_id AND m.scope=r.scope) THEN RAISE(ABORT,'unknown provenance source') END
  WHEN 'recovery' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM activity_events WHERE CAST(seq AS TEXT)=NEW.source_id
   AND request_id=NEW.request_id AND kind='interrupted') THEN RAISE(ABORT,'unknown provenance source') END
 END;
 SELECT CASE NEW.target_kind
  WHEN 'evidence' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM sources s JOIN chat_receipts r ON r.request_id=NEW.request_id
   WHERE s.id=NEW.target_id AND s.scope=r.scope) THEN RAISE(ABORT,'unknown provenance target') END
  WHEN 'step' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM turn_steps WHERE id=NEW.target_id AND request_id=NEW.request_id) THEN RAISE(ABORT,'unknown provenance target') END
  WHEN 'permission' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM permission_requests WHERE id=NEW.target_id AND request_id=NEW.request_id) THEN RAISE(ABORT,'unknown provenance target') END
  WHEN 'mutation' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM file_changes WHERE id=NEW.target_id AND request_id=NEW.request_id) THEN RAISE(ABORT,'unknown provenance target') END
  WHEN 'memory' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM memories m JOIN chat_receipts r ON r.request_id=NEW.request_id
   WHERE m.id=NEW.target_id AND m.scope=r.scope) THEN RAISE(ABORT,'unknown provenance target') END
  WHEN 'recovery' THEN CASE WHEN NOT EXISTS(
   SELECT 1 FROM activity_events WHERE CAST(seq AS TEXT)=NEW.target_id
   AND request_id=NEW.request_id AND kind='interrupted') THEN RAISE(ABORT,'unknown provenance target') END
 END;
 SELECT CASE WHEN (SELECT count(*) FROM provenance_edges WHERE request_id=NEW.request_id) >= 2000
  THEN RAISE(ABORT,'provenance edge limit reached') END;
END;
CREATE TRIGGER provenance_memory_delete BEFORE DELETE ON memories
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='memory' AND source_id=OLD.id) OR (target_kind='memory' AND target_id=OLD.id))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;

-- One branch is checked out per scope. Absence of a row means 'main', so existing scopes keep
-- behaving exactly as before this migration.
CREATE TABLE memory_branches (
 scope TEXT NOT NULL,
 branch TEXT NOT NULL CHECK(length(branch) BETWEEN 1 AND 60),
 active INTEGER NOT NULL DEFAULT 0 CHECK(active IN (0,1)),
 note TEXT,
 created_at TEXT NOT NULL,
 PRIMARY KEY(scope,branch)
);
CREATE UNIQUE INDEX memory_branches_active ON memory_branches(scope) WHERE active=1;

-- The decision timeline. Every considered value is kept with the reason it reached its state,
-- so 'chosen' can always be read beside what it beat rather than replacing it.
CREATE TABLE memory_decisions (
 id TEXT PRIMARY KEY,
 group_id TEXT NOT NULL,
 scope TEXT NOT NULL, key TEXT NOT NULL,
 branch TEXT NOT NULL DEFAULT 'main',
 memory_id TEXT REFERENCES memories(id) ON DELETE SET NULL,
 candidate_id TEXT REFERENCES candidates(id),
 value TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('considered','chosen','superseded')),
 reason TEXT NOT NULL CHECK(reason IN ('proposed','approved','replaced_by_newer','duplicate_merged','expired','rejected','conflict')),
 revision INTEGER NOT NULL CHECK(revision>=0),
 decided_at TEXT NOT NULL
);
CREATE INDEX memory_decisions_group ON memory_decisions(group_id,decided_at);
CREATE INDEX memory_decisions_key ON memory_decisions(scope,key,decided_at);

-- Usefulness feedback, attributed to the exact revision it judged. Stored per verdict rather
-- than as a mutable counter so a later edit cannot inherit praise for older wording.
CREATE TABLE memory_feedback (
 id TEXT PRIMARY KEY,
 memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL CHECK(revision>0),
 request_id TEXT,
 verdict TEXT NOT NULL CHECK(verdict IN ('useful','not_useful')),
 note TEXT,
 created_at TEXT NOT NULL,
 UNIQUE(memory_id,revision,request_id,verdict)
);
CREATE INDEX memory_feedback_memory ON memory_feedback(memory_id,created_at);

-- Recall may now keep a pinned profile entry that the rank cutoff would otherwise drop. That
-- needs its own reason, so `retrieval_candidates` is rebuilt with one extra allowed value. Rows
-- are copied verbatim: past receipts keep the exact reason they were written with.
ALTER TABLE retrieval_candidates RENAME TO retrieval_candidates_v11;
DROP INDEX retrieval_candidates_decision;
CREATE TABLE retrieval_candidates (
 request_id TEXT NOT NULL REFERENCES retrieval_receipts(request_id) ON DELETE CASCADE,
 memory_id TEXT NOT NULL,
 scope TEXT NOT NULL,
 key TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 0),
 rank INTEGER NOT NULL CHECK(rank >= 0),
 decision TEXT NOT NULL CHECK(decision IN ('included','excluded')),
 reason TEXT NOT NULL CHECK(reason IN ('ranked_and_fit','rank_cutoff','payload_ceiling','category_budget','pinned_profile')),
 lexical_score REAL NOT NULL DEFAULT 0,
 semantic_score REAL NOT NULL DEFAULT 0,
 scope_score REAL NOT NULL DEFAULT 0,
 recency_score REAL NOT NULL DEFAULT 0,
 usefulness_score REAL NOT NULL DEFAULT 0,
 total_score REAL NOT NULL,
 bytes INTEGER NOT NULL CHECK(bytes >= 0),
 PRIMARY KEY(request_id, memory_id)
);
INSERT INTO retrieval_candidates SELECT * FROM retrieval_candidates_v11;
DROP TABLE retrieval_candidates_v11;
CREATE INDEX retrieval_candidates_decision ON retrieval_candidates(request_id, decision, rank);

PRAGMA user_version=12;
COMMIT;
PRAGMA foreign_keys=ON;
