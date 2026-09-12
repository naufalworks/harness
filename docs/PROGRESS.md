# PROGRESS — journal

## 2026-09-11 · Notion AI via Local · P7-T05 design and executable publication spec

- Drafted `docs/design/incremental-publication.md`: today's whole-answer boundary, the constraint that actually blocks incremental publication, invariants I1–I5, the `safety::StreamRedactor` shape, the rejected alternative, storage/UI impact, and the test matrix.
- Grounding finding: `safety::redact` decides per line, replaces a matched line whole, and carries `-----BEGIN … PRIVATE KEY-----` state across lines. A pattern can therefore still be completed by later bytes of the same line (`pass` + `word=hidden`), and publication is durable and append-only, so a released partial line could never be retracted. The safe publication unit is a completed line, not a provider chunk and not a token.
- Recorded the rejection of intra-line masking: it would require changing `redact` from dropping a line to masking a span, which weakens a deliberately conservative module and needs its own task rather than arriving as a side effect of streaming work.
- Added `tests/test_incremental_publication.py` as the executable spec. It derives the marker vocabulary, the `sk-`/`AKIA` thresholds and the redaction marker from `src/safety.rs` so the reference cannot drift, then proves chunk-split equivalence, monotone-prefix publication, holdback until a line terminator, and tail discard on failure — over split-secret, private-key, CRLF, Unicode, token-shape, trailing-newline and no-newline fixtures, across every single cut, one-character chunks and seeded multi-cuts.
- Repointed `P7-T05`'s design link from the PLAN anchor to the new doc, added the spec to its file list, appended a design-ready note, and refreshed the PLAN P7 paragraph.
- Verified here: `python3 tests/test_incremental_publication.py` (7 tests, OK) and `python3 -m unittest discover -s tests -p 'test_*.py'` (60 tests, OK, up from 53), with migrations `001 -> 002 -> 003 -> 004 -> 005` and tool schemas 13 files / 17798 bytes still reported by the suite.
- Not verified here: no `cargo`, so `safety::StreamRedactor` is designed and specified but not implemented or gated; `npm`, `npx` and the `playwright` module are still absent, so `P7-T06` stays blocked.
- Next: on a cargo host, implement `StreamRedactor` and the per-publication `chunk` row against this spec, then run `cargo test --locked streaming && cargo test --locked redact && bash scripts/verify_release.sh`.

## 2026-09-11 · Notion AI via Local · P7 phase closure and remaining-work split

- **Resumed and re-verified the checkpoint before changing anything**: `/root/development/harness` on `p7-frontend-generation-stream`, clean tree, HEAD `b6a5fb1` "feat(ui): render durable generation events", and `git rev-list --left-right --count` against `origin/p7-frontend-generation-stream` reporting `0 0`.
- **Closed the umbrellas**: `P7-T01` and `P7-T02` were still `doing` even though every subtask beneath them (`P7-T02a`, `P7-T02b`, `P7-T02c`, `P7-T04`, `P7-T03`) is `done`. Both are now `done` with notes that map each `done-when` clause to the subtask that satisfied it and cite the `b6a5fb1` gate run (161 Rust tests, Clippy/build, 53 Python contracts, migration chain through 005, both local HTTP suites, frontend syntax) as their verification evidence. No code changed, so no new Rust result is claimed.
- **Scheduled what is actually left**: the two open items are now tasks instead of prose. `P7-T05` (todo) owns safe incremental publication and inherits the `P7-T02a` decision that whole-answer buffering holds until a boundary-aware redactor proves a secret split across provider chunks is never published early. `P7-T06` (blocked) owns making the browser suites executable and records the exact owner commands.
- **PLAN refreshed**: the P7 section no longer prescribes the finished `P7-T02c` → `P7-T04` → `P7-T03` order; it states what landed, that the umbrellas are closed on that evidence, and names the two remaining threads.
- **Verified here (docs-only change)**: `python3 -m unittest discover -s tests -p 'test_*.py'` 53 tests OK, `python3 tests/test_migrations.py` reporting `001 -> 002 -> 003 -> 004 -> 005, user_version=5, data/FTS/FKs preserved`, `python3 tests/recording_integration.py` PASS, `python3 scripts/gen_tool_schemas.py` 13 files, `node --check` on `static/app.js`, `tests/recording_ui.cjs` and `tests/ui_smoke.cjs`, and `git diff --check`.
- **Not verified here**: this host has `node`, `python3` and `git` but no `cargo`, `rustc`, `npm`, `npx` or `playwright` module, so `bash scripts/verify_release.sh` exits 2 (`BLOCKED: Rust/cargo required`) and the browser fixtures cannot run. `require('playwright')` returns `MODULE_NOT_FOUND`, which confirms the reported Playwright limitation is a host gap rather than a defect.
- **Next**: `P7-T05` is the next implementation task and needs the Rust toolchain, so it must run on a cargo-capable host. `P7-T06` needs an owner decision about installing a browser runtime.

## 2026-09-11 · Notion AI via locally · P7-T03 durable generation UI

- **Merged baseline**: fast-forwarded the completed P7 replay-attribution and migration-reporting commits into `main` at `c7e35bf` and pushed `origin/main` before starting the UI work; no conflicts or history rewrite were needed.
- **Durable answer path**: the chat subscribes with authenticated `fetch` to `/generation/stream`, parses split UTF-8/SSE frames, advances only on increasing database sequence IDs, stores the cursor with the pending request, and resumes from it after reload. `/generation` is the fallback when streaming is unavailable.
- **Visible states**: whole answers appear atomically from persisted `complete` events rather than the receipt response or a typewriter effect. Generating, failed, and interrupted states have explicit text-only cards, and reopened failed/interrupted history keeps the same distinction.
- **Coverage**: the mocked browser fixture now serves generation polling/SSE rows and asserts durable complete content, persisted cursor resume, authenticated generation subscription, and failed/interrupted cards. All content is assigned through `textContent`.
- **Verification**: `node --check static/app.js`, `node --check tests/recording_ui.cjs`, `git diff --check`, and `bash scripts/verify_release.sh` passed: 161 Rust tests, Clippy/build, migration chain through 005, 53 Python contracts, schemas, both local HTTP suites, and frontend syntax. The Playwright browser fixture could not execute because this checkout has no `playwright` module; its syntax passed and the release gate does not include browser suites.

## 2026-09-11 · Notion AI via locally · P7-T04 truthful migration reporting

- **Fixed**: `tests/test_migrations.py` derives both its displayed migration sequence and expected latest `user_version` from `CHAIN`, eliminating the stale hard-coded `004` / version 4 success line.
- **Correction**: the earlier P7-T02a journal note saying Python migration coverage stopped at 004 was inaccurate. The test already applied and asserted migration 005; only its printed summary was stale. Historical P6 entries describing 001→004 remain accurate for their dates.
- **Verification**: `python3 tests/test_migrations.py` exits 0 and now reports `001 -> 002 -> 003 -> 004 -> 005, user_version=5, data/FTS/FKs preserved`; `git diff --check` passes.
- **Next**: `P7-T03` is unblocked for frontend durable generation-feed integration.

## 2026-09-11 · Notion AI via locally · P7-T02c attributed generation replay

- **Fixed**: generation polling and SSE projections now preserve each durable event's `request_id`, so multiple turns in one session can be rendered under the correct message.
- **Replay coverage**: native tests prove ordered two-turn attribution, bounded cursor paging, exact tails, wrong-session isolation, completed events, and interrupted recovery attribution. The compiled-server suite compares authenticated SSE frames with polling rows and proves cursor resume has no duplicates.
- **Failure coverage**: the HTTP fixture now verifies interrupted and provider-failed generation rows carry the originating request ID and terminal error code. Invalid cursors, malformed sessions, and unauthenticated polling/streaming remain rejected.
- **Verification**: `cargo test --locked streaming` passed (9 tests), `cargo test --locked generation` passed, and `bash scripts/verify_release.sh` exited 0 with 161 Rust tests, Clippy/build, 53 Python contracts, both local HTTP suites, frontend syntax, and `git diff --check`. Browser suites were not run because no UI changed.
- **Review notes**: existing Rust dead-code warnings and Python SQLite `ResourceWarning`s remain non-blocking. The release gate's stale migration summary remains isolated as `P7-T04`, which is next before frontend integration in `P7-T03`.

## 2026-09-11 · Notion AI via Local · Fast-forward merge and P7 continuation plan

- **Merged**: fast-forwarded `autonomous-development-streaming` into `main` at `554fa6f` and pushed `origin/main`; no conflict resolution or history rewrite was needed.
- **Baseline**: the merged checkpoint had a clean tree and passed the full release gate before merge: 160 Rust tests, Clippy/build, 53 Python contracts, both local HTTP suites, frontend syntax, and `git diff --check`.
- **Next task**: `P7-T02c` is the next executable correctness task. It now explicitly depends on `P7-T02b` and names the storage, HTTP, native test, and integration-test files it owns.
- **Execution order**: finish attributed/resumable multi-turn generation replay (`P7-T02c`), correct migration reporting (`P7-T04`), then integrate the durable feed into the frontend (`P7-T03`). The frontend task depends on both backend replay and truthful migration-gate evidence.
- **Safety decision**: whole-answer buffering remains the active boundary. Incremental publication stays deferred until redaction can prove that secrets split across chunks never become visible early.

## 2026-09-11 · Notion AI via Local · P7-T02b idempotent generation recovery

- **Reviewed**: resumed the in-progress recovery fix on `autonomous-development-streaming`, inspected its existing diff, recovery ordering, recording SQL, generation projection, pending P7 tasks, and release gates without overwriting unrelated work.
- **Fixed**: `recover` now writes a generation `interrupted` event only for receipts that are still `generating`, before the receipt-state transition. Historical terminal receipts therefore cannot gain duplicate events or advance replay cursors on later startups.
- **Regression coverage**: the restart test captures the first generation feed, runs recovery again, proves byte-for-byte identical replay, and proves an unclaimed receipt in another session remains event-free and `captured`.
- **Verification**: `cargo test --locked restart` passed (2 tests), `cargo test --locked streaming` passed (9 tests), and `bash scripts/verify_release.sh` exited 0 (160 Rust tests, Clippy/build, migrations/contracts, 53 Python tests, both local HTTP suites, frontend syntax). `git diff --check` passed. Browser suites were not run because no UI changed.
- **Review notes**: the gate still reports three pre-existing Rust dead-code warnings and Python SQLite `ResourceWarning`s. These are non-blocking cleanup candidates. P7-T02c remains the next focused correctness task; P7-T04 already tracks the stale migration summary.

## 2026-09-10 · Codex via @local · P7-T02a provider boundary repair

- **Located/resumed**: `/root/development/harness`; clean inherited `main` at `227bd7e`; work is on `autonomous-development-streaming`.
- **Root causes reproduced**: role-only deltas stopped consumption; CRLF/multiline data disappeared; arbitrary network chunks used lossy UTF-8; incomplete/error streams succeeded; split secrets reached the sink; the text fallback wrote its answer twice; completion events were outside the receipt transaction.
- **Changes**: bounded byte-level SSE framing, strict UTF-8, content/body limits, role/comment/usage handling, explicit DONE and HTTP/error checks. Answers are buffered and redacted as a whole before sink delivery. Removed the unbounded asynchronous writer; one answer chunk and completed event now commit with the answer receipt. Restored the missing test attribute on the provider tool-choice contract.
- **Decision**: whole-answer buffering is the smallest reversible safety repair. This does not deliver incremental display. P7-T01/P7-T02 remain open rather than inheriting an unsupported completion claim.
- **Verification**: eight focused streaming tests and the fallback duplicate assertion pass. Final release gate exits 0: 160 Rust tests, Clippy/build, 53 Python tests, both HTTP integration suites, and frontend syntax. `git diff --check` passes. Existing warnings remain; browser suites were not run because no UI changed.
- **Environment failures resolved**: Cargo was installed outside PATH. Installed missing Clippy/rustfmt and Node.js to run the existing release gate; no live application or database was started or modified by this task.
- **Next**: generation transport/reconnect tests, request IDs in session generation rows, and restart-event deduplication; then safe incremental publication and P7-T03 frontend integration. Existing Python migration coverage stops at 004, while runtime migration 005 is exercised by the native DbStore tests.

