-- 019: admit the once-only tool effects fenced by P18-T04.
--
-- Migration 018 intentionally enumerated the external effects that were then wired. P18-T04 adds
-- process execution and remote browser input; SQLite cannot widen a CHECK in place, so rebuild the
-- table while preserving every effect identity, outcome, timestamp, index and immutability guard.
BEGIN IMMEDIATE;

CREATE TABLE external_effects_v19 (
  effect_id TEXT PRIMARY KEY,
  request_id TEXT NOT NULL REFERENCES chat_receipts(request_id) ON DELETE RESTRICT,
  step_identity TEXT NOT NULL CHECK(length(step_identity) BETWEEN 1 AND 256),
  payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 64),
  idempotency_key TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK(kind IN (
    'provider_call','webhook','file_write','notification','bash_command','browser_input'
  )),
  fence INTEGER NOT NULL CHECK(fence > 0),
  state TEXT NOT NULL CHECK(state IN ('reserved','succeeded','failed','unknown')),
  outcome_ref TEXT,
  reason TEXT,
  attempted_at TEXT NOT NULL,
  settled_at TEXT,
  CHECK((state = 'reserved') = (settled_at IS NULL)),
  CHECK(state <> 'unknown' OR reason IS NOT NULL),
  CHECK(settled_at IS NULL OR settled_at >= attempted_at)
);

INSERT INTO external_effects_v19(
  effect_id,request_id,step_identity,payload_digest,idempotency_key,kind,fence,state,
  outcome_ref,reason,attempted_at,settled_at
)
SELECT
  effect_id,request_id,step_identity,payload_digest,idempotency_key,kind,fence,state,
  outcome_ref,reason,attempted_at,settled_at
FROM external_effects;

DROP TABLE external_effects;
ALTER TABLE external_effects_v19 RENAME TO external_effects;

CREATE INDEX external_effects_open ON external_effects(state, request_id);
CREATE INDEX external_effects_request ON external_effects(request_id, attempted_at);

CREATE TRIGGER external_effects_settle_once BEFORE UPDATE ON external_effects
WHEN OLD.state <> 'reserved' AND NEW.state <> OLD.state BEGIN
  SELECT RAISE(ABORT,'an external effect outcome is recorded once');
END;

CREATE TRIGGER external_effects_key_immutable BEFORE UPDATE ON external_effects
WHEN NEW.idempotency_key <> OLD.idempotency_key
  OR NEW.request_id <> OLD.request_id
  OR NEW.step_identity <> OLD.step_identity
  OR NEW.payload_digest <> OLD.payload_digest BEGIN
  SELECT RAISE(ABORT,'an external effect identity is immutable');
END;

CREATE TRIGGER external_effects_no_delete BEFORE DELETE ON external_effects BEGIN
  SELECT RAISE(ABORT,'external effects are append-only; settle the row instead of deleting');
END;

PRAGMA user_version=19;
COMMIT;
