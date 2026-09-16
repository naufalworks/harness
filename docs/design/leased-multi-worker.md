# Design: leased multi-worker execution

## Status

**Approved (2026-09-16). Schema landed; no second worker enabled.** The owner approved multiple
writers and selected **Resolution B**, with one hard constraint: no race conditions, and the system
robust, fast and optimized. Rollout gate 2 is complete — `migrations/017_worker_leases.sql` adds the
`worker_leases` table, a per-request monotonic fence, and triggers that enforce fencing at the
schema level — with the worker count still pinned at one. Gates 3 and 4 are outstanding: no worker
identity, lease client, heartbeat, or second worker exists yet, and no code path admits one.

Fencing was deliberately landed *before* any concurrency. A guarantee added after concurrent writers
are already running is a guarantee that was absent for every turn executed until then.

## Thesis

Today exactly one Harness process owns the database, and that single owner is what makes the
system's safety arguments simple: a turn cannot be run twice, a cancellation cannot race a late
spawn, and an outbox row cannot be delivered by two senders. Scaling to more than one worker is
therefore not a throughput change — it is the removal of the assumption every side-effect guarantee
currently rests on.

The design goal is not "run N workers". It is: **a turn is owned by exactly one worker at a time,
ownership is provable, and a worker that has lost ownership cannot still commit a side effect.**

## What exists today

| Mechanism | File | What it guarantees |
|---|---|---|
| Process ownership | `src/process_lock.rs` | One process holds a kernel `flock` for its whole lifetime. Metadata can survive a crash; the kernel lock cannot, so stale ownership is recoverable without trusting a recorded PID. |
| Cancellation safety | `src/processes.rs` | Process groups are registered per request. `terminate` *seals* the request before draining, so a group spawned in the spawn→register gap is killed on sight rather than surviving a cancelled turn. |
| Durable cancel record | `migrations/009_run_cancellation.sql` | `run_controls` is the durable truth of a cancellation, independent of live process state. |
| At-least-once delivery | `migrations/002_recording.sql` | `recording_outbox` decouples producing an event from delivering it. |
| Frozen run state | `migrations/016_run_capsules.sql` | Content-addressed capsules pin the inputs of a run. |

The single `flock` owner is load-bearing. Any multi-worker design must either keep one writer or
replace this guarantee with an explicit, testable substitute — not silently drop it.

## Scope

In scope: multiple workers executing *different* turns concurrently against one database, with
leases, fencing, crash recovery, and exactly-once external side effects.

Explicitly out of scope:

- **Multi-user tenancy.** Per the roadmap, this is a separate product and security decision, not a
  side effect of scaling. Leases grant no authorization.
- **Splitting one turn across workers.** A turn stays whole; only whole turns are distributed.
- **Replacing SQLite.** A different engine is a distinct, larger decision.
- **Remote/distributed workers across machines.** Single-host, multi-process first.

## Lease model

A lease is a time-bounded, monotonically fenced claim on one unit of work.

- **Unit.** One `request_id`. Never a step, never a tool call.
- **Holder.** One `worker_id`, stable for a worker process's lifetime.
- **Term.** A short TTL (proposed: 30 seconds) with heartbeat renewal, deliberately much shorter
  than a long turn. A worker that cannot renew loses the turn even if it is still alive; liveness is
  proven continuously, not assumed from a successful start.
- **Fencing token.** A database-issued integer that increases on every acquisition or steal of a
  lease. The token, not the worker identity, is what authorizes a write.

Proposed `migrations/017_worker_leases.sql` (sketch, not written):

```sql
CREATE TABLE worker_leases (
  request_id   TEXT PRIMARY KEY,
  worker_id    TEXT NOT NULL,
  fence        INTEGER NOT NULL,
  acquired_at  TEXT NOT NULL,
  renewed_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  state        TEXT NOT NULL CHECK (state IN ('held','released','expired','stolen'))
);
```

Acquisition is a single conditional statement, so two workers cannot both believe they won:
claim only when no row exists, or the existing row has expired, and bump `fence` in the same
statement. A lost race is an ordinary, expected outcome and must not be an error.

## Fencing: the part that actually prevents duplicates

A TTL alone is insufficient. The classic failure is a worker that stalls (long GC pause, frozen
host, suspended container), loses its lease, then resumes and completes a write believing it is
still the owner. TTL expiry does not stop that write; fencing does.

Rules:

1. Every durable write made on behalf of a turn carries the fence the writer believes it holds.
2. A write whose fence is lower than the lease's current fence is **refused**, not merged, retried,
   or logged-and-continued.
3. The check and the write happen in one transaction. A check followed by a separate write
   reintroduces the race it was meant to close.
4. A refused write is terminal for that worker's attempt: it releases and stops rather than
   re-acquiring and rewriting.

This makes stale-writer protection a property of the schema rather than of worker good behavior.

## Recovery

Crash recovery must distinguish three cases without guessing:

| Case | Detection | Action |
|---|---|---|
| Worker exited cleanly | Lease `released` | Work is complete or explicitly abandoned; no recovery. |
| Worker died | Lease expired, no heartbeat | A new worker may steal with a higher fence. The dead worker's writes are already unable to land. |
| Worker is alive but partitioned/stalled | Lease expired, process may still exist | Identical handling. This is the point of fencing: the design never needs to answer "is it really dead?" |