## 2026-09-11 · AI session · P7-T02 runtime streaming integration

- Added text-only provider streaming path through `stream_turn` and a persistence forwarding sink.
- Tool-call turns continue using the existing completion loop.
- Verified: cargo test suite passes after runtime integration.
- Open: final release verification and commit.

## 2026-09-10 · AI session · P7-T02 stream boundary foundation

- **Delivered**: added provider SSE boundary parsing primitives for durable generation streaming.
- **Delivered**: added stream request builder contract and tests without changing the existing completion path.
- **Verified**: stream frame parsing now separates complete SSE events from partial network chunks.
- **Open**: wire the streaming HTTP response into `GenerationSink`, then persist incremental generation deltas.

Append-only. Newest entry first. Each entry: date, who (human / AI session), what changed,
## 2026-09-11 · AI session · P7-T02 provider stream response consumer

- Added the persistence bridge foundation for incremental generation streams.
- `GenerationEventWriter` now provides an async path from streaming code into `generation_events` using the existing `DbStore::append_generation` API.
- Next: wire stream deltas into this writer and add replay ordering tests.

## 2026-09-11 · AI session · P7-T02 async sink bridge

- Added a channel-based sink adapter between synchronous provider callbacks and async persistence code.
- Next: connect the channel consumer to ordered `generation_events` writes and terminal state handling.

- **Delivered**: Added the provider streaming response consumer boundary. `reqwest::Response` chunks are incrementally buffered, SSE events are extracted, provider deltas are decoded, and validated content is forwarded into `GenerationSink`.
- **Safety**: Invalid stream frames now fail through the sink instead of silently corrupting generation state.
- **Next**: Wire the streaming consumer into the generation persistence transaction so each provider delta becomes a durable `generation_events` record.
what was verified and how, what is open. Keep entries short; details go in TASKS.md status
and the design docs.

## How to resume (for an AI or a human)

1. Read `AGENTS.md`, then `docs/PLAN.md` (why), `docs/TASKS.md` (what, in order), this file (where we are).
2. Check the **Open questions** below. If any block the next task, ask the owner before coding.
3. Take the first `todo` task whose dependencies are `done`. Set it to `doing`. Read its `design:` section.
4. Implement. Run its `verify:` command. If you cannot run `cargo`, set `needs-verify` and say so in your entry.
5. Update TASKS.md status and append an entry here in the same change.

Suggested first message to an AI continuing this work:

> Read AGENTS.md, docs/PLAN.md, docs/TASKS.md and docs/PROGRESS.md in this repo. Tell me the current phase, the next task by ID, and whether anything in "Open questions" blocks it. Then implement that task following its design section and verify command. Do not skip the P0 gate result.

---

## 2026-09-10 · AI session (Notion AI via Local) · P6-T02 LSP diagnostics, references and rename

- **Delivered**: `lsp` is the twelfth registered tool. It selects `rust-analyzer` or `clangd` from the target extension, runs one fresh bounded stdio JSON-RPC session, converts 1-based Unicode positions to LSP UTF-16 positions, and returns capped diagnostics or reference locations with current whole-file hashes.
- **Approval-safe rename**: only `operation=rename` is side-effecting per call. The model supplies current hashes for every possible file; planning emits a capped combined multi-file diff while disk and `/changes` stay untouched, and the approved call starts a new server and rechecks all hashes before any write.
- **Bounded and recoverable**: 4 MiB frames, 2 MiB files, 20 files, 200 edits, 100 diagnostics/references, a 20-second default / 60-second maximum deadline, no model-controlled process command, no outside-root/non-file/new/resource paths, no overlapping edits, and rollback of earlier atomic writes if a later write fails. Each successful file becomes its own ordinary durable artifact.
- **Integration and guidance**: the schema generator, registry/order assertion, main-agent prompt and tool contract now include `lsp`. A deterministic fake stdio server drives the real HTTP fixture: read-only diagnostics/references bypass approval, and a reviewed two-file rename produces two applied/revertable change rows and two activity events.
- **Verification**: focused LSP suite 7/7, schema validation, Python compilation, direct HTTP integration, Clippy/build and `git diff --check` passed. Final release gate exited 0: 143 Rust tests, migrations 001→004, 12 tool schemas, 53 Python contracts and both mock-provider HTTP suites. Browser suites were not run because no UI changed.
- **Open**: the active Rust toolchain has only a `rust-analyzer` shim and reports the component missing, so live Rust calls currently return actionable `lsp_unavailable`; deterministic coverage requires no machine server and `/usr/bin/clangd` is available. Next is P6-T03 (`browser` via CDP).

## 2026-09-10 · AI session (Notion AI via Local) · P6-T01 ast_edit via ast-grep

- **Delivered**: `ast_edit` is the eleventh registered tool. It structurally rewrites every matching site in one existing Rust file with in-process ast-grep, then uses the same diff, atomic-write, diagnostics and durable change-artifact path as `edit`.
- **Approval-safe**: calls require the eight-hex `content_hash` from the latest `read`. Planning puts a bounded unified diff, before/after hashes and +/− counts in the pending permission while disk and the changes feed stay untouched; the approved run checks the hash again before writing, preventing approval-time diff drift.
- **Bounded refusals**: Rust only, 512 KiB per file, 20 matches by default and 200 maximum. Stale content, zero matches, over-broad patterns, unsupported files and invalid patterns all fail without writing.
- **Integration and guidance**: schema generation, the registry-order assertion, tool contract and main-agent prompt now include `ast_edit`. The HTTP fixture proves `read → pending approval → approved structural rewrite → applied/revertable file_changes` across two differently formatted matches.
- **Verification**: focused `ast_edit` suite 6/6, schema validation, Python compilation, direct HTTP integration and `git diff --check` passed. Final release gate exited 0: 136 Rust tests, Clippy/build, migrations 001→004, 11 tool schemas, 53 Python contracts and both mock-provider HTTP suites. Browser suites were not run because no UI changed.
- **Open**: nothing blocking. Next is P6-T02 (`lsp` diagnostics, references and rename).

## 2026-09-10 · AI session (Notion AI via Local) · P5-T03 task tool (read-only explore sub-agent)

