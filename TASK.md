# TASK — Measure and improve Harness runtime performance safely

Status: proposed execution brief  
Created: 2026-09-17  
Scope: observability, performance diagnosis, SQLite operations, provider reliability, and deployment hardening

> This file is a focused implementation brief, not the authoritative status ledger. Before implementation, split the selected work into stable entries in `docs/TASKS.md`, follow `AGENTS.md`, and journal every task transition in `docs/PROGRESS.md`.

## 1. Objective

Make normal Harness requests measurably faster and easier to diagnose without weakening recording-first execution, durable receipts, redaction, authentication, permissions, cancellation, lease fencing, spend limits, no-replay recovery, or rollback.

The first deliverable is trustworthy timing evidence—not a speculative optimization. For each accepted request, the evidence should answer:

1. How long did the request take end to end?
2. How much time was spent on admission, queueing, context construction, provider waits, tools, permission waits, SQLite reads/writes, verification, and publication?
3. How many agent steps, provider calls, tool calls, retries, and persisted bytes were involved?
4. Was the request actively working, queued behind a limit, blocked on SQLite, waiting on the network, or waiting for human approval?
5. Which measurements are known, unavailable, or overlapping?

Only after this evidence exists should a bottleneck be named or runtime behavior be changed.

## 2. Corrected current baseline

The earlier review was directionally useful but too confident in several places. Use this corrected baseline:

- Idle CPU and RAM are low, but one idle snapshot cannot rule out saturation, queueing, or latency during real requests.
- Provider latency is a hypothesis until Harness request-level measurements isolate it.
- Empty deployment log files do not prove missing observability. Harness already emits some JSON to journald and stores durable steps, events, provider accounting, incident evidence, and deployment identity in SQLite.
- The current source already configures SQLite WAL and a five-second busy timeout. The writer uses `synchronous=FULL`; bounded readers use `synchronous=NORMAL`.
- Existing maintenance, benchmark, backup, restore, deployment, rollback, smoke, browser, and fault-injection scripts must be reused instead of duplicated.
- Cargo is currently available at `/root/.cargo/bin/cargo`; the previous `cargo: not found` limitation is stale.
- A read performed with immutable mode while WAL is active is not sufficient proof of complete live-database health.
- The production service runs as root. Moving to a dedicated account is a security/reliability project, not a performance optimization, and requires separate rollout evidence.

No production behavior should change merely because it appears in this plan.

## 3. Non-goals

This work must not:

- declare the provider, agent loop, SQLite, frontend, CPU, RAM, or thread count to be the bottleneck before measurement;
- tune compiler flags, binary size, runtime threads, indexes, WAL, or vacuum settings without representative evidence;
- run `VACUUM`, delete WAL files, or copy the live database file as a backup;
- change writer durability from `synchronous=FULL` without an explicit owner-approved durability decision and fault tests;
- log authorization headers, tokens, keys, cookies, full prompts, full responses, tool arguments, command output, diffs, file contents, exact-original private data, or hidden reasoning;
- expose an unauthenticated metrics or debugging endpoint;
- use unbounded labels such as request IDs, session IDs, paths, arbitrary errors, prompts, or arguments in aggregate metrics;
- retry permanent failures, cancellations, spend refusals, or external effects with unknown outcomes;
- hold SQLite transactions during provider, tool, browser, permission, or network waits;
- deploy automatically as part of verification;
- alter recovery archives or verified backups;
- enable multiple workers incidentally.

## 4. Invariants

Every implementation slice must preserve:

1. Durable intent is committed before provider calls and non-replayable external effects.
2. Missing evidence is reported as unavailable, never converted to zero or success.
3. Restart never automatically repeats an uncertain provider, command, or browser effect.
4. Worker-owned writes remain protected by lease fencing.
5. User- and model-facing content continues through the existing redactor.
6. New HTTP surfaces inherit authentication, Origin, rate, and body-size controls.
7. Logs, labels, payloads, queues, retention, and responses remain explicitly bounded.
8. Instrumentation does not delay, suppress, or reinterpret cancellation.
9. Writer durability remains `synchronous=FULL` by default.
10. Every deployment change has a binary/config/schema-aware rollback path.

## 5. Evidence model

Use three complementary surfaces.

### 5.1 Durable request evidence

Extend existing receipts, steps, provider calls, and activity events when evidence is needed for restart interpretation, user-visible history, or incident diagnosis. Any schema change must be append-only, versioned, bounded, and migration-tested.

### 5.2 Structured operational events

Emit one-line versioned JSON events to stderr for journald. They are an operational aid, not a replacement for durable receipts.

### 5.3 Low-cardinality aggregates