On steal, the new holder must treat the turn as **resumable only from durable state**. In-memory
progress of the previous holder is unrecoverable and must not be inferred. A turn that cannot be
resumed from durable state is failed explicitly, not silently restarted — restarting is precisely
how a side effect gets duplicated.

## No duplicate side effects

The hard requirement: a stolen or retried turn must not repeat an *external* effect (a write to a
file, a provider call that costs money, a delivered webhook).

- **Internal database writes** are protected by fencing.
- **External effects** need an idempotency key derived from `(request_id, step identity, fence-independent
  payload digest)`. Fence must *not* be part of the key, or a steal would mint a new key and permit
  the duplicate the key exists to prevent.
- **Effects must be recorded before they are considered done,** and the record consulted before
  re-attempting. An effect whose outcome is genuinely unknown after a crash is reported as `unknown`
  and surfaced for human decision, consistent with the project's existing refusal to invent
  evidence. It is not optimistically retried.
- **`recording_outbox` is at-least-once, so consumers must be idempotent.** Leases do not upgrade it
  to exactly-once, and the design should not pretend otherwise.

## Interaction with the existing process lock

This is the sharpest open conflict. `process_lock.rs` grants one process exclusive ownership of the
database; a second worker cannot start today, by construction. Two coherent resolutions:

- **A. Keep one writer.** Workers execute turns, but all durable writes funnel through the single
  lock-holding process. Preserves every current guarantee, adds an internal boundary, gains less
  parallelism. Leases still needed to assign turns.
- **B. Multiple writers under WAL.** The process lock is narrowed from "owns the database" to "owns
  its worker identity", and correctness moves entirely onto leases plus fencing. Broader blast
  radius; requires proving SQLite write contention and busy-timeout behavior under real concurrency.

Recommendation was **A first**. **The owner chose B**, accepting multiple writers provided there are
no race conditions. That choice is recorded rather than quietly reinterpreted, and it raises the
evidence bar rather than lowering it: because B removes the single-writer invariant every current
side-effect guarantee rests on, correctness now depends entirely on leases plus fencing being
correct. Two obligations follow before any second writer runs, and neither is satisfied by the
schema alone:

- **SQLite write contention must be measured, not assumed.** WAL permits one writer at a time;
  concurrent writers serialize and can return `SQLITE_BUSY`. Busy-timeout behavior under real
  contention has to be demonstrated, since "fast" and "contended" are the claims most likely to
  conflict here.
- **Every durable write on behalf of a turn must carry its fence** and be refused in the same
  transaction if stale. Until that is true of every write path, adding a writer would create exactly
  the race the owner ruled out.

Staging through A is therefore skipped as a rollout step, but its safety property is not: the worker
count stays at one until the obligations above are met with evidence.

## Failure modes this design must be tested against

- Two workers race to acquire the same fresh turn — exactly one wins, loser is not an error.
- Lease expires mid-turn; original worker resumes and attempts a write — refused on fence.
- Worker dies holding a lease — another worker steals after TTL; no duplicate external effect.
- Cancellation arrives at the worker that no longer holds the lease — cancellation still honored via
  `run_controls`, which is already independent of live process state.
- Clock skew between workers — leases must compare database-issued times, never local wall clocks.
- Heartbeat storms / renewal under write contention — renewal must not be starved by turn work.

Each becomes a test before any second worker is enabled. The task's own `verify` command
(`cargo test --locked recovery && python3 tests/test_migrations.py`) is the floor, not the ceiling.

Landed so far, at the schema level (`tests/test_migrations.py::test_017_worker_lease_constraints`):
a fence cannot decrease; a handover cannot reuse a fence; a lease row cannot be deleted, which would
reset the fence and re-authorize a pre-crash writer; a lease cannot be moved between turns; states
are constrained so recovery never guesses; and a held, unexpired lease is not claimable, with the
lost race affecting zero rows rather than raising an error. Each trigger was mutation-tested by
dropping it and confirming the guarded write then succeeds — which caught two assertions that had
been passing for the wrong reason.

Still untested, because they need a lease client rather than a schema: heartbeat renewal under write
contention, clock-skew comparisons using database-issued time, steal-with-no-duplicate-external-
effect, and cancellation reaching a worker that no longer holds the lease.

## Rollout gates

1. Design approved (this document).
2. Lease table, fencing, and the failure-mode tests land with the worker count still pinned at one.
3. A second worker is enabled only behind explicit opt-in, defaulting off, mirroring how the remote
   runner and extension admission are already gated.
4. Documented rollback: pin back to one worker, which must be a configuration change, not a
   migration revert.

## Decisions needed from the owner

1. ~~**Resolution A or B**~~ — **decided: B**, multiple writers under WAL, on the explicit condition
   that there are no race conditions.
2. ~~**Is multi-worker actually wanted now?**~~ — **decided: yes**, "many writers or soon".
3. **Lease TTL and heartbeat interval.** Defaults stand (30s TTL, heartbeat well inside it) unless
   changed; these are cheap to tune once real contention numbers exist.
4. **Behavior for an unknown-outcome external effect.** Still open. The proposal is to surface it
   for human decision rather than retry, consistent with this project's refusal to invent evidence.
   Defaulting to retry would risk duplicating a paid provider call or a delivered webhook, so the
   proposal stands until the owner says otherwise.

## Deferred

Multi-instance semantics across machines, worker pools with priority or fairness, and cross-host
continuation packets all build on leases and are out of scope until leases exist and are trusted.
