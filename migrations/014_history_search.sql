-- 014: scoped sanitized history search, an explicit forget/source-delete boundary, and the
-- durable export/import ledger (P15-T04).
--
-- Additive only. No existing row is deleted, no historical revision is rewritten.
--
-- Why a projection table instead of an FTS5 external-content index straight over `messages`
-- and `sources`: only *sanitized* content may be indexed or returned, and sanitization is a
-- Rust function (`safety::sensitive` / `safety::redact`), not something an SQLite trigger can
-- call. A trigger-only index would therefore have to index the raw column and hope the writer
-- had already cleaned it. `history_documents` is instead an explicitly maintained projection:
-- the indexer writes a row only after the shared sanitizer accepted the text, and records which
-- sanitizer version accepted it plus the checksum of exactly what was indexed. `history_fts`
-- is then an ordinary external-content index over that projection, kept in sync by the same
-- three-trigger pattern `memory_fts` uses in 001_core.sql / 012_memory_governance.sql.
--
-- Why `forget` and `delete_source` are different columns rather than one status: migration 007
-- already established that vocabulary for sources, and the same distinction is needed for
-- conversation turns. `forgotten_at` stops a document being recalled or returned while the row,
-- its citation and its revision trail stay readable. `source_deleted_at` means the underlying
-- content is gone: the body is emptied here and the source row itself is removed, while this
-- row and its append-only audit remain so the deletion is provable rather than silent.
BEGIN IMMEDIATE;

-- One searchable, already-sanitized document. `kind` says which source table `source_id`
-- points at; the pair is a citation an operator can trace back to the exact row.
CREATE TABLE history_documents (
 rowid INTEGER PRIMARY KEY,
 id TEXT NOT NULL UNIQUE,
 kind TEXT NOT NULL CHECK(kind IN ('turn','artifact')),
 source_id TEXT NOT NULL,
 scope TEXT NOT NULL,
 -- Turns carry their conversation; artifacts (sources) are scope-level, so this is NULL for
 -- them. Search scoping accepts scope and/or session, which is as narrow as the schema allows.
 session_id TEXT,
 -- Monotonic per document. A re-index of changed content advances it, so a citation names a
 -- specific version of the text rather than "whatever the row says now".
 revision INTEGER NOT NULL CHECK(revision>0),
 role TEXT CHECK(role IS NULL OR role IN ('user','assistant')),
 title TEXT NOT NULL,
 body TEXT NOT NULL,
 -- Which sanitizer accepted this text. Recorded per row so a future sanitizer change cannot
 -- retroactively claim older rows were cleaned by it.
 sanitizer TEXT NOT NULL CHECK(length(sanitizer) BETWEEN 1 AND 60),
 content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
 source_created_at TEXT NOT NULL,
 indexed_at TEXT NOT NULL,
 -- Recall/search suppression. The row, its citation and its audit survive.
 forgotten_at TEXT,
 -- The underlying source content was removed. `body` is emptied in the same transaction.
 source_deleted_at TEXT,
 UNIQUE(kind,source_id),
 CHECK(source_deleted_at IS NULL OR body='')
);
CREATE INDEX history_documents_scope ON history_documents(scope,kind,source_created_at);
CREATE INDEX history_documents_session ON history_documents(session_id,source_created_at);
CREATE INDEX history_documents_live ON history_documents(forgotten_at,source_deleted_at);

CREATE VIRTUAL TABLE history_fts USING fts5(title,body,content='history_documents',content_rowid='rowid',tokenize='unicode61');
CREATE TRIGGER history_documents_ai AFTER INSERT ON history_documents BEGIN
 INSERT INTO history_fts(rowid,title,body) VALUES(new.rowid,new.title,new.body);
END;
CREATE TRIGGER history_documents_ad AFTER DELETE ON history_documents BEGIN
 INSERT INTO history_fts(history_fts,rowid,title,body) VALUES('delete',old.rowid,old.title,old.body);
END;
CREATE TRIGGER history_documents_au AFTER UPDATE ON history_documents BEGIN
 INSERT INTO history_fts(history_fts,rowid,title,body) VALUES('delete',old.rowid,old.title,old.body);
 INSERT INTO history_fts(rowid,title,body) VALUES(new.rowid,new.title,new.body);
END;

-- Append-only audit for the two operations. `restore` exists because forget is reversible by
-- design (the evidence was never destroyed) while a delete_source is not, and the difference
-- has to be legible in the trail rather than inferred from a missing row.
CREATE TABLE history_privacy_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 document_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('turn','artifact')),
 source_id TEXT NOT NULL,
 action TEXT NOT NULL CHECK(action IN ('index','forget','restore','delete_source')),
 revision INTEGER NOT NULL CHECK(revision>0),
 detail TEXT,
 created_at TEXT NOT NULL
);
CREATE INDEX history_privacy_events_document ON history_privacy_events(document_id,seq);
CREATE TRIGGER history_privacy_events_no_update BEFORE UPDATE ON history_privacy_events BEGIN SELECT RAISE(ABORT,'history privacy events are append-only'); END;
CREATE TRIGGER history_privacy_events_no_delete BEFORE DELETE ON history_privacy_events BEGIN SELECT RAISE(ABORT,'history privacy events are append-only'); END;

