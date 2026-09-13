BEGIN IMMEDIATE;
CREATE TABLE exact_archives (
 id TEXT PRIMARY KEY,
 source_id TEXT NOT NULL,
 relative_path TEXT NOT NULL UNIQUE,
 format_version INTEGER NOT NULL CHECK(format_version=1),
 algorithm TEXT NOT NULL CHECK(algorithm='AES-256-GCM'),
 key_id TEXT NOT NULL CHECK(length(key_id)=16),
 plaintext_sha256 TEXT NOT NULL CHECK(length(plaintext_sha256)=64),
 byte_length INTEGER NOT NULL CHECK(byte_length>=0),
 created_at TEXT NOT NULL,
 deleted_at TEXT
);
CREATE INDEX exact_archives_source ON exact_archives(source_id,created_at);
CREATE TABLE source_privacy_state (
 source_id TEXT PRIMARY KEY,
 forgotten_at TEXT,
 source_deleted_at TEXT,
 index_purged_at TEXT,
 updated_at TEXT NOT NULL
);
CREATE TABLE privacy_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 source_id TEXT NOT NULL,
 archive_id TEXT REFERENCES exact_archives(id),
 action TEXT NOT NULL CHECK(action IN ('forget','delete_source','purge_index','delete_archive')),
 created_at TEXT NOT NULL,
 CHECK((action='delete_archive' AND archive_id IS NOT NULL) OR (action!='delete_archive' AND archive_id IS NULL))
);
CREATE INDEX privacy_events_source ON privacy_events(source_id,seq);
CREATE TRIGGER privacy_events_no_update BEFORE UPDATE ON privacy_events BEGIN SELECT RAISE(ABORT,'privacy events are append-only'); END;
CREATE TRIGGER privacy_events_no_delete BEFORE DELETE ON privacy_events BEGIN SELECT RAISE(ABORT,'privacy events are append-only'); END;
PRAGMA user_version=7;
COMMIT;
