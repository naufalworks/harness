-- 020: make archive deletion intent and outcome durable.
--
-- delete_archive removes ciphertext bytes, so the database must record that the attempt started
-- before the filesystem mutation and then record the observed outcome afterward. SQLite cannot widen
-- the privacy_events action CHECK in place, so rebuild it while preserving append-only triggers.
BEGIN IMMEDIATE;

CREATE TABLE privacy_events_v20 (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 source_id TEXT NOT NULL,
 archive_id TEXT REFERENCES exact_archives(id),
 action TEXT NOT NULL CHECK(action IN (
  'forget','delete_source','purge_index','delete_archive',
  'delete_archive_intent','delete_archive_succeeded','delete_archive_missing'
 )),
 created_at TEXT NOT NULL,
 CHECK((action IN ('delete_archive','delete_archive_intent','delete_archive_succeeded','delete_archive_missing') AND archive_id IS NOT NULL)
   OR (action NOT IN ('delete_archive','delete_archive_intent','delete_archive_succeeded','delete_archive_missing') AND archive_id IS NULL))
);

INSERT INTO privacy_events_v20(seq,id,source_id,archive_id,action,created_at)
SELECT seq,id,source_id,archive_id,action,created_at FROM privacy_events;

DROP TABLE privacy_events;
ALTER TABLE privacy_events_v20 RENAME TO privacy_events;

CREATE INDEX privacy_events_source ON privacy_events(source_id,seq);
CREATE TRIGGER privacy_events_no_update BEFORE UPDATE ON privacy_events BEGIN SELECT RAISE(ABORT,'privacy events are append-only'); END;
CREATE TRIGGER privacy_events_no_delete BEFORE DELETE ON privacy_events BEGIN SELECT RAISE(ABORT,'privacy events are append-only'); END;

PRAGMA user_version=20;
COMMIT;