-- A retention sweep or an operator DELETE on the source row must not leave a document claiming
-- to quote content that no longer exists. These make the source-delete state reachable from the
-- source side too, with exactly the same observable outcome: the body goes, the citation stays.
CREATE TRIGGER history_source_message_deleted AFTER DELETE ON messages BEGIN
 UPDATE history_documents SET body='',source_deleted_at=COALESCE(source_deleted_at,strftime('%Y-%m-%dT%H:%M:%SZ','now'))
  WHERE kind='turn' AND source_id=old.id;
END;
CREATE TRIGGER history_source_artifact_deleted AFTER DELETE ON sources BEGIN
 UPDATE history_documents SET body='',source_deleted_at=COALESCE(source_deleted_at,strftime('%Y-%m-%dT%H:%M:%SZ','now'))
  WHERE kind='artifact' AND source_id=old.id;
END;

-- One reviewable bundle of things that may leave this machine. `draft` is assembled,
-- `reviewed` means an operator saw the exact contents (pinned by `content_sha256`), and only a
-- `released` bundle can be serialized out. A bundle whose contents changed after review fails
-- the checksum at release rather than leaving silently.
CREATE TABLE export_bundles (
 id TEXT PRIMARY KEY,
 kind TEXT NOT NULL CHECK(kind IN ('memory_selection','continuation_packet')),
 scope TEXT NOT NULL,
 -- Who the export is for. Recorded because "reviewed" is only meaningful against an audience.
 audience TEXT NOT NULL CHECK(audience IN ('self','team','public')),
 state TEXT NOT NULL CHECK(state IN ('draft','reviewed','released','rejected')),
 format_version INTEGER NOT NULL CHECK(format_version=1),
 item_count INTEGER NOT NULL CHECK(item_count>=0),
 content_sha256 TEXT CHECK(content_sha256 IS NULL OR length(content_sha256)=64),
 reviewed_at TEXT,
 released_at TEXT,
 note TEXT,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL,
 CHECK((state IN ('reviewed','released') AND content_sha256 IS NOT NULL AND reviewed_at IS NOT NULL) OR state IN ('draft','rejected')),
 CHECK((state='released' AND released_at IS NOT NULL) OR (state<>'released' AND released_at IS NULL))
);
CREATE INDEX export_bundles_state ON export_bundles(state,created_at);

-- One exported thing, addressed by the identifier it already has in this database plus the
-- revision it had when it was selected. Stable id + revision is what makes a round trip
-- idempotent: an import can tell "already have this exact version" from "this is newer".
CREATE TABLE export_items (
 bundle_id TEXT NOT NULL REFERENCES export_bundles(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('memory','history')),
 stable_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 payload_json TEXT NOT NULL,
 content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
 -- 0 means the shared sanitizer rejected the content. Such a row is kept so the reviewer can
 -- see it was considered, and release refuses while any of them is present.
 sanitized INTEGER NOT NULL CHECK(sanitized IN (0,1)),
 reviewed INTEGER NOT NULL DEFAULT 0 CHECK(reviewed IN (0,1)),
 created_at TEXT NOT NULL,
 PRIMARY KEY(bundle_id,kind,stable_id)
);
CREATE INDEX export_items_review ON export_items(bundle_id,reviewed,sanitized);

-- What an import decided, per item. Written even when nothing changed, so "idempotent" is an
-- observation in this table rather than a claim in a comment.
CREATE TABLE import_receipts (
 id TEXT PRIMARY KEY,
 bundle_id TEXT NOT NULL,
 origin TEXT NOT NULL,
 scope TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('memory_selection','continuation_packet')),
 content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
 accepted INTEGER NOT NULL CHECK(accepted>=0),
 unchanged INTEGER NOT NULL CHECK(unchanged>=0),
 skipped INTEGER NOT NULL CHECK(skipped>=0),
 created_at TEXT NOT NULL
);
CREATE TABLE import_decisions (
 receipt_id TEXT NOT NULL REFERENCES import_receipts(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('memory','history')),
 stable_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 outcome TEXT NOT NULL CHECK(outcome IN ('created','unchanged','revision_advanced','skipped_stale','skipped_unsanitized','skipped_unreviewed')),
 detail TEXT,
 PRIMARY KEY(receipt_id,kind,stable_id)
);
CREATE INDEX import_decisions_outcome ON import_decisions(outcome);

PRAGMA user_version=14;
COMMIT;
