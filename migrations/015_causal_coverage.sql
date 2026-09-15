-- 015: causal coverage measurement and first-class deployment provenance (P16-T03).
--
-- Additive only. No existing row is deleted and no historical row is rewritten.
--
-- Two things become durable here, and one deliberately does not.
--
-- What does NOT become durable: the coverage metrics themselves (missing-edge, earliest-break
-- and graph-size). Those are pure functions of rows that already exist — `provenance_edges`
-- plus the bounded `incident_view` projection — so storing them would be a cache with no
-- invalidation story, which is exactly the reasoning P16-T01 recorded when it declined to
-- persist its projection. They are computed on read, from the one projection, every time.
--
-- What DOES become durable, because neither is recomputable from anything else:
--
-- 1. `deployment_events`. A deployment is an event that happens *to* this machine: the build
--    that produced a binary, the restart that promoted it, and the smoke tests that accepted
--    or rejected it. Once the process is gone nothing in the database can reconstruct which
--    commit was promoted at which second by which binary hash, so it is recorded as it
--    happens. `phase` is the vocabulary of a deployment's causal trail (build -> restart ->
--    smoke -> outcome), and `parent_id` is the recorded dependency between phases of one
--    deployment — the same discipline `provenance_edges` uses for a coding turn: a real
--    recorded reference, never an inference from adjacent timestamps.
--
-- 2. `incident_reviews`. Reviewer time cannot be derived from a graph; it is an observation
--    about a human. A row is written when a reviewer opens an incident and closed when they
--    stop, so "reviewer time" is measured rather than estimated. `outcome` is nullable
--    because a review that was abandoned is a real, reportable outcome and must not be
--    back-filled with a guess.
--
-- Anomaly flags are columns on `deployment_events` rather than a separate table, because an
-- anomaly is always an anomaly *about* a recorded event. A flag with no event to point at
-- would be exactly the kind of free-floating claim this schema refuses to hold.
BEGIN IMMEDIATE;

-- One phase of one deployment. `deployment_id` groups the phases; `parent_id` records which
-- earlier phase this one actually followed.
CREATE TABLE deployment_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 -- Groups every phase of one deployment attempt. Not a foreign key: the first phase of a
 -- deployment is what creates the group, so there is no parent row to point at yet.
 deployment_id TEXT NOT NULL,
 -- The recorded dependency between phases. NULL only for the first phase of a deployment.
 -- RESTRICT rather than CASCADE: a deployment trail is evidence, and deleting a middle phase
 -- would leave a chain that silently claims a different sequence of events.
 parent_id TEXT REFERENCES deployment_events(id) ON DELETE RESTRICT,
 phase TEXT NOT NULL CHECK(phase IN ('build','restart','smoke','outcome','rollback')),
 -- `unknown` is a first-class status, not a placeholder: a phase whose result was never
 -- observed must not be recorded as either success or failure.
 status TEXT NOT NULL CHECK(status IN ('started','succeeded','failed','unknown')),
 -- Build identity. Nullable because a restart phase does not itself produce a binary, and
 -- copying the build's values onto it would assert an identity that phase never established.
 commit_sha TEXT CHECK(commit_sha IS NULL OR length(commit_sha) BETWEEN 7 AND 64),
 binary_sha256 TEXT CHECK(binary_sha256 IS NULL OR length(binary_sha256)=64),
 schema_version INTEGER CHECK(schema_version IS NULL OR schema_version>=0),
 -- What was checked and what came back. Bounded so a deployment record cannot grow without
 -- limit, and redacted by the writer before it ever arrives here.
 detail TEXT CHECK(detail IS NULL OR length(detail)<=2000),
 -- Anomaly flags. Each is a recorded observation about THIS event, and each is 0/1/NULL where
 -- NULL means the question was never asked — distinct from 0, which means it was asked and
 -- the answer was no.
 anomaly_identity_mismatch INTEGER CHECK(anomaly_identity_mismatch IS NULL OR anomaly_identity_mismatch IN (0,1)),
 anomaly_unready INTEGER CHECK(anomaly_unready IS NULL OR anomaly_unready IN (0,1)),
 anomaly_smoke_failed INTEGER CHECK(anomaly_smoke_failed IS NULL OR anomaly_smoke_failed IN (0,1)),
 anomaly_schema_regressed INTEGER CHECK(anomaly_schema_regressed IS NULL OR anomaly_schema_regressed IN (0,1)),
 started_at TEXT NOT NULL,
 finished_at TEXT,
 -- A finished phase cannot still be `started`, and an unfinished one cannot claim an outcome.
 CHECK((finished_at IS NULL AND status='started') OR (finished_at IS NOT NULL AND status<>'started')),
 -- A phase cannot be its own parent.
 CHECK(parent_id IS NULL OR parent_id<>id)
);
CREATE INDEX deployment_events_deployment ON deployment_events(deployment_id,seq);
CREATE INDEX deployment_events_phase ON deployment_events(phase,started_at);
CREATE INDEX deployment_events_parent ON deployment_events(parent_id);
-- Deployment provenance is evidence: it may be appended to and completed, but a recorded
-- phase's identity and history must not be rewritten afterwards. Only the fields that are
-- unknown at insert time (the result of the phase) may ever change.
CREATE TRIGGER deployment_events_append_only BEFORE UPDATE ON deployment_events BEGIN
 SELECT CASE WHEN old.id<>new.id OR old.deployment_id<>new.deployment_id OR old.phase<>new.phase
   OR old.started_at<>new.started_at
   OR COALESCE(old.parent_id,'')<>COALESCE(new.parent_id,'')
   OR (old.finished_at IS NOT NULL AND COALESCE(old.finished_at,'')<>COALESCE(new.finished_at,''))
   OR (old.status<>'started' AND old.status<>new.status)
  THEN RAISE(ABORT,'recorded deployment provenance is append-only')
 END;
END;
CREATE TRIGGER deployment_events_no_delete BEFORE DELETE ON deployment_events BEGIN
 SELECT RAISE(ABORT,'recorded deployment provenance cannot be deleted');
END;

-- One reviewer's pass over one incident. Written on open, completed on close.
CREATE TABLE incident_reviews (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL,
 -- What the reviewer was actually looking at, so a measured duration is attributable to a
 -- specific view rather than to "an incident" in the abstract.
 view TEXT NOT NULL CHECK(view IN ('causal','chronological','comparison','export')),
 -- Graph size as served to this reviewer, recorded because a duration is only interpretable
 -- next to how much graph the reviewer was shown.
 node_count INTEGER NOT NULL CHECK(node_count>=0),
 edge_count INTEGER NOT NULL CHECK(edge_count>=0),
 -- NULL until the review is closed. A NULL here means "still open or abandoned", which is a
 -- reportable state; it is never filled in with an assumed duration.
 duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms>=0),
 -- What the reviewer concluded, when they concluded anything. `unknown` is available on
 -- purpose so a reviewer who found no answer can say so instead of picking a cause.
 outcome TEXT CHECK(outcome IS NULL OR outcome IN ('cause_identified','unknown','abandoned')),
 opened_at TEXT NOT NULL,
 closed_at TEXT,
 CHECK((closed_at IS NULL AND duration_ms IS NULL) OR (closed_at IS NOT NULL AND duration_ms IS NOT NULL))
);
CREATE INDEX incident_reviews_request ON incident_reviews(request_id,opened_at);
CREATE INDEX incident_reviews_open ON incident_reviews(closed_at,opened_at);

PRAGMA user_version=15;
COMMIT;
