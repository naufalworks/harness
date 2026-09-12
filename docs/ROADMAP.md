# Continuation roadmap

> Superseded 2026-09-09 by `docs/PLAN.md` + `docs/TASKS.md` (agentic tool loop, activity UI, context manager, memory kinds). This file is kept for history. Its P0 gate is still required and is tracked as `P0-T01` in TASKS.md; items below that are not in TASKS.md live in its "Ideas parking lot".

Status: only Recording receipts is implemented in this checkpoint; native validation remains blocked. This document records future work, not completion claims.

## P0 · Native verification / release gate
- [ ] Compile with locked dependencies; run 21 Rust tests, Clippy and both live mock-provider HTTP suites.
- [ ] Fix any compiler/lifetime/API issues; regenerate lockfile only if Cargo requires it and review the diff.
- [ ] Exercise actual disk/permission errors, connection drops, process restart and duplicate concurrent submissions against the compiled server.
- [ ] Confirm provider-specific compatibility and timeouts on the user's local installation.
- [ ] Treat the inherited backup/permissions/single-process limitations as real; never upgrade the user's only DB copy.

## P1 · Exact-original archive and artifact safety
Three distinct layers: encrypted exact originals → sanitized searchable history → selected active memory.
- [ ] Explicitly opt into storing exact originals, including secret-looking text; explain unlocked-app/key-compromise risks.
- [ ] Use a reviewed AEAD implementation and versioned encrypted envelopes, unique nonces, key IDs, authenticated metadata, OS-backed key storage where available. No home-grown crypto or keys next to the DB.
- [ ] Store binary artifacts by content digest, preserve names/media types/source associations and version history; stream bounded uploads with checksums and atomic commit.
- [ ] Archive supported-size files BEFORE parser/indexer work. Unsupported parsers report “Archived; could not index” only when archive commit actually succeeded.
- [ ] Keep original bytes out of provider calls and routine indexes. Handle sensitive outputs as well as inputs.
- [ ] Encrypt backups; test restore onto a clean machine, missing keys, corruption, quota exhaustion and interrupted uploads. Define retention, explicit deletion and key rotation.
- [ ] Finish legacy artifact/conversation migration, not only memories.
Acceptance: an accepted artifact survives parser failure, extraction backlog and process restart; byte-for-byte equality after authorized decryption/restore. No silent rejection or exact-original claim for sanitized-only records.

## P2 · Trust-aware legacy migration
- [ ] Read a user-generated LOCAL audit / protected DB backup; never request credentials pasted into chat.
- [ ] Reconcile true counts. Match legacy key/value/category to confirmed pending_confirmations and inspect subsequent overwrites. Confidence 1.0 alone is not approval.
- [ ] Carry demonstrably approved, screened records as active with “Previously approved — legacy” provenance.
- [ ] Batch adoption preview for unconfirmed records: category groups, duplicate/conflict flags, deselection and auditable consent. Do not invent quotes.
- [ ] Flag sensitive records without exposing their content; preserve protected originals separately.
- [ ] Keep global preferences recallable; do not strand all records in legacy-review.
- [ ] Source-preserving, deterministic dry run, idempotence, reconciled counts, rollback and restore tests.
Acceptance: no hundreds of redundant approvals; every activation has a defensible approval/adoption event and original backup remains unchanged.

## P3 · Renewed UI + balanced memory interaction
Use `reference/renewed-ui-original/static/index.html` as the visual/interaction reference, NOT as trusted security architecture.
- [ ] Port clean chat layout, responsive sidebar, light/dark theme control, safe Markdown/code rendering and copy controls.
- [ ] Extract inline scripts/styles to local files; preserve strict CSP. Auth on all APIs and streams.
- [ ] Server history replaces localStorage transcripts. Keep theme preferences local; keep tokens ephemeral unless adopting a reviewed session-auth design.
- [ ] Inline suggestion tray linked to source request: Save / Edit / Dismiss; quote on expansion. Inbox remains for imports/backlog.
- [ ] Structured “Remember selected text” action = authorization for that exact validated fact. Do not infer authorization from quoted prompt text.
- [ ] Suppress redundant suggestions; conflicts show old/new values before replacing anything. Rejecting a suggestion must not delete its source conversation.
- [ ] Optional narrow auto-save for clear low-risk preferences with visible activity/undo; not based on model confidence alone. No sensitive inference or silent overwrites.
- [ ] Resolve unknown-save states gracefully; no forced “check forever” flow. Preserve the current Leave for later escape hatch.
Acceptance: keyboard/mobile/dark/light QA, no XSS, no hidden history, no redundant approval for explicit save, no legacy UI route reintroducing arbitrary server-path imports.

## P4 · Durable streaming (not fake typewriter animation)
- [ ] Authenticated fetch/SSE transport, ordered event sequence and resumable receipt cursor.
- [ ] Persist received chunks BEFORE displaying them; separate completed/interrupted/truncated states.
- [ ] Handle split UTF-8, JSON/SSE frames, timeouts, disconnect/reconnect and provider errors.
- [ ] Security boundary: do not send/display an unredacted prefix while waiting to learn that a later suffix identifies a secret. Line/window buffering and exact-original vault policy need explicit design.
- [ ] Complete answer + extraction intent independent of queue capacity; never downgrade acknowledged durable events.
Acceptance: reconnect without duplicate generation; interrupted streams clearly labeled; tests show byte/order preservation and no credential leakage from split frames.

## P5 · History can help without becoming permanent memory
- [ ] FTS5 over sanitized conversations and artifacts, scoped with message/artifact citations.
- [ ] Distinguish dated historical evidence from active/current preference. Never send the entire archive to the model.
- [ ] Follow context receipts back to memory revisions and source artifacts; inspect what was included/excluded under explicit budgets.
- [ ] Undo/archive memory as a new revision, with clear separate actions for “forget preference” versus “delete source”.

## P6 · Optional distinctive ideas (NOT implemented)
- **Memory branches:** try a new preference within one project without overwriting global behavior; promote after explicit approval.
- **Decision timeline:** distinguish “we considered X” from “we chose X”, and show when a decision was superseded, with dated evidence.
- **Memory rehearsal:** preview how a proposed memory changes retrieved context before approving it. Start with deterministic retrieval diffs, not unsupported causal claims about model behavior.
- **Quiet expiry:** time-bound temporary preferences and ask one compact question when evidence conflicts; don't auto-erase the source archive.
- **Portable continuation packet:** export a user-selected, sanitized project checkpoint with source links, unresolved questions and model-context receipts, with audience review before sharing.
- **Rate-aware scheduling:** foreground generation priority, budgeted background imports, Retry-After/jitter and provider concurrency limits; do not auto-repeat potentially billed interrupted generations.

These are product proposals, not claims of research novelty. Reliability and UX take precedence over feature count.

## P8 · Causal observability (new direction)

The next product/research thread is not another span viewer. It is a bounded causal incident graph that connects externally inspectable evidence to decisions, tool calls, permissions, state mutations, and recovery.

- [ ] Define a typed provenance-edge schema and durable references to existing rows.
- [ ] Instrument one coding turn without capturing private chain-of-thought.
- [ ] Add a read-only incident view that starts at a failure and walks upstream/downstream dependencies.
- [ ] Prove denial, stale-anchor, and crash-recovery attribution with real E2E fixtures.
- [ ] Measure earliest-break localization, missing-edge rate, graph size, and reviewer time versus flat trace inspection.

This is a research direction, not a completion claim. The executable backlog lives in `docs/TASKS.md`; design details live in `docs/design/causal-observability.md`.
