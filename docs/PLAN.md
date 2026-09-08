# Implementation plan and to-do list

Status: core implementation candidate delivered; native Rust release validation remains OPEN.

## Phase 1 — Implemented in source

- [x] Isolate source changes from the original archive/database.
- [x] Require Bearer authentication and loopback binding.
- [x] Add Origin checks, payload/concurrency limits, queue limits, and no-cache/security headers.
- [x] Remove server-side path ingestion; provide bounded local upload CLI.
- [x] Add conservative secret filtering before ordinary storage/provider use; reject credential memories.
- [x] Introduce versioned SQLite schema, foreign keys and FTS5 triggers.
- [x] Add candidates, evidence, project scope, expiration and revision history.
- [x] Make approval/rejection transactional with stale-revision conflict detection.
- [x] Store and replay bounded completed conversation turns.
- [x] Preserve sanitized import sources and normalize messages without lossy pairing.
- [x] Process all normalized content via UTF-8-safe chunks, including after the first 40 messages.
- [x] Use durable jobs, retries, restart recovery, and same-snapshot import deduplication.
- [x] Implement deterministic scoped FTS5 recall with evidence-linked records.
- [x] Add browser approval inbox, job retry UI, model settings, and recall explanations.
- [x] Provide safe backup and legacy-to-candidate migration scripts.

A checked implementation item means code exists and has received the available review; it does not imply native execution has passed.

## Phase 2 — Validation / release gates

- [x] Run shipped-schema / mutation-SQL contract tests with a Python transaction driver.
- [x] Run synthetic backup and legacy migration tests.
- [x] Run mocked-API browser flows and responsive visual inspection.
- [x] Check JavaScript and Python syntax and source patch whitespace.
- [x] Add native Rust unit/API tests and local-provider integration smoke test.
- [x] Add CI workflow and clean source-only package.
- [ ] Run `cargo fmt`, `cargo test --locked`, `cargo clippy --locked --all-targets`, and `cargo build --locked` on a Rust-enabled machine.
- [ ] Run `python3 tests/integration_smoke.py` against the compiled application.
- [ ] Validate real provider model names, output schema compatibility, timeout behavior and costs using non-sensitive synthetic inputs.
- [ ] Run controlled cancellation/crash/failure-injection tests against the compiled service.
- [ ] Approve a backup/restore drill and migration output before using personal data.
- [ ] Rotate any real credentials exposed in earlier archives (requires the account owner; not performed by this package).

## Phase 3 — Explicit backlog, not included

- [ ] Process lock / leases and graceful multi-worker shutdown before any multi-instance use.
- [ ] Typed error taxonomy, finer rate limiting and persisted retrieval traces.
- [ ] Conflict-rebase / edit proposal UI; migration scope curation tools.
- [ ] Archive/forget UI and complete deletion/retention policy across replicas/backups.
- [ ] Encrypted exact raw archive and secret-vault integration.
- [ ] Incremental appended-file ingestion with stable source offsets and cross-snapshot deduplication.
- [ ] Full agent-format fixture corpus, streaming/tool-call replay and source-version compatibility matrix.
- [ ] Search archived episodes, not only distilled preferences.
- [ ] Full OpenAI-compatible endpoint and SSE streaming.
- [ ] Labeled retrieval evals: no-memory baseline, lexical recall, then optional semantic recall.
- [ ] Opt-in shadow experiments with latency/cost and stale-memory metrics.
- [ ] Git-style memory diffs/export, temporal context, verified procedural memory.

## Acceptance criteria for a real release

1. No import automatically changes an active memory.
2. Repeated/conflicting approvals cannot produce inconsistent state.
3. No acknowledged successful turn lacks its persisted answer/job.
4. Every accepted source chunk is observable as pending, running, done or failed.
5. Prior completed turns reach the mock provider in the right order.
6. Wrong-token/foreign-origin requests cannot reach protected handlers.
7. Stored/tool-output instructions never gain command authorization.
8. Backups restore committed WAL state, and the original database is never overwritten.
9. All native and integration gates pass; unresolved failures remain visible rather than being described as complete.
