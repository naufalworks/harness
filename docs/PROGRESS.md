# PROGRESS — journal

Append-only. Newest entry first. Each entry: date, who (human / AI session), what changed,
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
