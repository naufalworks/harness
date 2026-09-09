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