If an authenticated metrics endpoint is added, aggregate only by bounded dimensions such as endpoint class, operation class, outcome, provider role, and tool name from the fixed registry. Never attach raw IDs, paths, errors, prompts, or arguments as metric labels.

Document which surface is authoritative for each field. Avoid independently storing the same value in several places unless one copy is explicitly derived.

## 6. Timing semantics

Use `std::time::Instant` for elapsed durations and UTC timestamps only for durable ordering/correlation.

Every duration must define:

- start boundary;
- stop boundary;
- unit;
- whether child work is included;
- whether it overlaps another category;
- missing-value behavior;
- maximum value;
- stable outcome vocabulary.

Do not add component timings and claim they equal total time unless they are explicitly non-overlapping.

## 7. Required measurements

### 7.1 Request summary

Capture or derive:

- event schema version;
- request ID only in durable/private evidence;
- bounded endpoint class;
- accepted and terminal timestamps;
- total duration;
- admission/auth duration where it can be measured without changing middleware behavior;
- queue wait duration and bounded queue name;
- context-build duration;
- agent-loop duration;
- provider wait total;
- tool execution total;
- permission wait total;
- verification duration;
- durable publication duration;
- SQLite read/write totals and operation counts;
- agent-step, provider-call, tool-call, and retry counts;
- input/output tokens when reported by the provider;
- response bytes;
- terminal state and stable error code;
- an explicit list of unavailable measurements.

### 7.2 Provider calls

For roles `main`, `subagent`, `compaction`, `verification`, and `extraction`, measure:

- reservation-to-dispatch duration;
- connect, first-byte, stream, and total duration when boundaries are trustworthy;
- stream chunk count and published bytes;
- usage/cost reporting state;
- attempt number and retry decision;
- bounded HTTP/error class without response bodies;
- cancellation, timeout, parse-failure, and unknown-dispatch outcomes separately;
- circuit-breaker transitions if that feature is later introduced.

### 7.3 Agent steps and tools

Measure:

- step kind and durable sequence;
- queue wait;
- execution duration;
- tool name from the registered set;
- permission wait;
- diagnostics duration;
- result bytes and truncation state;
- stable outcome/error code;
- bounded sub-agent provider/tool counts.

Do not add raw tool arguments or output merely for timing.

### 7.4 SQLite

Instrument named repository operations, never arbitrary SQL text:

- semaphore, writer-mutex, and reader-pool wait;
- connection acquisition;
- closure/transaction duration;
- commit duration;
- busy/locked/timeout/queue-full outcomes;
- bounded rows read or changed;
- WAL/checkpoint state sampled through maintenance, not per-query filesystem scans.

## 8. Execution phases

### Phase 0 — Register and baseline

1. Split this plan into stable `docs/TASKS.md` entries with required metadata.
2. Map them to existing P11, P14, and P16 roadmap coverage; create a new phase only if existing coverage truly cannot contain the work.
3. Use one branch/worktree per task.
4. Record source commit, live binary identity, schema, redacted runtime configuration, and toolchain versions.
5. Run the strict non-deploying release gate before runtime changes.
6. Create a deterministic synthetic-provider timing fixture. Live-provider samples are supplemental and require explicit spend limits.

Expected: a reproducible baseline with failures and skipped lanes reported honestly.

### Phase 1 — Define the contract

1. Write a design document covering timing boundaries, event names, types, cardinality, retention, privacy, and failure behavior.
2. Inventory existing durable timestamps/events before adding schema.
3. Derive values from existing records where accuracy is sufficient.
4. Add a migration only where current rows cannot represent evidence truthfully.
5. Version structured events, for example `harness.runtime/v1`.
6. Define bounded error classes and overlapping-duration rules.

Expected: reviewers can explain every number before code emits it.

### Phase 2 — Instrument request admission

1. Add Axum request timing without changing auth/origin/rate-limit ordering.
2. Classify rejected traffic separately from accepted work.
3. Record admission, queueing, handler completion, response status class, and response bytes.
4. Correlate accepted chat submissions with the existing durable request ID.
5. Emit one bounded terminal summary with explicit unavailable fields.
6. Test success, validation failure, auth failure, timeout, cancellation, and internal failure.

Expected: each accepted request has one trustworthy terminal duration.

### Phase 3 — Instrument context and agent loop

1. Measure initial context construction and compaction.
2. Measure the actual provider future, not an external proxy probe.
3. Measure tools, approvals, diagnostics, verification, and durable publication.
4. Reuse loop counters instead of recomputing them from logs.
5. Keep instrumentation outside held transactions unless measuring that transaction.
6. Preserve cancellation polling and deadlines exactly.
7. Prefer controlled synthetic timing tests over flaky wall-clock assertions.

