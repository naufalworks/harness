-- 017: fenced worker leases for leased multi-worker execution (P18-T01).
--
-- This migration lands the ownership machinery with the worker count still
-- pinned at one. Nothing here enables a second worker; rollout gate 3 in
-- docs/design/leased-multi-worker.md keeps that behind explicit opt-in. The
-- point of landing the schema first is that fencing must exist *before*
-- concurrency, never after: a guarantee added on top of running concurrent
-- writers is a guarantee that was absent for every turn already executed.
--
-- A lease is a time-bounded, monotonically fenced claim on one request_id. The
-- fence, not the worker identity, is what authorizes a durable write. A TTL
-- alone cannot stop a worker that stalls past its expiry and then wakes up
-- believing it still owns the turn; only a fence comparison performed in the
-- same transaction as the write can refuse it.
BEGIN IMMEDIATE;

CREATE TABLE worker_leases (
 request_id TEXT PRIMARY KEY REFERENCES chat_receipts(request_id) ON DELETE RESTRICT,
 worker_id TEXT NOT NULL CHECK(length(worker_id) BETWEEN 1 AND 128),
 -- Monotonic per request. Bumped on every acquisition or steal; never reused.
 fence INTEGER NOT NULL CHECK(fence > 0),
 acquired_at TEXT NOT NULL,
 renewed_at TEXT NOT NULL,
 expires_at TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('held','released','expired','stolen')),
 -- Leases compare database-issued times, never a worker's local wall clock, so
 -- clock skew between workers cannot manufacture or extend ownership.
 CHECK(renewed_at >= acquired_at),
 CHECK(expires_at > renewed_at)
);

-- Claiming scans for work whose lease is absent or lapsed; both predicates hit
-- this index rather than the table.
CREATE INDEX worker_leases_expiry ON worker_leases(state, expires_at);
CREATE INDEX worker_leases_worker ON worker_leases(worker_id, state);

-- The fence is only useful if it cannot go backwards. Enforcing that in the
-- schema makes stale-writer protection a property of the database instead of a
-- property of worker good behavior: a buggy or stalled worker cannot lower a
-- fence even if it tries.
CREATE TRIGGER worker_leases_fence_monotonic BEFORE UPDATE ON worker_leases
WHEN NEW.fence < OLD.fence BEGIN
 SELECT RAISE(ABORT,'worker lease fence must never decrease');
END;

-- Re-acquiring or stealing must mint a strictly higher fence. An update that
-- changes the holder while reusing the fence would let two workers present the
-- same authorization.
CREATE TRIGGER worker_leases_steal_bumps_fence BEFORE UPDATE ON worker_leases
WHEN NEW.worker_id <> OLD.worker_id AND NEW.fence <= OLD.fence BEGIN
 SELECT RAISE(ABORT,'a lease handover must raise the fence');
END;

-- A lease row is the durable record of ownership history for its request. If a
-- row could be deleted, the next acquisition would restart the fence at 1 and a
-- pre-crash writer holding the old fence would be authorized again. Terminal
-- states are recorded by updating `state`, not by removing the row.
CREATE TRIGGER worker_leases_no_delete BEFORE DELETE ON worker_leases BEGIN
 SELECT RAISE(ABORT,'worker leases are append-only; set state instead of deleting');
END;

-- A lease belongs to the turn it was minted for, permanently.
CREATE TRIGGER worker_leases_request_immutable BEFORE UPDATE ON worker_leases
WHEN NEW.request_id <> OLD.request_id BEGIN
 SELECT RAISE(ABORT,'a lease cannot be moved to another request');
END;

PRAGMA user_version=17;
COMMIT;
