-- 006: bounded typed provenance edges over existing durable rows.
-- The graph stores inspectable row references only: never model chain-of-thought.
BEGIN IMMEDIATE;

CREATE TABLE provenance_edges (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES chat_receipts(request_id),
 source_kind TEXT NOT NULL CHECK(source_kind IN ('evidence','step','permission','mutation','memory','recovery')),
 source_id TEXT NOT NULL CHECK(length(source_id) BETWEEN 1 AND 128),
 relation TEXT NOT NULL CHECK(relation IN ('supports','contradicts','depends_on','authorizes','mutates','invalidates','triggers')),
 target_kind TEXT NOT NULL CHECK(target_kind IN ('evidence','step','permission','mutation','memory','recovery')),
 target_id TEXT NOT NULL CHECK(length(target_id) BETWEEN 1 AND 128),
 created_at TEXT NOT NULL,
 CHECK(source_kind != target_kind OR source_id != target_id),
 UNIQUE(request_id,source_kind,source_id,relation,target_kind,target_id)
);
CREATE INDEX provenance_edges_request ON provenance_edges(request_id,created_at,id);
CREATE INDEX provenance_edges_source ON provenance_edges(source_kind,source_id);
CREATE INDEX provenance_edges_target ON provenance_edges(target_kind,target_id);

-- SQLite cannot express a polymorphic foreign key. Resolve both endpoints in a trigger and bind
-- request-owned rows to this turn. Evidence and memories may span turns, but never scopes.
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

-- Match foreign-key RESTRICT semantics for polymorphic endpoints. Durable evidence cannot vanish
-- underneath an incident graph; callers must remove the edge first.
CREATE TRIGGER provenance_evidence_delete BEFORE DELETE ON sources
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='evidence' AND source_id=OLD.id) OR (target_kind='evidence' AND target_id=OLD.id))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;
CREATE TRIGGER provenance_step_delete BEFORE DELETE ON turn_steps
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='step' AND source_id=OLD.id) OR (target_kind='step' AND target_id=OLD.id))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;
CREATE TRIGGER provenance_permission_delete BEFORE DELETE ON permission_requests
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='permission' AND source_id=OLD.id) OR (target_kind='permission' AND target_id=OLD.id))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;
CREATE TRIGGER provenance_mutation_delete BEFORE DELETE ON file_changes
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='mutation' AND source_id=OLD.id) OR (target_kind='mutation' AND target_id=OLD.id))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;
CREATE TRIGGER provenance_memory_delete BEFORE DELETE ON memories
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='memory' AND source_id=OLD.id) OR (target_kind='memory' AND target_id=OLD.id))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;
CREATE TRIGGER provenance_recovery_delete BEFORE DELETE ON activity_events
 WHEN EXISTS(SELECT 1 FROM provenance_edges WHERE (source_kind='recovery' AND source_id=CAST(OLD.seq AS TEXT)) OR (target_kind='recovery' AND target_id=CAST(OLD.seq AS TEXT)))
 BEGIN SELECT RAISE(ABORT,'provenance endpoint is still referenced'); END;

PRAGMA user_version=6;
COMMIT;
