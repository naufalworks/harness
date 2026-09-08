# Start here — Harness recording receipts checkpoint

## User intent (do not restart discovery)
The user supplied two ZIPs: a renewed UI/old backend and a security-hardened backend/newer basic UI. They approved implementation, but explicitly asked this session to focus on ONE meaningful improvement and preserve the rest for later sessions.

Their priorities:
- Do not lose conversations or artifacts. Recording must not depend on memory approval.
- Keep previously approved memories when their approval can be verified; no hundreds of unnecessary clicks.
- Bring contextual, one-click memory suggestions back into chat.
- Clean, modern, simple, responsive, informative UI matching the supplied renewed UI. Avoid dashboards full of controls and approval interruptions.
- Creative memory ideas are welcome when useful, inspectable, reversible, and reliable—not speculative features claimed as implemented.

## What this session actually built
**Recording receipts**, one vertical slice on the hardened backend:
1. Commit sanitized user message + receipt + durable extraction intent before generation.
2. A serial generation worker—not the HTTP request—owns the model call. Losing a tab/HTTP response does not cancel admitted work.
3. Same request identity + same sanitized content replays the receipt, not the paid generation. Different content is a conflict.
4. Save the exact sanitized provider message array and recalled memory revisions BEFORE invoking the provider. This is a context receipt, NOT chain of thought, causal attribution, or proof that the provider received it.
5. Completed answers commit independently from extraction queue capacity. A durable outbox schedules extraction when capacity returns. A full/failed extraction queue no longer rolls back the answer.
6. Provider failures keep the user message. Restart changes started generation to interrupted, without automatic potentially billed replay. Never-started captured work resumes.
7. Authenticated receipt/context/history endpoints, history pagination and session reopening; understated status and expandable receipt UI.

This is NOT the full merge. The current runnable UI is still the hardened backend's basic UI with these additions. The preferred renewed UI is preserved verbatim in `reference/renewed-ui-original/` for the next UI-focused session. Original ZIPs are included under `reference/source-archives/` in the downloadable package.

## Honest verification status
- PASS: 41 executable SQLite contracts (25 new recording contracts + 16 inherited contracts).
- PASS: two mocked-browser suites, 28 named checks total, including lost response/reload recovery, status, context evidence, mobile/dark mode and HTML injection rendered as text.
- PASS: JavaScript/Python syntax and whitespace checks.
- Native Rust: 21 tests included (8 new); **NOT EXECUTED**. No cargo/rustc in this sandbox; Rust distribution DNS inaccessible.
- Live Rust HTTP tests: two scripts included, **NOT EXECUTED** (no binary).
- No real provider tested. No user DB was supplied or migrated. No service installed/deployed.
- This is a SOURCE IMPLEMENTATION CANDIDATE, not a compiled release. See `docs/VALIDATION.md`.

## FIRST action next session
Obtain a Rust-capable environment, then run `scripts/verify_release.sh`. Fix compiler/integration failures before working on additional features. SQL mirror tests cannot establish Rust orchestration correctness. Run on synthetic fixtures first, not the user's real DB.

## Do not promise more than this checkpoint provides
- Recordings are sanitized plaintext in local SQLite (existing mode 0600 on Unix), NOT encrypted exact originals.
- No general binary artifact vault or arbitrary attachment capture. Existing imports are text-only, bounded, and still coupled to parsing/queue admission.
- No SSE streaming or durable streamed chunks. Only completed sanitized provider text is saved; a crash between provider completion and DB commit can lose that answer. Receipt says interrupted, not complete.
- Drafts and bearer tokens are NOT persisted in browser storage. Unsent raw draft text is lost on reload/lock. Only request/session/scope identifiers are persisted in sessionStorage.
- No semantic archive search, undo-memory UI, balanced memory policy, trust-aware legacy migration, or full renewed-UI merge yet.
- Single trusted user, one process, local filesystem. No multi-instance coordination or hardware-loss guarantee. Backups are still required.
- One unresolved generation per session; up to 100 captured/generating requests globally. Rejected requests are NOT described as recorded.
- Repeating a failed/interrupted request ID only returns its receipt. No auto retry. A genuinely new send uses a new ID and may incur another provider call.

## Roadmap carried forward
See `docs/ROADMAP.md` for ordered acceptance criteria. Recommended sequence after native verification:
1. Exact-original encrypted archive + artifact preservation, explicit opt-in and real restore test.
2. Trust-aware legacy migration and batch adoption without invented approvals/evidence.
3. Full renewed UI integration + balanced inline memory flow.
4. Durable, authenticated streaming with record-before-display and Unicode-safe redaction.
5. Searchable sanitized conversation history, linked provenance, memory undo and decision history.
6. Provider-aware scheduling/rate-limit backoff and optional novel memory experiments.

## Key code locations
- `migrations/002_recording.sql`: additive schema v1 → v2; no fake legacy receipts.
- `src/recording.rs`: admission, receipt queries, generation, completion, outbox, recovery.
- `src/recording_sql.rs`: exact SQL shared with contract tests.
- `src/main.rs`: authenticated submit/receipt/context/history routes.
- `src/memory_agents.rs`: construct provider messages once, persist and send the same array.
- `src/storage.rs`: migration/recovery hookup and outbox dispatch before job claims.
- `static/app.js`: two-phase send, idempotent reconnect, history, receipt progressive disclosure.
- `tests/recording_integration.py`: real compiled-service crash/restart and context equality gate.

## Preserve previous decisions
The earlier proposal required answer + extraction-job commit together. That is deliberately SUPERSEDED: answer + durable extraction INTENT commit is enough; job scheduling must not roll back the answer. Ordinary memory rejection never deletes the underlying chat. Original secret-looking data, when an encrypted archive exists, stays isolated from search/model calls. Do not restore an unencrypted raw log just to claim lossless capture.

Do not use `scripts/migrate_legacy.py` to claim the user's approved-memory concern is solved. It is the inherited quarantine-only importer and remains unchanged. Alleged counts 754 versus 697+58=755 are unverified; no private DB is in either input ZIP.