Expected: a slow turn decomposes into evidence-backed stages without exposing content.

### Phase 4 — Instrument storage boundaries

1. Measure blocking-task queue, writer mutex, reader pool, transaction, statement, and commit boundaries.
2. Declare a bounded set of operation names in code.
3. Count `SQLITE_BUSY`, `SQLITE_LOCKED`, timeout, and queue-full outcomes.
4. Generate production-sized synthetic data rather than copying private production records.
5. Exercise concurrent reads, serialized writes, WAL growth, held readers, checkpoint behavior, restart, and cancellation.
6. Keep query plans in offline benchmark artifacts only.

Expected: SQLite can be confirmed or ruled out under representative load.

### Phase 5 — Add a safe diagnostic surface

1. Prefer an authenticated bounded JSON summary integrated into the existing API.
2. Return rolling counts/distributions for fixed windows, queue depth, and worker readiness.
3. Exclude secrets, content, paths, database locations, arbitrary labels, and raw errors.
4. Cap response size and computation time.
5. Metrics failure must not fail an otherwise valid user request; durable correctness writes remain fail-closed.
6. Add API schema tests and browser tests if the data is shown in the UI.

Expected: an operator can answer “what is slow?” without inspecting private content.

### Phase 6 — Establish representative baselines

Run disposable scenarios for:

- readiness;
- short synthetic-provider chat;
- multi-step read-only tools;
- approved and denied writes;
- provider timeout;
- one bounded transient retry;
- cancellation during provider wait and tool execution;
- long-history context construction;
- concurrent reads and serialized writes;
- held WAL reader/checkpoint;
- restart recovery without external-effect replay.

For every scenario report sample count, median, p95, maximum, stage shares, errors/timeouts, database/WAL change, CPU, peak RSS, commit identity, fixture digest, and limitations.

Do not invent thresholds before collecting a baseline. The owner approves budgets afterward.

### Phase 7 — Optimize the measured bottleneck

Choose the largest actionable contributor and make the smallest reversible change. Candidates only when supported by evidence:

- remove repeated history reads;
- avoid unchanged context reconstruction;
- reuse a prepared statement on a measured hot path;
- add an index justified by representative `EXPLAIN QUERY PLAN` output;
- batch writes belonging to one atomic transition;
- reduce duplicate serialization/copying;
- remove unnecessary provider calls;
- parallelize independent read-only work within existing budgets/receipts;
- reduce duplicate frontend requests;
- compact retained events under an approved policy.

Acceptance requires the identical before/after fixture, unchanged correctness evidence, and no regression outside approved budgets.

### Phase 8 — Evaluate SQLite maintenance separately

1. Monitor DB/WAL/SHM size, page count, freelist, checkpoint state, and held readers.
2. Reuse the maintenance and online backup paths.
3. Define WAL thresholds from measured workload.
4. test checkpoint modes on disposable copies before production.
5. Preserve writer `synchronous=FULL`.
6. Propose `VACUUM` only when reclaimable space or measured behavior justifies its blocking/I/O cost.
7. Before production maintenance, create an encrypted online backup and pass a restore drill.

Expected: maintenance follows evidence rather than being used as a generic speed fix.

### Phase 9 — Provider reliability

1. Inventory existing deadlines, limits, and circuit behavior before adding settings.
2. Separate connect, first-byte, idle-stream, and total deadlines when needed.
3. Retry only explicitly transient classes and honor bounded `Retry-After`.
4. Use capped exponential backoff with jitter and a small attempt ceiling.
5. Never retry cancellation, authentication failure, invalid requests, deterministic parse failures, spend refusals, or unknown external-effect outcomes.
6. Preserve effect reservation/settlement and lease fencing.
7. Expose provider queue time separately from network wait.
8. Test breaker transitions with a synthetic provider.

Expected: transient failures recover within a bounded policy without amplifying outages.

### Phase 10 — Deployment source of truth

Enforce this flow:

```text
clean source commit
  -> candidate build
  -> strict non-deploying verification
  -> encrypted online backup and restore drill
  -> staged process with disposable database
  -> authenticated readiness/identity check
  -> production promotion
  -> authenticated smoke test
  -> rollback evidence
```

Required controls:

- one authoritative production binary, environment file, and database path;
- staging/recovery artifacts never auto-selected as production;
- source commit and binary SHA-256 recorded;
- schema compatibility checked before replacement;
- `._*`, temporary, recovery, and backup files excluded from artifact selection;
- secrets never printed;
- bounded backup retention and encrypted off-server copies;
- rollback never restores a database automatically;
- post-deploy health checks HTTP, database readability, schema, workers, and binary identity.

