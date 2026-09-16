-- 018: durable record of non-replayable external effects (P18-T03).
--
-- Decision 4 in docs/design/leased-multi-worker.md says an external effect whose
-- outcome is unknown is surfaced for a human decision and never optimistically
-- retried. Until this migration that was a stated intent with nothing to enforce
-- it: there was no place to record that an effect had been attempted, so "do not
-- retry" depended on worker code remembering, and a worker that crashed mid-call
-- remembered nothing.
--
-- The key property is that the idempotency key is *fence-independent*. A steal
-- raises the fence, so if the fence were part of the key the new holder would
-- compute a different key for the same logical effect and the uniqueness
-- constraint would not stop it from issuing the call a second time. The fence is
-- still recorded, because knowing which holder attempted an effect is needed to
-- explain it later -- but it authorizes the write, it does not identify the effect.
BEGIN IMMEDIATE;

CREATE TABLE external_effects (
  effect_id TEXT PRIMARY KEY,
  request_id TEXT NOT NULL REFERENCES chat_receipts(request_id) ON DELETE RESTRICT,
  -- Identity of the step that wants the effect, stable across a retake of the turn.
  step_identity TEXT NOT NULL CHECK(length(step_identity) BETWEEN 1 AND 256),
  -- SHA-256 of the fence-independent request payload.
  payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 64),
  -- (request_id, step_identity, payload_digest), as specified by the design. UNIQUE
  -- is what actually prevents a duplicate paid call: a second attempt at the same
  -- logical effect collides here instead of reaching the provider.
  idempotency_key TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK(kind IN ('provider_call','webhook','file_write','notification')),
  -- The lease fence held when the effect was reserved. Recorded, never part of the key.
  fence INTEGER NOT NULL CHECK(fence > 0),
  state TEXT NOT NULL CHECK(state IN ('reserved','succeeded','failed','unknown')),
  -- Provider-side identifier, when the provider returned one.
  outcome_ref TEXT,
  reason TEXT,
  attempted_at TEXT NOT NULL,
  settled_at TEXT,
  -- An effect is either still in flight or settled at a recorded time. Without this
  -- a sweep could mark an outcome terminal while leaving it looking in-flight.
  CHECK((state = 'reserved') = (settled_at IS NULL)),
  -- An unknown outcome is the one state a human has to act on, so it must say why.
  CHECK(state <> 'unknown' OR reason IS NOT NULL),
  CHECK(settled_at IS NULL OR settled_at >= attempted_at)
);

-- The restart sweep and the surfacing queue both look for effects that are still
-- reserved; neither should scan settled history to find them.
CREATE INDEX external_effects_open ON external_effects(state, request_id);
CREATE INDEX external_effects_request ON external_effects(request_id, attempted_at);

-- Reserve-then-settle only. An effect that already has an outcome cannot be given
-- a different one, so "it failed, try again" cannot be rewritten into "it never
-- happened" by a later worker. This one guard also covers re-reservation: a settled
-- row moving back to 'reserved' is a state change from a non-reserved state, so a
-- separate no-re-reserve trigger could never be the constraint that fires. Mutation
-- testing caught exactly that: dropping it left the write still refused, which means
-- it was decoration rather than protection, so it is not here.
CREATE TRIGGER external_effects_settle_once BEFORE UPDATE ON external_effects
WHEN OLD.state <> 'reserved' AND NEW.state <> OLD.state BEGIN
  SELECT RAISE(ABORT,'an external effect outcome is recorded once');
END;

-- The key is the deduplication identity. If it could be edited, a duplicate call
-- could be made to look like a distinct effect.
CREATE TRIGGER external_effects_key_immutable BEFORE UPDATE ON external_effects
WHEN NEW.idempotency_key <> OLD.idempotency_key
  OR NEW.request_id <> OLD.request_id
  OR NEW.step_identity <> OLD.step_identity
  OR NEW.payload_digest <> OLD.payload_digest BEGIN
  SELECT RAISE(ABORT,'an external effect identity is immutable');
END;

-- History of attempted external effects is evidence about money spent and messages
-- delivered. Deleting a row would make a duplicate undetectable.
CREATE TRIGGER external_effects_no_delete BEFORE DELETE ON external_effects BEGIN
  SELECT RAISE(ABORT,'external effects are append-only; settle the row instead of deleting');
END;

PRAGMA user_version=18;
COMMIT;