- **Delivered**: The model can delegate exploration. `task` is registered like any other tool (tenth in the array), but the loop intercepts the call before `Registry::invoke`, because a sub-agent needs the provider while `Tool::run` is synchronous and filesystem-bound.
- **One request, one ordered list, still a tree**: the parent opens a `subagent` step whose `parent_step_id` is the `task` tool-call step, and the sub-agent's own model and tool calls hang off that. `STEP_BEGIN` carries the parent id at `?3` and `begin_child_step` is the only way to set it. No migration: schema 003 already had the column and the `subagent` kind.
- **Read-only by construction, not by permission**: only `read`, `grep` and `glob` are offered, the allow-list is re-checked when the model's call returns, and anything else is refused as `unknown_tool` without reaching the registry — so no approval can be raised inside a delegation even in `auto_all`. `TOOLS` excludes `task`, so a sub-agent cannot spawn one.
- **Bounded and shared**: a fresh context (own system prompt plus one exploration message, never the parent's history), ≤ 8 model calls, and the parent's remaining steps, tool bytes and wall deadline; what it spent is added back to the parent's counters. The parent sees a capped report (1000-char summary + ≤ 12 paths + why it stopped), never the sub-agent's transcript.
- **Deviations**: orchestration lives in `agent_loop` rather than `src/subagent.rs`, which is contract-only (tools, bounds, message and report shapes), because only the loop owns the provider, step writer and budgets; the planned `bounded_summary` helper collapsed into `report_content`.
- **Verification**: release gate exit 0 — 130 Rust tests (9 new), Clippy/build, migrations 001→004, 10 tool schemas, 53 Python contracts, both mock-provider HTTP suites. `recording_integration.py` gained a delegation leg asserting the step tree, the read-only tool array, the report shape and that a refused `write` left `deny.md` untouched. `git diff --check` clean. Browser suites not run: no UI change.
- **Open**: nothing blocking. Next is P6-T01 (`ast_edit` via ast-grep), which opens P6.

## 2026-09-10 · AI session (Notion AI via Local) · P5-T02 skills with progressive disclosure

- **Delivered**: A project can keep reusable procedures in `skills/<name>/SKILL.md`. Each turn's window lists only name, one-line description and path; the model calls the new `skill` tool to pull one body on demand, capped at 16 KiB and cut on a UTF-8 boundary after frontmatter is stripped.
- **Bounded and sandboxed**: 32 skills, 200-char redacted descriptions, 64-char single-segment names, 256 KiB per file, real directories only (no symlink traversal), with `paths::resolve` still gating the read. Skipped and over-cap directories are counted in the index instead of vanishing; a failed scan degrades to one `skills:not_indexed` line.
- **Untrusted by construction**: a body arrives as a tool result behind a banner saying project text cannot grant tool permissions, approve a denied command or override system rules.
- **Deviation from the task wording**: discovery runs per turn before the first provider call, not at process startup, because a scope's root path is configurable at runtime.
- **Two magic numbers removed**: `src/agent_loop.rs` and `tests/recording_integration.py` both asserted exactly eight tools; they now count the registry's schemas and `tools/schemas/*.json`.
- **Verification**: release gate exit 0 — 121 Rust tests (9 new), Clippy/build, migrations 001→004, 9 tool schemas, 52 Python contracts, both mock-provider HTTP suites. `recording_integration.py` gained a `skills/review` fixture proving the index reaches the window and the body does not. `git diff --check` clean. Browser suites not run: no UI change.
- **Open**: this repo ships no `skills/` directory, so the feature stays inert here until one exists. Next is P5-T03 (read-only explore sub-agent).

## 2026-09-10 · AI session (Notion AI via Local) · P5-T01 verifier step

- **Delivered**: Every text answer is now audited by a separate `verification` step that runs after the main model call and before `answer_saved`. The verifier sees the redacted answer and an evidence manifest of this request's durable tool steps only, and returns claims marked verified / unverified / skipped with the step ids they rest on.
- **Advisory by construction**: the answer is written first and never rewritten. Strict JSON parsing rejects unknown fields, oversized reports and any "verified" claim citing a step outside the turn; a provider or parse failure records `verification_failed` / `unavailable` on the verification step alone. Input and projection are bounded (24 steps, 12,000 answer chars, 20 claims, 8 evidence ids, 10 diagnostics, 500 chars per projected field).
- **UI**: a header badge derived only from the persisted step shows Verified, "N unverified", skipped or unavailable, with claim reasons in its tooltip; claim text is inserted as text, never markup. Settings gained a `verification` model role that falls back to the turn model and is cleared by lock.
- **Fixture correction**: `tests/mock_provider.py` and `tests/integration_smoke.py` assumed the last provider call of a turn was the answer, which the audit call broke. Both now route marker-carrying calls to a verification reply; the release gate caught this, not the browser suites.
- **Verification**: release gate passed (exit 0): 113 Rust tests, Clippy/build, migrations 001→004, 8 tool schemas, 52 Python contracts, and both mock-provider HTTP suites including new verification step, projection and event-order assertions. Browser suites run separately with a local Playwright and Chrome: 24 ambient-UI checks (3 new) and 15 recording checks. `git diff --check` clean.
- **Open**: nothing blocking; next is P5-T02 (skills with progressive disclosure). Note that `scripts/verify_release.sh` still does not run the browser suites, so they must be run by hand.

## 2026-09-10 · AI session (Notion AI via Local) · P3-T02–P4-T04 context and ambient memory

- **Delivered**: Tool-result compaction with a stable three-call boundary, unchanged-read references with full durable audit output, 70%-token turn compaction with exact receipts and review-only episodic candidates, and deterministic per-scope repository maps capped at 8 KiB.
- **Memory**: Added schema 004 with three new memory kinds and embedding storage. Hybrid recall now unions FTS5 and bundled deterministic offline vectors, preserves project shadowing, uses recency/usefulness in reranking, and retains the 6,000-byte output ceiling. Extraction separates untrusted plan context from exact user evidence and gives explicit corrections high priority.
- **Ambient UI**: Chat suggestions now appear beneath their source turn with Save/Edit/Dismiss; the Inbox is import-only. Candidate edits preserve evidence and expected revision, hostile text remains inert, and the real HTTP smoke suite covers filtering and editing.
- **Verification**: Release gate passed: 107 Rust tests, Clippy/build, migration 001→004, 8 tool schemas, 51 Python contracts, and both mock-provider HTTP suites. Both browser suites passed (21 ambient-UI checks and 15 recording checks); JavaScript syntax and `git diff --check` passed.
- **Delivery**: implementation commit `4fbc037` (`P3/P4: ship context management and ambient memory`).

## 2026-09-10 · AI session (Notion AI via Local) · P3-T01 deterministic context manager

**Why.** The first provider window had one coarse 24 KB history trim, embedded memory and plan text inside the prompt, and no receipt explaining what fit. Tool schemas were regenerated later by the loop, so the immutable receipt could not prove the definitions on the first call.

**Changed.**
- `docs/design/context.md`, `src/context.rs` — defined and implemented nine stable categories with independent source-byte budgets totalling 95,872 bytes. Every successful build emits a fixed-order ledger with budget/candidate/included/excluded byte counts and explicit part IDs; no category borrows from another. Required system rules/current message/registry fail closed, optional structured parts stay whole, and recent chat history keeps the newest complete turn suffix. UTF-8 and overflow behavior are unit-tested.
- `src/recording.rs`, `src/agent_loop.rs`, `prompts/main_agent.md` — both project and chat-only turns now use the same builder. The write-once format-v2 receipt stores exact first-call `provider_messages` and `provider_tools`, only memories that actually reached the window, and the category receipt before any provider call. The loop receives that exact tool array instead of regenerating it. Synthetic skills/map/memory/plan/summary material is one explicitly untrusted reference message; the current user message remains last and exact.
- `tests/recording_integration.py`, `docs/RECORDING_PROTOCOL.md` — the real HTTP gate proves stored messages and full tool definitions equal the first provider request and validates all nine ledgers. The old claim-time 24 KB trim is gone so exclusions inside the existing 20-message source bound are auditable.

**Verified.** `cargo test --locked context` → 7 passed; `cargo test --locked` → 93 passed; `python3 -m py_compile tests/recording_integration.py` → OK; `git diff --check` → clean; `bash scripts/verify_release.sh` → exit 0 (93 Rust tests, Clippy/release build, migrations 001→003, 8 tool schemas, 51 Python contracts, both local mock-provider HTTP suites). Log: `/tmp/verify-p3t01.log`. Browser suites were not rerun because no UI asset or browser behavior changed.

**Open / next.** The typed `skills_index`, `repo_map`, and `compacted_history` sources remain empty until P5-T02, P3-T04, and P3-T03. Current-turn tool bodies are still appended verbatim after the immutable first window; P3-T02 is next and owns old-result compaction plus the unchanged-file read cache.

## 2026-09-10 · AI session (Notion AI via Local) · P2-T03 create-revert coverage follow-up

**Why.** A post-ship audit found that the modify/revert path was proved end to end, but undoing a file the turn created had never crossed the real HTTP handler, and the browser mock never returned its distinct `status="deleted"` result. That was a small but real first-use risk.

**Changed.**
- `src/main.rs` — extended the existing revert integration test with an `action="create"`, `before_hash=NULL` change. It proves the card is initially revertable, the handler returns `status="deleted"`, the file is absent rather than zero-byte, a second request is 409, exactly one `file_reverted` event exists for that change, and the relisted card says Already reverted.
- `tests/ui_smoke.cjs` — added a second create card and mock result so the browser clicks both kinds of Revert, proves the create-specific "file this turn created was removed" notice, and stops offering the action afterward. The suite now reports 17 checks.
- `docs/TASKS.md` and the handler comment — clarified that `action="delete"` restoration is forward-compatible only. No current tool emits delete rows, so that branch is not described as a shipped or end-to-end-covered user path. The browser API remains mocked; the Rust test is what proves server behavior.

**Verified.** `cargo test --locked changes` → 5 passed; `node --check static/app.js` → OK; ui_smoke → 17/17 including `diff_card_create_revert`; `git diff --check` → clean; `bash scripts/verify_release.sh` → exit 0 (89 Rust tests, Clippy/build, migrations 001→003, 8 schemas, 51 Python contracts, both HTTP suites). Final gate log: `/tmp/verify-p2t03-followup-final.log`.

**Open.** The filesystem restore and `reverted_at` bookkeeping still cannot be one atomic operation; a database failure after the file write can leave a stale card. `/changes` also re-hashes each changed file on rail refresh. Both are deferred follow-ups rather than hidden inside this coverage-only correction.

## 2026-09-09 · AI session (Notion AI via Local) · P2-T03 diff cards with undo

**Why.** The rail could say a file changed but not show what changed, and there was no way back. `file_changes` has carried `before_hash`, `after_hash` and the unified diff since P1-T14, so the data for both a card and an undo was already recorded — only the read side and one endpoint were missing.

**Changed.**
- `src/tools/textdiff.rs` — `reverse(after, diff)` rebuilds pre-edit text by reverse-applying a recorded diff. It refuses a diff carrying the `[diff truncated]` marker and verifies every hunk's after-side against the file it was handed, so a stale diff cannot invent content.
- `src/agentic_sql.rs`, `src/storage.rs` — `FILE_CHANGE_GET` reads one change joined to its turn's scope and session, so a revert cannot be aimed at another project. `record_revert` sets `reverted_at` only `WHERE reverted_at IS NULL` (single-shot) and appends a `file_reverted` activity event in the same immediate transaction. `activity_events.kind` is open-vocabulary, so no migration was needed.
- `src/main.rs` — `GET /changes` enriches each row with `revertable` and a `revert_note` by hashing the file on disk in `spawn_blocking`. `POST /changes/{id}/revert` restores the previous content behind two proofs — the file must still hash to `after_hash`, and the rebuilt text must hash to `before_hash` — writing through the existing `edit_tools::atomic_write` (now `pub(crate)`). Refusals are 409 with the reason and touch nothing: already reverted, file changed since, no project root, no recorded previous content. A revert of a file the turn created deletes it again.
- `static/*` — the rail grew a "File changes" panel below the steps: `path · +A −B · applied HH:MM`, the diff coloured per line by CSS class on spans built from `textContent` (CSP is `style-src 'self'`, and this also keeps a hostile diff inert), then Revert or the server's muted note. A revert refreshes the turn, so the card flips to "Already reverted" from the server's own state rather than a local guess.
- `tests/*` — ui_smoke renders a card from an XSS-laden diff, asserts the markup stays text, then clicks Revert and waits for the footer to change; the SQL contracts cover the revert roundtrip and its single-shot update.

**Scope call.** The ticket said "accept/reject". A row only exists after `agent_loop::finish_step` wrote it with `applied=1`, so there is nothing left to accept; the card offers Revert alone instead of a button pretending to gate an edit that already landed.

**Verified.** `cargo test --locked` 89 passed (4 new); `node --check static/app.js`; ui_smoke 16/16 including `diff_card_rendered` and `diff_card_revert`; recording_ui 15/15; `tests/test_agentic_sql.py` 8/8; `bash scripts/verify_release.sh` exit 0 (clippy, build, migrations, 51 Python contracts, both HTTP suites). One self-inflicted failure on the way: a first unit test asserted `reverse` would refuse a file with a foreign line appended after the hunk. It cannot — no diff describes lines it never touched — so the test now asserts the honest property and documents that the endpoint's two hash checks are what catch that case.

**Open.** Revert has no keyboard shortcut and cards are not grouped by step. Carried over: the composer still does not auto-grow (CSP blocks the inline style it would need) and assistant messages render as plain text pending a sanitizing markdown renderer.

## 2026-09-09 · AI session (Notion AI via Local) · P2-T01 SSE endpoint over activity_events

**Why.** The rail was accurate but late. P1-T13's 1 s poll re-fetched the whole turn on a timer, so a step appeared up to a second after it committed and every open tab paid a full `/activity` read per second. P2-T02 deliberately left the transport alone so this task only had to swap it.

**Changed.**
- `src/main.rs` — `GET /activity/stream?session_id=&after_seq=N`, inside the existing auth and Origin layer. A spawned task polls `agentic_sql::EVENTS_AFTER` every 200 ms (batch 200, matching the statement's own LIMIT) and pushes `id: <seq>` / `event: <kind>` / `data: <row>` frames into a 64-frame channel drained by `Frames`, a small `futures_core::Stream` handed to `Body::from_stream`; a `: heartbeat` comment goes out after 15 s of quiet, and the task gives up after 25 consecutive read failures. Bounds match `/activity`: 400 on a non-UUID `session_id` or a negative cursor, 401 without the bearer token.
- `static/app.js` — `followActivityStream(sessionId)` reads that stream with `fetch` + `TextDecoder` and an `AbortController`, subscribed per turn from `followReceipt` and closed on a terminal state. Header auth keeps the token out of the URL, which is the whole reason this is not `EventSource`. Frames advance a cursor and mark the rail live; refreshes are debounced 150 ms; a hidden tab marks itself stale instead of rendering; reconnects back off 1 s → 5 s and after 3 failures fall back to the old poll, which stays as the safety net.
- `Cargo.toml` — `futures-core` for the stream impl, tokio's `test-util` dev feature for virtual time. `README.md` documents the endpoint and why callers must use `fetch`, not `EventSource`.
- `tests/recording_integration.py` — a `stream a turn` script, an SSE reader, and the gate's new assertions: frames from a stream opened before the turn equal the `/activity` rows for that session (seq, kind and payload), resuming at `next_after_seq` replays nothing, `after_seq=-1` and a bad `session_id` are 400, no token is 401.
- `tests/recording_ui.cjs` — a mock `/activity/stream` route returning a finite SSE body plus a heartbeat, and a new `activity_stream_subscribed` check asserting the rail actually subscribes.

**Verified.** `cargo test --locked` → 85 passed (83 + `activity_stream_frames_recorded_rows_and_resumes_from_the_cursor` and `activity_stream_heartbeats_a_quiet_session`). `python3 tests/recording_integration.py` → PASS. `bash scripts/verify_release.sh` → exit 0 (clippy, build, migrations 001→003, 8 schemas, 50 Python contracts, both HTTP suites). `node tests/ui_smoke.cjs` → 14/14, `node tests/recording_ui.cjs` → 15/15, `node --check static/app.js` → OK. Browser suites again needed playwright-core in /tmp/harness-qa symlinked as `playwright` plus `CHROMIUM_PATH`.

**Continuity note.** The code landed in a session that was cut before any gate ran; this session re-verified the working tree unchanged, then did the bookkeeping and the commit. Nothing was marked done on trust.

**Gotchas.** (1) A missing `session_id` is an axum `Query` rejection with a plain-text body, so that status is asserted in the Rust test — the Python helper parses JSON. (2) Never hand a streaming path to the suite's `call` helper; it reads to EOF and hangs. (3) The 15 s heartbeat is only testable under `#[tokio::test(start_paused = true)]`; real time would add 15 s to the suite. (4) Exactly-once lives in the cursor: it advances only past frames already queued, so a mid-turn disconnect neither repeats nor drops a row.

**Open / deferred.** Frames still come from a 200 ms DB poll rather than a write notification — fine for a single-user SQLite install, revisit if the loop ever grows a broadcast channel. The two clippy dead-code warnings (`ToolCtx.request_id`, `Tool::plan`) stay parked on P2-T03. Assistant markdown rendering and composer auto-grow remain open P2 items.

**Next.** P2-T03 diff cards with accept/reject and undo (`POST /changes/{id}/revert`) — now the first `todo` whose dependencies are done.

## 2026-09-09 · AI session (Notion AI via Local) · P2-T02 full UI redesign

**Why.** The owner's verdict on the P1 minimal UI: too rigid, too bland, too much chrome on the chat. They asked for a full redesign before P2-T01 and picked the direction by survey: terminal-flat chat (no bubbles, like omp/claude-code), a collapsible right activity rail, dark-first theme with a light toggle, cozy 15px density. Task was reordered ahead of P2-T01 with the owner; the rail still uses the P1-T13 polling and T01 now only swaps the transport.

**Changed.**
- `static/index.html` — new shell: topbar (nav drawer button, brand, New chat, rail toggle, theme toggle, lock, connection badge) + left sidebar (scope picker with datalist, Conversations, nav: Chat/Inbox/Imports/Settings, stats footer) + centered chat column + right `#rail` hosting `#agent-turn` (plan, steps, context placeholder). Auth is a centered card. The permission card moved out of the agent panel to sit sticky above the composer (ui.md: "impossible to miss"). Every element id referenced by app.js and the browser suites is preserved (checked programmatically, 69/69); the stylesheet link tag stays byte-identical for recording_ui snapshots.
- `static/style.css` — full rewrite. Palette/typography ported from `reference/renewed-ui-original` into `:root` (dark default) and `:root[data-theme="light"]`; mono accents for roles/tools/timestamps. `.message` is now a flat grid row (label gutter + hairline separator), `#notice` is a floating toast (pointer-events:none so it never blocks clicks), sidebar drawer ≤920px, rail overlay ≤1100px.
- `static/app.js` — additive shell block only: theme init/toggle (localStorage `harness_theme`; not sensitive — tokens and drafts still never persist), drawer + rail toggles, Enter-to-send with Shift+Enter newline (ui.md#keyboard). Behavior edits: session list only auto-closes in drawer mode; rail auto-opens on wide screens while a turn is active and closes when it ends; chat scroll now targets the new `#chatscroll` container (loadHistory used to scroll `#log`, which no longer scrolls); a running step's meta shows live elapsed m:ss from `started_at`, re-rendered by the existing 1s poll; the rail context line shows real tokens-so-far summed from step receipts.
- `tests/ui_smoke.cjs`, `tests/recording_ui.cjs` — added the missing `/scopes` mock route. Pre-existing gap since P1-T15: `refreshScopeSetup()` 404'd in the mock and the auth handler re-hid the workspace, so both suites timed out at connect. The browser suites had never actually run; they pass now.

**Verified.** `node --check static/app.js` OK. `node tests/ui_smoke.cjs` → passed (14 checks). `node tests/recording_ui.cjs` → passed (14 checks). `bash scripts/verify_release.sh` → exit 0 (83 Rust tests, clippy, release build, migrations 001→003, 8 schemas, 50 Python contracts, both mock-provider HTTP suites). Browser suites ran with playwright-core in /tmp/harness-qa (symlinked as `playwright`) and `CHROMIUM_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`. Screenshots in docs/qa reviewed across desktop dark/light, 820px rail overlay and 390px mobile.

**Gotchas found.** (1) Playwright fullPage screenshots paint a transform-hidden fixed sidebar — the closed drawer is now `visibility:hidden` as well, which is also more robust for real renderers. (2) While QA-ing, a scope-mismatched mock made `loadHistory` refuse to render another scope's messages — the guard worked as designed; mock data just has to match the page scope. (3) CSP `style-src 'self'` forbids inline style attributes, so all presentation state moves via classes/hidden/data-theme; composer auto-grow skipped for that reason.

**Open / deferred.** Assistant markdown rendering (ui.md allows in P2 with a sanitizing renderer); context budget bar fills in P3; diff cards are P2-T03. QA screenshots under emulated light scheme show dark unless the toggle is clicked — tests assert overflow/behavior, not palette.

**Next.** P2-T01 SSE endpoint over `activity_events` — `refreshAgentTurn`'s 1s poll is the single call site to swap; then P2-T03 diff cards.

## 2026-09-09 · AI session (Notion AI via Local) · P1-T15 first-run project setup

**Why.** The owner ran the finished P1 harness for the first time and it looked broken: in the default `global` scope the agent answered "I don't have access to a terminal or file system tools in this conversation." Nothing was broken — `global` had no `root_path`, so the loop attached zero tools exactly as P1-T04 requires. The defect was that no layer said so. The workaround was a hand-written `curl POST /scopes/myharness`, which is not a product.

**Changed.**
- `prompts/main_agent.md` line 2 is now `{{tools}}`. `window()` fills it with `TOOLS_ATTACHED` (the old sentence) or `TOOLS_WITHHELD`, which tells the model to blame the missing project root, name `Project & models → Project scope settings` / `POST /scopes/{scope}`, and *not* to claim the conversation or provider lacks tool support.
- `agent_loop::run` records one `tools_withheld` activity event (`reason: no_root_path`) beside `turn_started` when the tool list is empty, so `/activity` and the UI carry the same fact as the answer.
- New `GET /scopes` (`SCOPES_LIST`, `DbStore::scopes`, `list_scopes`) lists every configured scope, a null `root_path` included — that scope exists, it just cannot run tools.
- UI: the scope field is backed by a `datalist` of real scopes (each labelled with its root path, or "no project root · tools off"), and the chat view shows a `#setup-banner` whenever the current scope has no root, with a button that jumps to the existing project form and focuses `Root path`. Refreshed on connect, on scope change, on New conversation, on reopening a conversation, and after saving project settings; cleared on Lock.

**Verified.** `bash scripts/verify_release.sh` → exit 0: 83 Rust tests (up from 81), clippy, release build, migrations, 8 tool schemas, 50 Python contracts, both mock-provider HTTP suites. New tests: `configured_scopes_are_listed_for_the_picker` (empty on a fresh install; ordered; a rootless scope is still listed) and `the_prompt_names_the_missing_project_root_instead_of_promising_tools` (withheld text names scope and fix; the configured prompt still says "You have tools." and never says "NO tools"). `SCOPES_LIST` gained a contract assertion in `tests/test_agentic_sql.py`. `node --check static/app.js` → OK.

**Not verified.** The banner and datalist are not exercised by the gate — `tests/ui_smoke.cjs` / `tests/recording_ui.cjs` need a running server and are still run by hand.

**Decisions.**
- The tool gate itself did not move. An unconfigured scope still gets no tools; only the explanation changed. Making `global` auto-adopt a working directory would have turned a safety property into a surprise.
- The banner reads the same `/scopes` data the picker uses, so it cannot disagree with what the loop will do; it does not ask the server "are tools on?" as a separate opinion.
- Still open: the two clippy warnings (`ToolCtx.request_id`, `Tool::plan`) remain parked on P2-T03, and `GET /scopes` has no pagination — a single-user install with hundreds of scopes is not a case worth code yet.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1 review pass before P2

**Changed.** No behaviour; this is the review of P1-T10…T14 read back as a whole.
- `request_permission` had grown to eight positional arguments, five of them `String`, so a caller could transpose `request`/`session`/`step` and still compile. It now takes a `NewPermission` struct, like `NewStep` and `StepOutcome` already do. This also retired the `clippy::too_many_arguments` warning it had earned.
- Rewrote the `grep` literal fallback's two context loops as slice iterations, retiring the `needless_range_loop` pair that had been carried since P1-T06, and dropped a `redundant_closure` in a P1-T11 test. Clippy is now down to the two deliberate dead-code warnings.
- **That refactor was untested**, because `rg` is installed here and the fallback never runs: the existing grep tests all pass `context: 0`. Added `grep_fallback_numbers_its_context_window`, which calls the fallback directly and pins the rendered window — match lines keep `:`, neighbours keep `-`, each with its own number — including the clamp at the first and last line. Hand-numbered context is exactly the arithmetic that rots silently.
- `ToolCtx.request_id` and `Tool::plan` now say in the source that they are waiting on **P2-T03** (accept/reject a diff card), not P1-T11. P1-T10 records changes as `applied=1` because the tool has already written the file, so the plan-then-apply split has no caller until a diff card can reject one.

**Verified.**
- `bash scripts/verify_release.sh` → exit 0, unchanged: 80 Rust tests, clippy, release build, 50 Python contracts, migrations, 8 schemas, both mock-provider HTTP suites.
- Read the T11–T14 diffs against the design doc. The permission gate is right where it counts: `resolve_permission` decides on the stored `status` and never on the clock, which is safe because something always flips a pending row — the loop at its own deadline, or `RECOVER_PERMISSIONS` at startup — and the "approved a moment before the deadline" race is handled by `expire_permission` returning the current status so a late `approved` is still honoured.
- The T13 UI never touches `innerHTML`; every model- and tool-authored string goes through `textContent` or `createElement`. No DOM-injection path from tool output or a diff preview.

**Open.**
- Unchanged caveats: `.harness/logs/` is never pruned; a background `pid` is the owning shell, not the job; `permission_payload` diffs cap at 64 KiB; the browser suites (`tests/ui_smoke.cjs`, `tests/recording_ui.cjs`) are still not run by the gate.
- The `rg`-vs-fallback caveat is now narrower, not gone: the fallback's own rendering is pinned, but nothing asserts the two engines agree on the same corpus, and the `.gitignore` behaviour only ripgrep provides is still untested.
- While a turn is pending the UI polls three endpoints every second and the API admits eight concurrent requests. It degrades quietly today (the interval swallows the error), and P2-T01 removes two of the three polls, but the SSE stream must not hold its semaphore permit for the life of the connection.

**Next.**
- P2-T01 resumable authenticated SSE over `activity_events`. `/activity` already returns a DB-sequence cursor that provably does not replay, which is the hard half.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T14 scripted tool-call HTTP coverage

**Changed.**
- Added `tests/mock_provider.py`, a reusable OpenAI-style loopback provider with per-prompt canned responses, tool-call construction, failures, and captured request bodies.
- Expanded `tests/recording_integration.py` to configure a temporary project scope and exercise the real HTTP agent loop: read → edit → bash → answer, assistant/tool message replay, applied file changes, stale anchors, sandbox path escape, permission denial, budget exhaustion, provider failure, and SIGKILL during a running bash step.
- Recovery assertions verify the interrupted tool is recorded as `interrupted`, the receipt is interrupted, and restart does not make another provider call or re-execute the tool.

**Verified.**
- `python3 -m py_compile tests/mock_provider.py tests/recording_integration.py` → pass.
- `python3 tests/recording_integration.py` → PASS.
- `bash scripts/verify_release.sh` → exit 0: 80 Rust tests, migrations, tool schemas, 50 Python contracts, and the real HTTP suite.

**Next.**
- P2-T01: authenticated resumable SSE over `activity_events`, replacing the UI's 1-second polling path.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T13 minimal UI

**Changed.**
- Restored the existing local-first conversation shell after the initial UI pass and added a turn record panel in `static/index.html`, `static/app.js`, and `static/style.css`.
- The panel polls the durable steps, plan, and pending-permission APIs while a request is active. Steps are native collapsible rows with input/output previews; approval cards show the tool-owned diff or command and send idempotent Approve/Deny decisions; plans render as a checklist with progress.
- Renamed the Models view to Project & models and added scope settings for root path, permission mode, diagnostics command, and budgets via `/scopes/{scope}`.
- Kept the existing security posture: relative API calls, bearer token only in tab memory, no inline styles, no `innerHTML`, and all untrusted text rendered through DOM nodes / `textContent`.

**Verified.**
- `node --check static/app.js` → pass.
- `git diff --check` and HTML-hook check → pass; all 64 direct JS element hooks resolve.
- `cargo test --locked` → **80** passed.
- Browser harness was attempted but is blocked in this checkout because the `playwright` Node module is not installed.

**Next.**
- P1-T14: extend the synthetic HTTP provider to emit scripted tool calls and cover the full read → edit → bash → answer path over HTTP.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T12 API for steps, plan, activity

**Changed.**
- Readers in `src/storage.rs`: `turn_steps`, `activity_since`, `turn_changes` (`plan` already existed), with thin handlers in `src/main.rs` under the existing auth and Origin middleware. They only read rows the loop already committed — nothing recomputes a summary or re-renders a diff — so the UI cannot show a version of a turn that the record disagrees with.
- `GET /chat/requests/{id}/steps` returns steps in `seq` order with 2 KB previews. Two decisions worth knowing: it **404s** on an unknown request, because an empty step list would otherwise read as "this turn did nothing"; and `previews_capped` is reported separately from `truncated`, because the preview hitting 2 KB and the tool's own output being capped are different facts.
- `summary` is the tool's own phrase, read back from the finished step's output. A step that is still running therefore has none — its `tool_started` activity event carries it, which is what the live UI polls anyway. No summary is ever recomputed from the model's text.
- `GET /activity?session_id=&after_seq=N` returns ≤ 200 events plus `next_after_seq`, which only advances when rows came back, so a poll that finds nothing cannot skip an event that commits a moment later. This feed is the agentic log only; `captured`/`generation_started` stay in `recording_events` behind `/chat/requests/{id}/context`.
- `GET /sessions/{id}/plan` treats an unknown session and a session without a plan identically (`{items:[]}`), matching `/sessions/{id}/messages`. The plan is a view of a session, not proof one exists.
- Added `GET /changes?request_id=` from the same design section, beyond the task's done-when: ten lines, it consumes the last dead SQL constant, and P1-T13 / P2-T03 both need it. `POST /changes/{id}/revert` stays P2-T03.

**Verified.**
- `python3 tests/recording_integration.py` → PASS, now including the read side over real HTTP: a completed turn's single `model_call` step (bounded previews, no `error_code`) and its `turn_started → model_call_started → model_call_finished → answer_saved` feed with cursor paging that neither replays nor skips; a provider failure reading `failed / provider_failed` and ending in `turn_failed`; a SIGKILL'd step reading `interrupted`, not `failed`; empty plan and empty changes for a turn that ran no tools; 401 / 403 / 404 on the new routes.
- `cargo test --locked` → **80** passed (new: steps/plan/activity/changes read back what `begin_step`/`finish_step` wrote, including the preview cap, the cursor and every bounds case). `bash scripts/verify_release.sh` → exit 0.

**Open.**
- The suite's provider is text-only, so no *tool* step, plan item or file change is asserted over HTTP yet — those paths are covered by `cargo test` against the scripted provider, and P1-T14 owns the scripted-tool-call HTTP suite.
- Every endpoint is polling. SSE is P2, and `/activity` has no per-request filter (session only), which is all the P1 UI needs.
- `ToolCtx.request_id` is now the only remaining deliberate dead-code warning (with `Tool::plan` / `PendingChange`, which wait on a dry-run tool contract). All 23 SQL constants are used.
- Unchanged: `.harness/logs/` is never pruned; a background `pid` is the owning shell, not the job; `permission_payload` diffs cap at 64 KiB; the browser suites were not run.

**Next.**
- P1-T13 minimal UI for steps and permissions — its dependencies (T04, T11, T12) are all done now, and it is the first task that makes any of this visible without curl.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T11 permission gate

**Changed.**
- `GET /permissions?scope=` lists the approvals still waiting, each with the tool's own `summary` and its `args_json` payload (the diff preview, the command) so a card can be rendered without asking the model to describe what it is about to do.
- `POST /permissions/{id}` body `{decision:"approve"|"deny", scope}`. `DbStore::resolve_permission` writes the decision, reads the session through `SESSION_OF_REQUEST` and appends `permission_resolved` in ONE Immediate transaction.
- Idempotency comes from the `status='pending'` guard already in `PERMISSION_RESOLVE`, not from a second read: the first decision wins, a replayed click answers `200 {"recorded":false}` and logs **no** second event, the opposite decision is `409` and changes nothing, a decision that arrives after the loop gave up is `410`, and a foreign scope is `404` rather than a hint that the row exists.
- The loop needed no new waiting logic. It already polls `status` every 500 ms, so committing the decision is what unblocks the turn — which is exactly the path T10 could not test.
- **The T10 open question is answered: the earlier deadline wins.** `permission_ttl()` writes `expires_at` as `min(30 min, remaining wall budget)`, and `request_permission` now takes that TTL from its caller. A default turn offers a 15-minute window and says so, instead of advertising 30 minutes it will not honour. Documented in `docs/design/agentic-turn.md#permissions`.
- `auto_edit` / `auto_all` needed no new code — `Registry::requires_permission` already held the matrix — but they are now covered end to end rather than by inspection.
- README's API sketch lists both endpoints; the deny test in `agent_loop` now denies through `resolve_permission` instead of raw SQL, so the test drives the same code the endpoint does.

**Verified.**
- `cargo test --locked permissions` → 8 passed: an approval lets a write reach the disk with exactly one `permission_resolved` event and an empty pending list afterwards; `auto_edit` applies a write with no approval row at all yet still stops `bash`; `auto_all` still asks before a deny-listed `git push --force` and reports the denial to the model; resolution is idempotent and refuses a flip; an expired row cannot be approved afterwards; the TTL clamp picks the earlier clock; the endpoints reject an unknown id (404), a bad decision word (400) and an unauthenticated caller (401).
- `bash scripts/verify_release.sh` → exit 0: **79** Rust tests (up from 71), clippy, release build, 50 Python contracts, migrations, 8 schemas, both mock-provider HTTP suites.

**Open.**
- No UI yet: approving still means calling the endpoint by hand. The Approve/Deny card is P1-T13, and a python HTTP approval round-trip belongs to P1-T14; `recording_integration.py` does not exercise these endpoints yet.
- A single turn's `expires_at` is fixed when the row is created; raising a scope's `max_wall_seconds` mid-wait does not extend a row that is already pending.
- Dead-code warnings left for P1-T12: `STEPS_LIST`, `EVENTS_AFTER`, `FILE_CHANGES_LIST`, `ToolCtx.request_id`. (`PERMISSION_GET`, `PERMISSION_RESOLVE` and `PERMISSIONS_PENDING` are now used.)
- Unchanged: `.harness/logs/` is never pruned; a background `pid` is the owning shell, not the job; `permission_payload` diffs cap at 64 KiB; the browser suites were not run.

**Next.**
- P1-T12 API for steps, plan and activity — its only dependency (T10) is done, and it consumes `STEPS_LIST` and `EVENTS_AFTER`.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T10 agent loop

**Changed.**
- `src/agent_loop.rs` (new) is the whole turn: `window()` renders `prompts/main_agent.md` with the scope root, recall and plan as the system message, and `run()` drives model call → tool calls → model call until the provider returns text with no tool calls.
- The storage half lives in the same file, next to its only caller. `begin_step` (next sequence, step row, activity event) and `finish_step` (result, event, artifacts) each commit in ONE Immediate transaction, so a step is never readable without the event that announces it.
- `Artifact::FileChange` writes `file_changes` with **`applied=1`**: the tool has already written the file by the time the loop sees the artifact. The design doc's plan → approve → write ordering needs a dry-run tool contract, so `PendingChange` and `Tool::plan` stay unused until P1-T11 — they warn as dead code today, deliberately.
- `Artifact::Plan` now goes through a new `storage::write_plan`; `DbStore::replace_plan` is gone. The plan lands in the same transaction as the step that produced it instead of a second one that could fail on its own.
- Budget is checked before every provider call, and the answer an exhausted turn returns says in as many words that the task is **not** finished — an out-of-budget stop must not read as success.
- `context_json` keeps the first window only, as the contract says; each `model_call` step then stores its own message array plus the tool **names**. Storing the schemas per step would have multiplied ~8 KB of JSON by up to 40 steps.
- Tool arguments that are not a JSON object fail the step as `invalid_arguments` before any tool runs. A provider that rejects the `tools` field once is retried without tools (`tools_unsupported`); a provider that fails otherwise fails the turn rather than inventing an answer.
- Text-only turns (no scope root) run the same loop with an empty tools array, so `memory_agents::chat_prepared` was deleted rather than left as a second code path.
- `recording.rs`: `recover()` also marks running steps `interrupted`, denies orphaned permissions and writes the activity rows; `complete_recording` writes `answer_saved`; `fail_recording` writes `turn_failed {error_code}`. All 13 activity kinds in the events contract are now emitted by something.
- The permission gate is built as far as this task reaches: create the `pending` row plus `permission_requested`, poll every 500 ms, expire on timeout, and hand the denial to the model as a tool error it can adapt to. P1-T11 owns the endpoints and the approval path.

**Verified.**
- `cargo test --locked agent_loop` → 7 passed, against an in-process scripted provider on loopback and the real `recording::generate`: a tool turn records every step and change before answering, an exhausted budget answers without claiming success, unreadable arguments never reach a tool, a denied tool changes nothing and is reported to the model, a provider without tool support falls back to text, a provider failure fails the turn, and a restart interrupts running work and reruns nothing.
- `bash scripts/verify_release.sh` → exit 0: 71 Rust tests, clippy, release build, 50 Python contracts, migrations, 8 tool schemas, and both mock-provider HTTP suites — including `recording_integration.py`'s SIGKILL-and-restart check that no tool is replayed.

**Open.**
- In `ask` mode nothing can approve a request until P1-T11's endpoints exist, so a side-effecting call waits out the turn's wall budget (15 min) and is then denied. The row's own TTL is 30 min and therefore unreachable from the loop; T11 should decide which limit wins.
- Dead-code warnings that P1-T11/T12 will consume: `STEPS_LIST`, `EVENTS_AFTER`, `PERMISSION_GET`, `PERMISSION_RESOLVE`, `PERMISSIONS_PENDING`, `FILE_CHANGES_LIST`, `ToolCtx.request_id`, `Tool::plan`.
- `.harness/logs/` still grows without pruning; the background `pid` is the owning shell, not the job; `permission_payload` diffs are capped at 64 KiB; the browser suites (`tests/ui_smoke.cjs`, `tests/recording_ui.cjs`) were not run.

**Next.**
- P1-T11 permission gate — its only dependency (T10) is now done.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T09 think / todo_write

**Changed.**
- Implemented P1-T09 as one file `src/tools/meta_tools.rs` (the name the `mod.rs` stub reserved) instead of `think.rs` + `todo.rs`. All eight tools of P1 are now registered.
- Neither tool is `side_effecting`, so a scratchpad note and a plan update can never sit waiting for an approval, in any permission mode.
- `think` returns the trimmed text as the step **output** — the collapsed reasoning card the UI will render — and puts the "noted" acknowledgement in the `summary`, because one result field cannot be both. Over 4000 characters is `too_large`; blank is `invalid_arguments`.
- `todo_write` normalizes (trim, absent `status` → `pending`, `seq` = position) and enforces ≤ 30 items, ≤ 200 characters and one `in_progress` as `invalid_arguments`, so the model can repair its own plan instead of hitting a CHECK. The result goes to the loop as `Artifact::Plan`; `DbStore::replace_plan` does the writing, so tools still never touch the database.
- `src/storage.rs` gains `replace_plan` (one Immediate transaction: `PLAN_CLEAR`, `PLAN_INSERT` per item, read back through `PLAN_LIST`) and `plan` for the `plan_updated` event and the P1-T13 panel. It re-checks the same limits, because a CHECK failure would otherwise reach the caller as an opaque 500. `MAX_PLAN_ITEMS` / `MAX_PLAN_TEXT` / `PLAN_STATUSES` live there as the single source of truth and the tool reads them.
- `truncate_chars` moved into `src/tools/mod.rs` so `bash` and `todo_write` share one label shortener.
- The old `verify:` line named `tools::todo`, which matches no test path. It is now `cargo test --locked plan`, which covers both halves of the task — the tool tests and the storage test — in one command.

**Verified.**
- `cargo test --locked plan` → 6 passed (think's output and refusals, todo_write normalization and artifact, seven rejected plan shapes, whole-plan replacement with rollback, plus the pre-existing edit_tools plan test the filter also catches).
- A new registry test asserts all eight schema files parse and their `function.name`s match the registered tools in order — this also retired the `schema`/`schemas` dead-code warnings.
- `bash scripts/verify_release.sh` → passed: 64 Rust tests, Clippy, release build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- `think` echoes the thought back to the model, which costs tokens on every turn it is used. Worth revisiting if P3 compaction shows it dominating.
- Nothing calls `replace_plan` or `plan` outside tests until P1-T10 consumes `Artifact::Plan` and emits `plan_updated`.
- The Local Ops bridge dropped mid-task; the code was already on disk and was verified once the connection returned.

**Next.**
- P1-T10: the agent loop in the generation worker — all its dependencies (T03, T05, T07, T08, T09) are now `done`. It must call `plan` → record `file_changes(applied=0)` → approve → `run` → flip to `applied=1`, and persist `Artifact::Plan` through `replace_plan`.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T08 bash

**Changed.**
- Implemented P1-T08 as `src/tools/bash_tool.rs` — the name the `src/tools/mod.rs` stub already reserved, rather than the `src/tools/bash.rs` in TASKS.md. The `verify:` filter still matches, because `tools::bash` is a prefix of `tools::bash_tool::tests`.
- Foreground calls reuse `edit_tools::run_capped`, so the env whitelist (`PATH HOME LANG LC_ALL TERM` + `HARNESS_SCOPE`), the cwd and the temp-file output capture are shared with `diagnostics_cmd` and cannot drift apart.
- `run_capped` gained two things for this task. The child now runs in its own process group (`process_group(0)`) and a timeout kills the **group**, so a `make`-style process tree cannot outlive the step that started it. A `started` flag also separates "could not spawn" (`spawn_failed`) from "killed by a signal" (`signal`).
- A non-zero exit is deliberately not a failed step: status stays `complete`, the code goes into the new `ToolResult::exit_code` (the `tool_finished {exit_code?}` field the events contract already promised) and the output ends with `[exit N]`. A red test run is information the loop has to act on, not a harness error.
- A timeout **is** a failed step but keeps its partial output, via a new `ToolResult::failed` that leaves the content as raw output instead of the JSON envelope `err` produces.
- `timeout_seconds` over the 600 s maximum is clamped, with a note appended to the output; a fractional or `< 1` value is `invalid_arguments`. A missing `description` falls back to the first 60 chars of the command instead of failing the call.
- Background runs `sh -c '<cmd>' >>log 2>&1 &` and reports `$!`, not `setsid`: macOS does not ship `setsid`, and the util-linux builds that fork print the wrapper's pid rather than the job's. The outer shell exits immediately, so the job is reparented to init, and the new process group already puts it out of reach of signals aimed at the harness. Logs go to `<root>/.harness/logs/<step_id>.log`, with the step id reduced to a safe filename stem.
- The deny-list was already in `mod.rs` (`is_dangerous_command`, consulted by `Registry::requires_permission`). This task covers it with tests and surfaces it to the future UI as `dangerous` in `permission_payload`, alongside the command, cwd, timeout and background flag.

**Verified.**
- `cargo test --locked tools::bash` → 8 passed (cwd + stripped env, failing command, timeout with partial output, clamped timeout, seven bad argument shapes, background pid/log round trip, deny-list in every permission mode, summary and payload).
- `bash scripts/verify_release.sh` → passed: 59 Rust tests, Clippy, release build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- The reported pid is the shell that owns the job, so `kill <pid>` stops a simple command but not every child of a compound one. The returned note tells the model how to tail and stop the job.
- Nothing prunes `.harness/logs/`; a long-lived scope will accumulate one log per background step.
- `exit_code` is plumbed but unread until P1-T10 emits `tool_finished`.

**Next.**
- P1-T09 (`think` / `todo_write` over the existing `plan_items` SQL), then P1-T10 wires the provider adapter, the registry, the permission gate and `file_changes` into the generation worker.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T07 edit / write

**Changed.**
- Implemented P1-T07 as one new file `src/tools/edit_tools.rs` (`Edit` + `Write` + shared plan/apply/diagnostics helpers), matching the `fs_tools.rs` precedent. `docs/TASKS.md` named `src/tools/edit.rs` and `src/tools/write.rs`; the `verify:` line named `tools::write`, which matches no test path, so it was corrected.
- The prepare/record/apply requirement became a default trait method in `src/tools/mod.rs`: `Tool::plan(ctx, args) -> Option<Result<PendingChange, ToolResult>>`, plus a `PendingChange` struct. `plan` never writes, so P1-T10 can record `file_changes(applied=0)`, render the diff for approval, then call `run` and flip the row to `applied=1`. Non-editing tools inherit `None` and are unaffected.
- `run` re-plans before applying rather than trusting the plan it was approved on: the file can change while a permission request is pending, and the hash anchors must still hold at write time.
- `edit` anchors on the `content_hash` and per-line hashes that `read` emits. A drifted anchor fails with `stale_anchor` and three lines of current context instead of overwriting; `old_string` must match exactly once (`no_match` / `ambiguous_match` with the offending line numbers). `write` refuses an existing file unless `overwrite:true`, caps input at 512 KiB, and rejects directories.
- Writes are atomic: `.<name>.harness-tmp-<step>` in the destination directory, then rename, preserving the existing file mode; the temp file is removed on failure. An identical rewrite reports "already had this content" and records no `file_changes` row, so a model re-issuing the same edit does not queue a redundant approval.
- `permission_payload` carries `{path, action, plus, minus, before_hash, after_hash, diff}` (diff capped at 64 KiB) for the P1-T13 UI. `summary` stays `edit <path>` because the trait gives it no filesystem access, so the ± counts live in the payload.
- Diagnostics go through a shared `run_capped` helper (`sh -c`, cwd = root, env stripped to `PATH HOME LANG LC_ALL TERM` + `HARNESS_SCOPE`, 60 s cap, 4 KiB of output kept, output interleaved through a temp file so a full pipe cannot deadlock the timeout poll). It is `pub(crate)` because P1-T08 reuses it for `bash`.

**Verified.**
- `cargo test --locked tools::edit_tools` → 7 passed (stale anchor, ambiguous and missing `old_string`, first write, clobber refusal, no-op rewrite, atomic replace with mode preserved).
- `cargo clippy --locked --all-targets` → exit 0; only the pre-existing `needless_range_loop` pair in the `grep` fallback plus the dead-code cascade below.
- `bash scripts/verify_release.sh` → passed: 51 Rust tests, Clippy, release build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- The diff in `permission_payload` is truncated at 64 KiB; a larger change is still applied in full but reviewed partially. P1-T13 should say so in the UI.
- `PendingChange`, `Registry::invoke` and the new caps report dead-code warnings until P1-T10 consumes them.
- Nothing has been committed yet this session.

**Next.**
- P1-T08 (`bash`: `sh -c` at the scope root, env whitelist, 120 s default / 600 s max, `background:true` via `setsid` logging to `<root>/.harness/logs/<step_id>.log`, deny-list that always prompts even in `auto_all`), then P1-T09 (`think` / `todo_write`), then P1-T10 wires the loop.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T04 scopes + P1-T05/T06 verified

**Changed.**
- Implemented P1-T04. `src/storage.rs` gains `ScopeConfig` (with `mode()`, `budgets()` defaulting to 40 / 400000 / 900, and `tool_ctx()`), a `ScopePatch` request type, `canonical_root()`, and `DbStore::scope_config` / `upsert_scope` on the existing `SCOPE_GET`/`SCOPE_UPSERT` constants. `src/main.rs` gains `GET`/`POST /scopes/{scope}` behind the existing auth + origin middleware.
- `POST` merges into the stored row: an absent field keeps its value, an explicit `null` clears it. This is why the request type uses `Option<Option<T>>` — a partial POST must not be able to silently drop `root_path` and disable tools.
- Validation runs before SQLite so a bad request is a 400 instead of a CHECK failure: `root_path` must be absolute, is canonicalized (symlinks resolved), must be an existing directory, must not be the filesystem root, and must not sit inside the harness data dir; `permission_mode` goes through `PermissionMode::parse`; `diagnostics_cmd` is trimmed, capped at 512 chars, and refused when it matches the bash deny-list; the three budgets are range-checked to mirror the 003 CHECKs.
- Tool gate: `Registry::invoke(ctx, name, args)` in `src/tools/mod.rs` is now the loop's single entry point. `ctx` is `None` for a scope without `root_path`, and every call then fails with `tools_disabled` instead of guessing a working directory. `paths::harness_data_dir()` became `pub` so P1-T04 can reuse it.
- P1-T05 and P1-T06 moved from `needs-verify` to `done`: both compiled unchanged in this checkout, and `src/tools/fs_tools.rs` gained end-to-end tests for `read`, `grep` and `glob` over a temp project (numbered lines and `content_hash`, `offset`/`limit` windowing, `.env` refusal, directory and missing-argument errors, match counts, glob filtering, `..` rejection).
- P1-T06's `verify:` line named three module paths (`tools::read`, `tools::grep`, `tools::glob`) that never existed — all three tools share `tools::fs_tools`. Corrected the command rather than splitting the file; the existing note already documented the one-file decision.

**Verified.**
- `cargo test --locked scopes` → 6 passed (storage merge/gate, validation rejections, HTTP round trip, HTTP 400).
- `cargo test --locked tools::paths` → 4 passed. `cargo test --locked tools::fs_tools` → 4 passed. `cargo test --locked tools::` → 10 passed.
- `bash scripts/verify_release.sh` → passed: 41 Rust tests, Clippy, build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- `rg` is installed here, so `grep`'s literal fallback branch (and the `.gitignore` behaviour that only ripgrep provides) is still untested.
- Nothing calls `Registry::invoke`, `ScopeConfig::budgets` or `tool_ctx` outside tests yet, so the crate still reports dead-code warnings; P1-T10 consumes them.
- No UI for scopes yet; that is P1-T13. Browser suites remain separate.

**Next.**
- P1-T07 (`edit`/`write` with hash anchors and `file_changes`), then P1-T08 (`bash`), P1-T09 (`think`/`todo_write`), then P1-T10 wires the provider adapter to the registry.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T03 provider adapter + P1-T01 verification

**Changed.**
- Implemented P1-T03 in `src/memory_agents.rs`: non-streaming OpenAI-style tools, `tool_calls`, usage, assistant-message replay, bounded provider errors, and safe malformed-argument parsing; the text-only extraction path remains intact.
- Marked P1-T01 done after verifying the migration chain and storage test in the owner's checkout.

**Verified.**
- `cargo test --locked provider` → 6 passed.
- `cargo test --locked` → 35 passed.
- `python3 tests/test_migrations.py && cargo test --locked storage` → passed.
- `bash scripts/verify_release.sh` → passed: native, SQL, and local mock-provider HTTP gates.

**Open.**
- Browser UI suites remain separate.
- The provider adapter is ready for P1-T10 to consume; malformed arguments still need to be turned into persisted `invalid_arguments` tool steps by the agent loop.

**Next.**
- P1-T04: scopes (`root_path`, `permission_mode`, `diagnostics_cmd`), then the remaining tool modules and agent loop.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P0 baseline verification

**Changed.**
- No application code changed.
- Updated `docs/TASKS.md`: P0-T01 is `done`; clarified that agentic recovery belongs to P1-T10, corrected the P1-T06 file path, added the prepare/record/apply requirement for file changes, and repaired later task dependencies.

**Verified.**
- `bash scripts/verify_release.sh` → passed in the owner's local checkout.
- Cargo/Rust 1.92.0: 29 Rust tests, Clippy, and release build passed.
- Python contracts and both local mock-provider HTTP suites passed.
- `node --check static/app.js` passed.

**Open.**
- Browser UI suites remain separate.
- P1-T01, P1-T05, and P1-T06 retain their existing `needs-verify` status until their task-specific verification and dependency sequencing is completed.

**Next.**
- P1-T03 is now the first dependency-satisfied implementation task: add the non-streaming provider adapter for OpenAI-style tool calls while preserving the extraction path.

---

## 2026-09-09 · AI session (Notion AI, sandbox without cargo) · Planning + P1 groundwork

**Context.** Owner asked for (1) tooling like oh-my-pi / OpenClaude, (2) a thin main agent backed by context/recall/advisor managers so coding does not hallucinate, (3) a chat UI that shows plan, running tools, logs and diffs, and (4) a written plan an AI can continue from.

**Changed.**
- New: `AGENTS.md` (entry point + operating rules), `docs/PLAN.md`, `docs/TASKS.md`, `docs/PROGRESS.md`, `docs/design/agentic-turn.md`, `docs/design/tools.md`, `docs/design/ui.md`.
- New: `migrations/003_agentic.sql` (scopes, turn_steps, activity_events, permission_requests, file_changes, plan_items; user_version=3).
- New: `tools/schemas/*.json` for read, grep, glob, edit, write, bash, think, todo_write, generated by `scripts/gen_tool_schemas.py`.
- New: `prompts/main_agent.md` (evidence rules, work loop, boundaries; `{{root_path}} {{scope}} {{recall}} {{plan}}` placeholders).
- New tests: `tests/test_migrations.py`, `tests/test_tool_schemas.py` (both unittest-discoverable, so `verify_release.sh` and CI already run them).
- Edited: `src/storage.rs` — accept `user_version` 1..=3 and apply 003 when `< 3` (two lines).
- Edited: `README.md` "Remaining work" and `docs/ROADMAP.md` header now point to PLAN/TASKS.

**Verified here.** `python3 -m unittest discover -s tests -p 'test_*.py'` → OK (16 SQL-contract tests + 2 new). `node --check static/app.js` → OK. Python's SQLite 3.40 has FTS5, so the migration chain 001→002→003 was applied for real, including CHECK/UNIQUE enforcement and the recovery UPDATEs.

**Not verified.** Anything Rust: the sandbox has no `cargo`/`rustc`. The `storage.rs` edit is trivial but unbuilt → P1-T01 is `needs-verify`. The repo's own ROADMAP says the *existing* code has never been compiled in this checkpoint either (P0-T01 remains the gate).

**Decisions made (and why).**
- Separate `activity_events` table instead of widening `recording_events.kind`: keeps the 002 CHECK and the existing receipt timeline stable; UI/SSE reads the new table.
- Per-step rows in `turn_steps` with `input_json` = the exact message array for each `model_call`: gives a full audit trail while keeping the write-once `context_json` semantics from 002.
- Hash-anchored `edit` (`line:hash`, 4-hex per line) borrowed from oh-my-pi; `old_string` allowed only within the anchored region. Rationale: rejects stale edits deterministically and gives the model fresh anchors in the result.
- Permission modes `ask | auto_edit | auto_all`, with a bash deny-list that always asks.
- Memory-category widening deferred to 004 because it needs a table rebuild (SQLite cannot alter a CHECK).

**Open questions for the owner.**
1. Does the target provider (`LongCat-2.0` at `HARNESS_BASE_URL`) support OpenAI `tools`/`tool_calls`? If not, P1-T03 needs a fallback model or a prompt-based tool protocol. The adapter design already records `tools_unsupported`.
2. Should `bash` be allowed at all in `auto_edit` mode without asking, for read-only commands (`git status`, `ls`)? Current design: no; every bash asks unless `auto_all`.
3. Line-hash function: SHA-256 of the right-trimmed line, first 4 hex. OK, or prefer something cheaper (xxhash) once the crate list grows?
4. Do you want `similar` (diffs), `regex`, `ignore`, `sha2` added to Cargo.toml in P1, or keep zero new deps and hand-roll? (Design assumes the crates.)

**Next.** P0-T01 on the owner's machine (`bash scripts/verify_release.sh`). Then P1-T03 → P1-T05 → P1-T06 in that order; all Rust tasks will be `needs-verify` if written from a sandbox without cargo.

---

## 2026-09-09 (session 2) — SQL layer for the agentic turn + first tool modules

**Context.** Owner said "keep going" and asked for all changed/new files as a downloadable bundle (zip downloads are blocked on their side; `.md`/`.txt` work, so bundles are shipped as a zip renamed to `.md`/`.txt` — rename back after download, or use the self-extracting `harness-changes.md`).

**Changed.**
- New: `src/agentic_sql.rs` — every SQL statement the loop, permission gate, plan tool and recovery need (23 `pub const`s: `SCOPE_*`, `STEP_*`, `EVENT`/`EVENTS_AFTER`, `PERMISSION_*`, `FILE_CHANGE*`, `PLAN_*`, `SESSION_OF_REQUEST`, `RECOVER_STEPS/PERMISSIONS/ACTIVITY`). Same style as `recording_sql.rs`.
- New: `tests/test_agentic_sql.py` — 7 offline contract tests that regex-extract those constants and execute them against the real 001→002→003 schema (scope upsert + CHECK, step lifecycle incl. double-finish guard and UNIQUE(request_id,seq), event cursor, permission approve/deny idempotency + expiry, file_changes CHECK, plan replace, recovery). Discoverable by `verify_release.sh`/CI.
- New: `src/tools/mod.rs` — `Tool` trait, `Registry`, `ToolCtx`, `ToolResult` (redact + 32 KB head/tail cap), `Artifact` (FileChange | Plan) so tools never touch the DB, `PermissionMode`, `requires_permission`, `is_dangerous_command`, `line_hash`/`content_hash` (SHA-256 via `ring`), `render_line` (`N:hash│text`).
- New: `src/tools/paths.rs` — `resolve(root, input)` canonicalizes the deepest existing ancestor (works for new files), rejects `..`, absolute paths outside root, symlink escapes, the harness data dir, and secret names (`.env*`, `*.pem`, `*.key`, `id_rsa*`, …). 4 unit tests.
- New: `src/tools/textdiff.rs` — dependency-free unified diff (LCS with a 4M-cell cap, then whole-file fallback). 3 unit tests. Exists so P1-T07 needs no `similar` crate (open question 4 → leaning "zero new deps").
- New: `src/tools/fs_tools.rs` — `Read`, `Grep`, `Glob` tools (P1-T06). `Grep` prefers `rg --json` (with secret globs excluded), falls back to a literal walk and says so. `Glob` is a hand-rolled `**`/`*`/`?` matcher, mtime-desc, ≤500 paths. 1 unit test.
- Edited: `src/tools/mod.rs` declares only modules that exist; T07–T09 modules are commented with their intended names (`edit_tools`, `bash_tool`, `meta_tools`).
- Edited: `src/main.rs` adds `mod agentic_sql; mod tools;` so the new files are part of the crate.
- Edited: `docs/TASKS.md` — P1-T05, P1-T06 → `needs-verify`; notes on T09/T10/T11 saying which parts already exist.

**Verified here.** `python3 -m unittest discover -s tests -p 'test_*.py'` → OK (16 + 2 + 7). `node --check static/app.js` → OK.

**Not verified.** All Rust in `src/tools/*` and `src/agentic_sql.rs` (no cargo in the sandbox). Expect small compile fixes: borrow of `pend`/`row` tuples in tests, `include_str!` paths (relative to `src/tools/`, so `../../tools/schemas/*.json`), and possibly unused-import warnings. `agentic_sql` will warn `dead_code` until P1-T10 uses it — acceptable.

**Decisions.**
- Tools return `Artifact`s; the loop persists them. Keeps `src/tools/*` free of `rusqlite` and unit-testable with a temp dir.
- One `fs_tools.rs` instead of `read.rs`/`grep.rs`/`glob.rs`: they share `read_text`, `walk`, `glob_match`.
- Hand-rolled diff + glob + SHA via `ring` → **no new crates so far**. Revisit if `regex` is wanted for grep fallback.

**Next (in order).** P0-T01 on the owner machine → fix compile errors in `src/tools/*` (journal them here) → P1-T07 `edit_tools.rs` (use `textdiff::unified`, anchors via `line_hash`, emit `Artifact::FileChange`) → P1-T08 `bash_tool.rs` → P1-T09 `meta_tools.rs` → P1-T03 provider adapter → P1-T10 loop.

**Resume prompt for an AI.** "Read AGENTS.md, then docs/TASKS.md. Pick the first `todo` whose deps are `done`/`needs-verify`. For Rust files marked needs-verify, run `cargo test --locked` first and fix errors before adding code. Journal every session in docs/PROGRESS.md."

---

## 2026-09-11 (session 3) — P7-T05 safe incremental generation publication

**Context.** Owner asked to run the autonomous dev loop and start P7-T05. First finding of the session: `cargo` was installed under `~/.cargo/bin` but absent from the non-interactive PATH, which is why earlier sessions recorded "no cargo in the sandbox" and parked Rust at `needs-verify`. Fixed by symlinking every `~/.cargo/bin` binary into `/usr/local/bin`, so `cargo` resolves in the server's bare `/bin/sh`. `cargo build --locked`, `cargo test --locked` (161 passed at the time), `cargo clippy --all-targets`, both Python integration suites, `node --check static/app.js` and `git diff --check` were all green before any edit.

**Changed.**
- Edited `src/safety.rs` — new `classify_line` holds the BEGIN / `in_key` / `sensitive` / keep ladder shared by `redact` and the new `StreamRedactor` so they cannot drift; `REDACTION_MARKER` const; `StreamRedactor` with `push` / `finish` / `pending_len`, `emitted`-guarded `\n` join so dropped key-body lines leave no blank line. 4 new unit tests: chunking equivalence over every single cut and char-wise, split-secret holdback, no-blank-line on a dropped key body, plus an abandonment test.
- Edited `src/memory_agents.rs` — `GenerationSink` is now async (`BoxFuture` alias, `text()` / `usage()` accessors so a sink's accumulated answer can be read back); `BufferedGeneration` implements them; `consume_stream_response` publishes completed lines as they arrive and flushes the final unterminated line only at `[DONE]`, and every `fail` call is awaited.
- Edited `src/recording.rs` — new `RecordingGenerationSink`: one `chunk` row committed **before** delivery, first failure recorded as `generation_stream_save_failed` and surfaced by `generate` through `fail_recording`; `complete` writes no row. `complete_recording` now writes the terminal `completed` row only, so the answer is not duplicated.
- Edited `src/agent_loop.rs` — `run` takes the sink as a parameter instead of constructing a buffer internally.
- Edited `static/app.js` — `chunk` events append to `.generation-content` via `textContent` in `seq` order; `complete` still renders the saved answer.
- Edited `src/recording_tests.rs`, `tests/recording_integration.py` — assertions updated from the fixed two-row model to "one `chunk` row per publication, terminal row last".

**Verified here.** `cargo test --locked` → 165 passed, 0 failed. `cargo test --locked streaming` → 9 passed. `cargo test --locked redact` → 5 passed. `python3 tests/test_incremental_publication.py` → 7 OK. `bash scripts/verify_release.sh` → PASS (60 SQL contracts, migrations 001→005, tool schemas). `node --check static/app.js` → OK.

**Not verified.** The browser fixture (`tests/recording_ui.cjs`) — still blocked by the `P7-T06` runtime gap (no `npm` / `npx` / Playwright on this host). The UI change is syntax-checked only.

**Decisions.**
- Publication unit is a **completed line**, never a provider chunk or token: `redact` drops a matched line whole, and durable append-only events cannot be retracted, so a partial line is never safe. Intra-line masking stays rejected (it would change redaction semantics and needs its own task).
- The terminal `completed` row carries no content now. Chunks are the answer's durable record; re-writing the answer at completion would duplicate it. `SELECT content ... WHERE state='chunk' ORDER BY seq` is unchanged and still equals `redact(full_answer)`.
- A publication failure ends the turn explicitly rather than silently truncating an answer the reader already saw.

**Next (in order).** P7-T05 is done on its branch; P7-T06 (browser suites) remains blocked on the runtime gap and needs `npm`/Playwright or a different host. Unrelated and still open: `development-mcp` has no git repository, and `harness` has grown monolithic (`agent_loop.rs` 118 KB, `main.rs` 85 KB, `storage.rs` 70 KB) — worth a task before more features land.

---

## 2026-09-12 (session 4) — P7-T06 browser suites executable

**Context.** Owner asked to install whatever the tests needed, after P7-T05 landed. P7-T06 had been `blocked` on a runtime gap: the host had `node`, `python3` and `git` but no `npm`, `npx` or Playwright module, so both browser fixtures failed at `require('playwright')`.

**Installed.** `npm` 9.2.0 (`apt-get install --no-install-recommends npm`), then `playwright` 1.63.0 plus Chromium 1243 and its system libraries in the project. `package-lock.json` and `node_modules/` were already git-ignored, so `package.json` is the only new tracked file.

**Changed.**
- Added `scripts/setup_browser_tests.sh` — installs the runtime, fails with a clear message when `npm` is missing.
- Added `scripts/verify_browser.sh` — runs both fixtures after resolving the browser via Playwright's own `executablePath()`, so nothing is hard-coded per machine; honours a `CHROMIUM_PATH` override.
- Edited `scripts/verify_release.sh` — runs the browser suites when `node_modules` exists, skips them with a notice otherwise.
- Edited `README.md` — documents the setup and the reason the suites sit outside the default gate.
- Added `package.json` — the Playwright dev dependency.

**Verified here.** `node tests/recording_ui.cjs` -> `passed`, 18 checks. `node tests/ui_smoke.cjs` -> `passed`, 24 checks. Both run through `scripts/verify_browser.sh` with the `/usr/local/bin/chromium` symlink removed, proving the documented path stands on its own. The release gate was re-run after the edit and stays green.

**Not verified.** The fixtures exercise a mocked API only — neither starts the compiled Rust service, so they are frontend evidence, not end-to-end evidence. That boundary is printed by the fixtures themselves.

**Decisions.**
- The browser suites stay out of the default `verify_release.sh` path but run automatically when the runtime is present. A bare host should not fail the release gate over an optional browser install, and a provisioned host should not silently skip coverage.
- `verify_browser.sh` resolves Chromium from Playwright instead of a fixed path, so the repo carries no machine-specific assumption.

---

## 2026-09-12 — tailnet origin allow-list

**Goal:** make the deployed instance usable from a tailnet browser, which the
origin guard was rejecting with `Origin not allowed`.

**Root cause:** `authenticate` compared the request `Origin` against three
loopback strings built from `HARNESS_ADDR`'s port. A browser reaching the app
through `tailscale serve` sends `https://<machine>.<tailnet>.ts.net:8443`, which
could never match, so every authenticated call failed regardless of a valid
token. The guard itself is correct — arbitrary origins must not be accepted,
since the page sends the bearer token.

**Change:** `Harness` gained `origins: Arc<Vec<String>>`, seeded with the same
three loopback defaults and extended by an optional comma-separated
`HARNESS_ALLOWED_ORIGINS`. The hardcoded inline array is gone; the comparison
now reads the configured list.

**Verified here.** `cargo test --locked` -> 166 passed, 0 failed (up from 165;
new `configured_origin_is_allowed`, existing `foreign_origin_is_rejected`
still passes). Release binary rebuilt and the unit restarted. Replayed the
exact failing request with the real browser origin -> 200; `https://evil.invalid`
-> 403; `http://127.0.0.1:8080` -> 200, so the defaults are intact.

**Decisions.**
- The extra origins come from configuration, not a hardcoded hostname. The
  tailnet name is deployment-specific and must not live in source.
- Loopback defaults stay unconditional: local use should never require config,
  and removing them would break the documented `cargo run` path.
- An unset or empty variable changes nothing, so the default posture is exactly
  as strict as before.

---

## 2026-09-12 (session 5) — chat submit contract fix and main consolidation

**Reported.** Sending a message failed with `Unexpected response (422)` and the composer kept the draft, so chat was unusable and no retry recovered it.

**Root cause.** The live binary was built at 17:49 from `20c90a3` (the P7-T06 tip), 79 seconds before the tree moved to `fix-tailnet-origin-allowlist`, and was never rebuilt. In that build `static/app.js` stores the draft identity with `generation_cursor` and spreads it into the `/chat/submit` body, but `ChatRequest` is `#[serde(deny_unknown_fields)]` and has no such field. axum answered the extractor rejection itself with a `text/plain` 422, which `api()` could not JSON-parse, so the UI fell back to `Unexpected response (<status>)` and held the draft. The cursor belongs to `GenerationQuery` (`/generation/stream?after_seq=`), never to the submit body.

**Changed.**
- `src/main.rs` — added `JsonBody<T>` implementing `FromRequest`, mapping `JsonRejection` onto `ApiError` so a JSON route can never answer `text/plain`; both chat handlers use it; the rejection detail is logged as `request_body_rejected` rather than shown to the reader.
- `static/app.js` — `PENDING_FIELDS` is both the set of fields `/chat/submit` accepts and the only thing put on the wire; a stored identity is sanitized and re-persisted on load, so `generation_cursor` survives for reload-resume but is never sent. A stale draft from another build now self-heals instead of wedging every retry.
- Consolidated onto `main`: fast-forwarded to `20c90a3`, then merged `fix-tailnet-origin-allowlist` (`e264d6e` origin allow-list plus the fix above). `docs/PROGRESS.md` was the only conflict; both journal entries were kept.

**Verified here.** `bash scripts/verify_release.sh` -> PASS: `cargo test --locked`, `cargo clippy --locked --all-targets`, `cargo build --locked`, 60 Python tests OK, migrations `001 -> 002 -> 003 -> 004 -> 005` (`user_version=5`), tool schemas 13 files, both mock-provider integration suites PASS, `node --check static/app.js`, and the browser suites (`recording_ui.cjs` 18 checks including `generation_cursor_resume` and `reload_recovers_without_resend`, `ui_smoke.cjs` 24 checks). Deployed with `systemctl restart harness` (PID 56749 -> 61297); the served `app.js` then matched the tree (`cde612f0`). Live probes: the exact failing payload and malformed JSON both return `400 application/json` `{"error":"Message payload was not accepted. Reload this tab, then send again"}`; an empty prompt still returns `Prompt must contain 1-16000 UTF-8 bytes`; the tailnet origin is accepted; a foreign origin still gets `403 Origin not allowed`; a missing token still gets `401 Bearer token required`.

**Confirmed by owner.** End-to-end chat, including the real provider round-trip, working on the restarted service (2026-09-12 09:07 +07). No provider turn was sent from the agent side.

**Next.** Six `Json<...>` extractors remain (`confirm`, `candidates`, scope patch, decision, memory ingest); converting them to `JsonBody` would make every JSON route answer JSON uniformly. `tests/integration_smoke.py:84` already accepts `400` or `422`, so that change needs no test edit. Also still open from earlier sessions: `development-mcp` has no git repository, and `harness` is monolithic (`agent_loop.rs`, `main.rs`, `storage.rs`).

---

## 2026-09-12 (session 5, follow-up) — uniform JSON rejections and branch cleanup

**Changed.** The six remaining body extractors in `src/main.rs` (`confirm`, `edit_candidate`, `set_config`, `set_scope`, `decide_permission`, `ingest_memory`) now use `JsonBody`, so no served route can answer a malformed body with axum's `text/plain` 422. One plain `Json` extractor remains in `src/agent_loop.rs`: it is the mock provider inside the test module, not a served route, so it stays.

**Branches.** Deleted seven fully merged topic branches locally and on origin: `p7-t05-incremental-publication` (0f4ad37), `p7-t06-browser-runtime` (20c90a3), `p7-frontend-generation-stream` (460f667), `p7-migration-reporting` (c7e35bf), `p7-generation-replay-attribution` (a758030), `autonomous-development-streaming` (554fa6f), `fix-tailnet-origin-allowlist` (965457d). Every SHA is reachable from `main`, so any branch can be recreated with `git branch <name> <sha>`. Kept `upcloud-verify` (not merged) and the `v1`, `v2-upgrade`, `v3` release lines. This removes the divergent tips that caused a build from a stale branch head earlier in the session.

**Verified.** `bash scripts/verify_release.sh` -> PASS (60 Python tests, migrations `001 -> 005`, both mock-provider integration suites, `recording_ui.cjs` 18 checks, `ui_smoke.cjs` 24 checks), then redeployed with `systemctl restart harness` and probed each converted route with a body no target type can accept.

---

## 2026-09-12 (session 5, follow-up 2) — a JSON body must be an object, and a deploy must prove itself

**Found while verifying the previous entry.** `POST /scopes/global` with a body of `[]` answered `200 application/json` with the stored row instead of `400`, and moved that row's `updated_at`. Not owner-reported: the probe was mine, and it was disclosed at the time. Reading the source proved no column value changed (the merge applied six `None`s); `created_at` stayed `2026-09-09T15:49:14`.

**Root cause.** serde's derive accepts the *sequence* form of a struct as well as the map form, and every field of `ScopePatch` is `#[serde(default)]`, so a zero-length array deserializes as a valid all-defaults patch. `#[serde(deny_unknown_fields)]` cannot catch it, because an array carries no field names to reject. The five other routes converted in the previous entry refused `[]` only by luck: their target types have required fields, so the sequence length did not match. Two defects, not one — the API accepted a body shape it never meant to accept, and a patch that named no field still rewrote a row.

**Changed.** `JsonBody<T>` in `src/main.rs` no longer delegates to `Json<T>`'s extractor: it checks the content type itself, buffers with `Bytes`, refuses any body whose first non-whitespace byte is not `{`, then parses with `Json::<T>::from_bytes`. The bound moved from `Json<T>: FromRequest<S, Rejection = JsonRejection>` to `T: DeserializeOwned`, so all eight extractor sites and any target type added later inherit the rule. `upsert_scope` in `src/storage.rs` returns the stored row untouched when `ScopePatch::is_empty()`, while still creating a scope that does not exist yet (`configured_scopes_are_listed_for_the_picker` depends on that). Rejection wording is now `Request body was not accepted. Reload this tab, then send again`, since it is no longer only about chat messages. `docs/design/agentic-turn.md` states the object rule and the `{}` semantics.

**The deploy path was lying.** The gate was green, the commit was pushed, `systemctl restart harness` reported success — and the live service still answered `[]` with `200` and the pre-fix wording. The unit starts `target/release/harness`, but `scripts/verify_release.sh` runs `cargo build --locked`, the debug profile. Nothing in the repo ever built the artifact systemd starts, so the restart faithfully relaunched the binary from `02:16:43`, which predated the fix. This is the same gap that produced the stale-binary incident earlier in the session, which was misread at the time as a branch problem. `scripts/deploy.sh` now owns deployment: it blocks unless the unit's `ExecStart` names the binary it is about to build, builds `--release`, restarts, waits for `active`, compares the md5 of `/proc/<MainPID>/exe` against the binary it just produced, then smoke-tests that the API answers and that a non-object body is still refused. A restart that deploys nothing can no longer look like a success. README documents the path and `verify_release.sh` now states which profile it validates.

**Verified here.** `bash scripts/verify_release.sh` -> exit 0: 169 Rust tests (new `non_object_json_bodies_are_refused_with_json`, `an_empty_patch_still_creates_a_missing_scope`, `an_empty_patch_leaves_the_stored_row_untouched`), 60 Python tests, migrations `001 -> 005` (`user_version=5`), 13 tool schemas, both mock-provider HTTP suites, `recording_ui.cjs` 18 checks, `ui_smoke.cjs` 24 checks. Then `bash scripts/deploy.sh` -> `deployed 5e48f1d to harness: pid 64619, release md5 bb0decb1934012666c827bc6435e6a2e, API answering, non-object body refused with 400`. Live probes against the deployed build: `[]`, `"ask"` and an empty body each return `400 application/json` `{"error":"Request body was not accepted. Reload this tab, then send again"}`, with `{"detail":"Body is not a JSON object","event":"request_body_rejected"}` in the journal; `{}` returns `200` with the stored row and `updated_at` still `2026-09-12T03:02:37.439006652`, unchanged across the post and a following read.

**Side effect, disclosed.** The two pre-fix probes moved the live `global` row's `updated_at` to `03:02:37.425` and then `.439`. No column value changed and `created_at` is intact; with the fix deployed, a body that names no field can no longer move it.

**Next.** `static/app.js` still lists `422` in its retry gate; no served route can produce one now, so it can go. The binary carries no version stamp, so `deploy.sh` proves identity by md5 rather than by commit — stamping the build would be better. Still open from earlier sessions: `development-mcp` has no git repository, and `harness` stays monolithic (`main.rs` is now ~92 KB). The local MCP bridge dropped twice today, once mid-deploy; the work resumed unchanged after waiting it out.