### Phase 11 — Leave root execution as an independent change

1. Create a non-login service account.
2. Inventory every required read/write/execute path, including configured project scopes.
3. Ensure tool access is not silently broken by ownership changes.
4. Move runtime data and secrets to narrow permissions.
5. Add systemd filesystem protections incrementally.
6. Test backups, browser/LSP subprocesses, diagnostics, project tools, shutdown, and rollback.
7. Preserve an immediate rollback until validation passes.

Expected: Harness no longer requires UID 0, or the remaining requirement is explicitly documented and mitigated.

## 9. Verification

Each implementation task runs focused tests and at minimum:

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
git diff --check
```

Before integration or deployment:

```bash
bash scripts/verify_release.sh
```

Performance tasks must run the same benchmark before and after. Migration tasks also run migration, SQL-contract, backup, restore, and fault-injection lanes. API/UI tasks run schema, frontend syntax, mocked-browser, and real browser-to-server E2E lanes declared by the repository.

A skipped tool/runtime is not a pass. Mark the task `needs-verify` or `blocked` and record the missing evidence.

## 10. Production rollout

1. Confirm the reviewed commit and clean source tree.
2. Confirm the task owns a separate branch/worktree.
3. Run strict verification without touching production.
4. Record the current binary hash, commit, schema, DB/WAL sizes, and readiness.
5. Create an encrypted online backup and restore it in a disposable location.
6. Build and stage without replacing the running binary.
7. Validate the candidate against a disposable database and synthetic/approved provider.
8. Run authenticated readiness, API, browser, cancellation, and recovery smoke tests.
9. Gracefully drain production.
10. Promote only reviewed binary/config changes; never overwrite production DB during binary promotion.
11. Verify identity, schema, DB/workers, authenticated flow, and redacted logs.
12. Observe bounded metrics during a defined canary period.
13. Roll back immediately if acceptance fails and preserve diagnostic evidence.
14. Record deployment provenance and result.

## 11. Rollback triggers and procedure

Rollback if:

- readiness reports the wrong binary/schema or fails;
- authentication/origin enforcement weakens;
- integrity or migration checks fail;
- accepted work lacks durable receipts;
- duplicate external effects appear;
- cancellation fails;
- secrets/private content enter logs or metrics;
- approved latency/error budgets regress;
- SQLite busy errors materially increase;
- WAL or memory grows without bound;
- worker queues do not recover;
- systemd enters a restart loop.

Procedure:

1. Stop or drain the candidate.
2. Restore the previous binary and configuration.
3. Do not restore the database automatically.
4. Start the old binary only if it supports the current schema.
5. Verify authenticated readiness, identity, DB readability, and a representative request.
6. Preserve logs, summaries, deployment identity, and backup references.

Database restoration is a separate explicit decision because it can discard accepted work.

## 12. Expected deliverables

- approved observability design and exact timing semantics;
- stable entries in `docs/TASKS.md` and journal entries in `docs/PROGRESS.md`;
- bounded structured runtime events;
- per-request timings linked to existing receipts and steps;
- authenticated low-cardinality summaries;
- deterministic performance fixtures and owner-approved budgets;
- before/after evidence for every optimization;
- tested WAL/checkpoint policy or evidence-backed decision to retain current behavior;
- bounded provider timeout/retry/breaker policy when justified;
- documented production source of truth;
- verified backup, restore, rollout, and rollback procedures;
- separate reviewed service-user migration.

## 13. Definition of success

Success means:

1. A normal request decomposes into documented measured stages.
2. The largest latency contributor is identified from representative evidence.
3. At least one targeted change improves an approved user-visible metric reproducibly.
4. Recording, redaction, authentication, permissions, cancellation, fencing, spend, recovery, and no-replay gates remain green.
5. Production provides enough safe evidence to diagnose slow/failing requests without exposing private content.
6. Deployment identifies the exact commit, binary, schema, database, and rollback candidate.
7. Every performance claim names its command, fixture, sample window, and limitations.

## 14. First recommended implementation slice

Define the observability contract and instrument one synthetic-provider chat turn end to end:

- accepted request to terminal receipt;
- context construction;
- provider wait;
- tool execution;
- permission wait;
- SQLite queue/read/write/commit boundaries;
- verification and publication;
- explicit unavailable fields;
- one bounded terminal JSON event;
- tests proving no prompt, token, key, argument, output, path, or response body is emitted.

Run the fixture repeatedly, report median/p95/max with sample count and commit identity, then select the next optimization from the measured largest contributor. Do not deploy until the strict gate, staging run, leak tests, backup/restore drill, and rollback checks pass.
