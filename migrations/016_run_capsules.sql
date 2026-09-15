-- 016: immutable, content-addressed Memory Wind Tunnel run capsules (P17-T01).
BEGIN IMMEDIATE;

CREATE TABLE run_capsules (
 id TEXT PRIMARY KEY CHECK(length(id)=64 AND id NOT GLOB '*[^0-9a-f]*'),
 format TEXT NOT NULL CHECK(format='harness-run-capsule-v1'),
 validator TEXT NOT NULL CHECK(validator='harness-capsule-validator-v1'),
 source_request_id TEXT NOT NULL REFERENCES chat_receipts(request_id) ON DELETE RESTRICT,
 replay_mode TEXT NOT NULL CHECK(replay_mode IN ('strict','live','hybrid')),
 manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json) AND length(manifest_json)<=1048576),
 manifest_sha256 TEXT NOT NULL UNIQUE
    CHECK(length(manifest_sha256)=64 AND manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
 created_at TEXT NOT NULL,
 CHECK(id=manifest_sha256)
);
CREATE INDEX run_capsules_source ON run_capsules(source_request_id,created_at);
CREATE INDEX run_capsules_mode ON run_capsules(replay_mode,created_at);

-- A treatment or replay may reference a capsule, but never rewrite its experiment boundary.
CREATE TRIGGER run_capsules_immutable_update BEFORE UPDATE ON run_capsules BEGIN
 SELECT RAISE(ABORT,'run capsules are immutable');
END;
CREATE TRIGGER run_capsules_immutable_delete BEFORE DELETE ON run_capsules BEGIN
 SELECT RAISE(ABORT,'run capsules are immutable');
END;

PRAGMA user_version=16;
COMMIT;
