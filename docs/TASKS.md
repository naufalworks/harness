# TASKS — ordered backlog with stable IDs

How to use: pick the first task with `status: todo` whose `depends:` are all `done`.
Change status in place. Never renumber or delete a task; mark it `dropped` with a reason.

Status values: `todo` | `doing` | `done` | `needs-verify` | `blocked` | `dropped`

Each task has: `status`, `depends`, `design` (doc section), `files` (touched),
`done-when` (observable outcome), `verify` (command).

---

## P0 · Gate (owner machine)

### P0-T01 · Compile and run the existing suites
- status: done
- depends: —
- design: docs/ROADMAP.md#p0
- files: (none; run only)
- done-when: `cargo test --locked`, `cargo clippy --locked --all-targets`, `cargo build --locked`, both Python HTTP suites and `node --check static/app.js` pass on the owner's machine; failures are fixed with minimal diffs and journaled in PROGRESS.md.
- verify: bash scripts/verify_release.sh
- note: Passed in the owner's local checkout with Cargo/Rust 1.92.0: 29 Rust tests, Clippy, release build, 50 Python contract tests, both mock-provider HTTP suites, and frontend syntax checks all passed. Browser suites remain a separate UI check.

### P0-T02 · Verify migration chain and schemas offline
- status: done
- depends: —
- design: docs/design/agentic-turn.md#schema
- files: tests/test_migrations.py, tests/test_tool_schemas.py
- done-when: migrations 001→003 apply on an empty DB and on a v2 DB; all `tools/schemas/*.json` parse and have `name`, `description`, `parameters` with `additionalProperties:false`.
- verify: python3 tests/test_migrations.py && python3 tests/test_tool_schemas.py

## P1 · Tool loop with receipts per step

### P1-T01 · Migration 003_agentic.sql
- status: done
- depends: —
- design: docs/design/agentic-turn.md#schema
- files: migrations/003_agentic.sql, src/storage.rs (accept user_version 3, apply 003)
- done-when: `DbStore::init` upgrades v2→v3 additively; new tables `scopes`, `turn_steps`, `activity_events`, `permission_requests`, `file_changes`, `plan_items` exist. Recovery updates for agentic steps and permissions are owned by P1-T10.
- verify: python3 tests/test_migrations.py && cargo test --locked storage
- note: Verified in the owner's local checkout: migrations 001→003 apply with constraints enforced, and `cargo test --locked storage` passed. The `recover()` additions for steps/permissions remain part of P1-T10.

### P1-T02 · Tool schemas and system prompts
- status: done
- depends: —
- design: docs/design/tools.md
- files: tools/schemas/*.json, prompts/main_agent.md
- done-when: eight schemas (read, grep, glob, edit, write, bash, think, todo_write) in OpenAI function format; system prompt states evidence-bound and verify-after-write rules in ≤ 60 lines.
- verify: python3 tests/test_tool_schemas.py

### P1-T03 · Provider adapter: tools in, tool_calls out (non-streaming)
- status: done
- depends: P0-T01
- design: docs/design/agentic-turn.md#provider-adapter
- files: src/memory_agents.rs (rename later to provider.rs is optional), src/agent_loop.rs (types)
- done-when: `complete_with_tools(model, messages, tools) -> ModelTurn { text: Option<String>, tool_calls: Vec<ToolCall>, usage }`; `ToolCall { id, name, arguments_json }`; malformed JSON arguments produce a `tool_call` step with `status=failed, error_code=invalid_arguments` (fed back to the model as an error result), never a panic; the old text-only `chat_prepared` path is unchanged for `extraction`.
- verify: cargo test --locked provider
- note: Implemented in `src/memory_agents.rs`: OpenAI-style `tools` + `tool_choice=auto`, parsed tool calls/usage, verbatim assistant-message replay, bounded provider errors, and safe malformed-argument parsing. `cargo test --locked provider` passed (6 tests); the full release gate also passed (35 Rust tests, 50 Python contracts, HTTP suites, and frontend syntax). Browser suites remain separate.

### P1-T04 · Scopes: root_path, permission_mode, diagnostics_cmd
- status: done
- depends: P1-T01
- design: docs/design/agentic-turn.md#scopes
- files: src/main.rs (GET/POST /scopes/{scope}), src/storage.rs
- done-when: a scope row can be created/updated with a canonical absolute `root_path` that exists and is a directory, `permission_mode in (ask, auto_edit, auto_all)`, optional `diagnostics_cmd` (≤ 512 chars); tools refuse to run for scopes without `root_path`.
- verify: cargo test --locked scopes
- note: `GET/POST /scopes/{scope}` land in src/main.rs; `ScopeConfig`, `ScopePatch::validate` and `DbStore::scope_config`/`upsert_scope` in src/storage.rs (using the existing `SCOPE_GET`/`SCOPE_UPSERT` constants). A POST merges into the stored row, so an unmentioned field keeps its value and an explicit `null` clears it — a partial POST can never silently disable tools. Validation happens before SQLite so bad input is a 400, not a CHECK failure: `root_path` is trimmed, must be absolute, is canonicalized (symlinks resolved), must be an existing directory, must not be the filesystem root and must not sit inside the harness data dir; `permission_mode` goes through `PermissionMode::parse`; `diagnostics_cmd` is trimmed, capped at 512 chars and refused if it matches the bash deny-list; budgets are range-checked to match the 003 CHECKs. The tool gate is `Registry::invoke(ctx, name, args)`, where `ctx` is `None` for a scope without `root_path` and every call fails with `tools_disabled`. `cargo test --locked scopes` → 6 passed; full `bash scripts/verify_release.sh` → 41 Rust tests, Clippy, build, 50 Python contracts, both HTTP suites, frontend check.

### P1-T05 · Tool registry and sandboxed path resolution
- status: done
- depends: P1-T04
- design: docs/design/tools.md#registry, docs/design/tools.md#path-rules
- files: src/tools/mod.rs, src/tools/paths.rs
- done-when: `Tool` trait `{ name, schema, side_effecting, run(ctx, args) -> ToolResult }`; `resolve(root, user_path)` canonicalizes and rejects anything outside root, symlink escapes, and denied names (`.env*`, `*.pem`, `id_rsa*`); `ToolResult { content, truncated, bytes, artifacts }` with content passed through `safety::redact` and capped at 32 KB (head 24 KB + tail 8 KB).
- verify: cargo test --locked tools::paths
- note: written 2026-09-09 as src/tools/mod.rs + src/tools/paths.rs (+ src/tools/textdiff.rs, a dependency-free unified diff for T07). Not compiled (no cargo in the AI sandbox). SHA-256 via the existing `ring` crate, so no new deps yet. `is_dangerous_command` lives in mod.rs so the gate compiles before T08.
- note: verified in the owner's checkout — compiles with no changes needed, `cargo test --locked tools::paths` → 4 passed (inside-root, escape, secret names, symlink escape). `harness_data_dir()` is now `pub` so P1-T04 can refuse a `root_path` inside it, and `Registry::invoke` was added as the single entry point that refuses when a scope has no root.

### P1-T06 · read / grep / glob
- status: done
- depends: P1-T05
- design: docs/design/tools.md#read, #grep, #glob
- files: src/tools/fs_tools.rs
- done-when: `read` returns numbered lines `N:hash│text` with per-line 4-hex hash (see design), supports `offset/limit`, caps 400 lines per call, reports `total_lines` and file `content_hash`; `grep` shells to `rg` when available else falls back to a Rust regex walk, max 100 matches, 1 context line; `glob` returns ≤ 500 paths sorted by mtime desc, respects `.gitignore` when `rg`/`ignore` crate available.
- verify: cargo test --locked tools::fs_tools
- note: written 2026-09-09 as ONE file src/tools/fs_tools.rs (Read, Grep, Glob) instead of three; grep shells to `rg --json` when present, else a literal (non-regex) walk that says so in its output; glob uses a hand-rolled `**`/`*`/`?` matcher and skips .git/target/node_modules. Not compiled.
- note: verified in the owner's checkout — compiles unchanged, and end-to-end tests over a temp project were added for all three tools: `cargo test --locked tools::fs_tools` → 4 passed. The old `verify` line named three module paths that never existed (all three tools share one module), so it was corrected to the real path rather than splitting the file. Caveat: `rg` is installed on this machine, so the literal fallback branch and the `.gitignore` behaviour that comes with ripgrep are still unexercised by tests.

### P1-T07 · edit / write with hash anchors and file_changes
- status: done
- depends: P1-T06
- design: docs/design/tools.md#edit, #write, docs/design/agentic-turn.md#file_changes
- files: src/tools/edit.rs, src/tools/write.rs
- done-when: `edit` accepts `{path, anchors:[{line, hash}], old_string?, new_string}`; every anchor must match the current file or the tool fails with `stale_anchor` and the current lines; `old_string` (if given) must match exactly once; edit/write first prepare and describe the change, then the loop records `file_changes` before applying an approved change; writes are atomic (temp + rename) and the row is flipped to `applied=1` after; `write` refuses existing files unless `overwrite:true`; both run `diagnostics_cmd` (if set, 60 s cap) and append its trimmed output to the result.
- verify: cargo test --locked tools::edit_tools
- note: written as ONE file `src/tools/edit_tools.rs` (Edit + Write + shared plan/apply/diagnostics), matching the `fs_tools.rs` precedent; the old `verify` line named `tools::write`, which matches no test path, so it was corrected. The prepare/apply split is a new default trait method `Tool::plan(ctx, args) -> Option<Result<PendingChange, ToolResult>>` plus `PendingChange` in `src/tools/mod.rs`: `plan` never writes, so the loop can record `file_changes(applied=0)` and render a diff for approval, and `run` re-plans before applying (the file can change while a permission request is pending, and the anchors must still hold at write time). `permission_payload` carries `{path, action, plus, minus, before_hash, after_hash, diff}` for the UI; `summary` stays `edit <path>` because the trait gives it no filesystem access, so the ± counts live in the payload. Writes go to `.<name>.harness-tmp-<step>` in the destination directory and are renamed, preserving the existing file mode; the temp file is removed on failure. An identical rewrite is reported as "already had this content" and records no `file_changes` row. Diagnostics run through a shared `run_capped` helper (`sh -c`, cwd = root, env stripped to PATH/HOME/LANG/LC_ALL/TERM + HARNESS_SCOPE, output interleaved through a temp file so a full pipe cannot deadlock the timeout poll) — P1-T08 reuses it. `cargo test --locked tools::edit_tools` → 7 passed; full gate → 51 Rust tests.

### P1-T08 · bash
- status: done
- depends: P1-T05
- design: docs/design/tools.md#bash
- files: src/tools/bash_tool.rs
- done-when: runs `sh -c` with cwd = root_path, env stripped to a whitelist (PATH, HOME, LANG, TERM), timeout default 120 s (max 600), output capped per `ToolResult`; `background:true` starts detached with stdout/stderr to `<root>/.harness/logs/<step>.log` and returns the pid + log path; a deny-list of destructive patterns (`rm -rf /`, `git push --force`, `mkfs`, `dd if=`, `> /dev/sd`) always requires permission even in `auto_all`.
- verify: cargo test --locked tools::bash
- note: written as `src/tools/bash_tool.rs` (the name the `src/tools/mod.rs` stub already reserved; the `verify` filter still matches `tools::bash_tool::tests`). Foreground calls reuse `edit_tools::run_capped`, so the whitelist (`PATH HOME LANG LC_ALL TERM` + `HARNESS_SCOPE`), the cwd and the temp-file capture are shared with `diagnostics_cmd` and cannot drift apart. `run_capped` gained two things for this task: the child is spawned in its own process group (`process_group(0)`) and a timeout now kills the group, not just the shell, so a `make`-style tree cannot outlive its step; and a `started` flag separates "could not spawn" (`spawn_failed`) from "killed by a signal" (`signal`). A non-zero exit is deliberately **not** a failed step — status stays `complete` with the code in the new `ToolResult::exit_code` (the `tool_finished {exit_code?}` field in the events contract) and the output ending in `[exit N]`, because a red test run is information the loop must act on. A timeout is a failed step but keeps the partial output via the new `ToolResult::failed` (raw output instead of a JSON envelope). `timeout_seconds` above 600 is clamped with a note in the output rather than refused; a non-integer or `< 1` value is `invalid_arguments`. Background uses `sh -c '<cmd>' >>log 2>&1 &` and reports `$!`, not `setsid`: macOS does not ship `setsid`, and the util-linux versions that fork print the wrapper's pid instead of the job's — the outer shell exiting immediately already reparents the job to init, and the process group above already detaches it from our signals. Logs land in `<root>/.harness/logs/<step_id>.log` with the step id reduced to a safe stem. The deny-list itself was already in `src/tools/mod.rs` (`is_dangerous_command`, used by `Registry::requires_permission`); this task covers it with tests and surfaces it to the UI as `dangerous` in `permission_payload`. `cargo test --locked tools::bash` → 8 passed.

### P1-T09 · think / todo_write and plan_items
- status: done
- depends: P1-T01
- design: docs/design/tools.md#think, #todo_write
- files: src/tools/meta_tools.rs, src/storage.rs
- done-when: `think` stores its text as the step output and returns "noted"; `todo_write` replaces the session's plan (≤ 30 items, each ≤ 200 chars, one `in_progress` max) in `plan_items` transactionally and returns the normalized list.
- verify: cargo test --locked plan
- note: SQL for plan_items already exists in src/agentic_sql.rs (PLAN_CLEAR/PLAN_INSERT/PLAN_LIST) and is contract-tested by tests/test_agentic_sql.py; only the Rust tool + storage method remain.
- note: written as ONE file `src/tools/meta_tools.rs` (the name the `mod.rs` stub reserved) instead of `think.rs` + `todo.rs`; the old `verify` line named `tools::todo`, which matches no test path. The filter is now `plan`, which covers both halves of the task — `tools::meta_tools::tests::*` and `storage::tests::plans_are_replaced_whole_or_not_at_all` — in one command. Neither tool is `side_effecting`, so a scratchpad note and a plan update can never sit waiting for an approval; that also keeps them out of the deny-list path. `think` returns the trimmed text as the step **output** (the UI's collapsed reasoning card) and puts the "noted" acknowledgement in the `summary`, because a single-field result cannot be both; over 4000 chars is `too_large`. `todo_write` normalizes (trim, absent `status` defaults to `pending`, `seq` = position) and enforces ≤ 30 items / ≤ 200 chars / one `in_progress` as `invalid_arguments` so the model can fix its own plan instead of hitting a CHECK. It hands the result to the loop as `Artifact::Plan`, and `DbStore::replace_plan` does the writing in one Immediate transaction (clear + insert + read back), so tools still never touch the database; the same limits are re-checked there because a CHECK failure would otherwise surface as an opaque 500. `MAX_PLAN_ITEMS`/`MAX_PLAN_TEXT`/`PLAN_STATUSES` live in `src/storage.rs` as the single source of truth. `DbStore::plan` reads the stored list for the `plan_updated` event and the P1-T13 panel. A new registry test asserts all eight schema files parse and their `function.name`s match the registered tools in order. `cargo test --locked plan` → 6 passed.

### P1-T10 · Agent loop in the generation worker
- status: done
- depends: P1-T03, P1-T05, P1-T07, P1-T08, P1-T09
- design: docs/design/agentic-turn.md#loop
- files: src/agent_loop.rs (new: window, run, step/permission storage), src/recording.rs (generate calls the loop; recover marks steps/permissions/activity), src/storage.rs (write_plan replaces replace_plan), src/memory_agents.rs (chat_prepared removed), src/main.rs (mod agent_loop), src/tools/meta_tools.rs (doc comment)
- done-when: `generate()` runs the loop from the design doc: commit `model_call` step → provider → commit output → for each tool call commit `tool_call` step (and `permission_request` if required) → run → commit result → append to messages → repeat until final text or budget (`max_steps=40`, `max_tool_bytes=400_000`, `max_wall=15 min`); every transition writes an `activity_events` row; `context_json` is written once with the initial window and each `model_call` step stores its own full message array; restart marks `running` steps `interrupted`, the receipt `interrupted`, and never re-executes a tool.
- verify: cargo test --locked agent_loop && python3 tests/recording_integration.py
- note: the `files` line above is corrected: no new `recording_sql.rs` statements were needed — every statement for steps, events, permissions, file changes and recovery already existed in `src/agentic_sql.rs` — and `recover()` lives in `src/recording.rs`, not `src/storage.rs`. The `verify` line was already right and passes as written. `src/agent_loop.rs` holds the storage half (`begin_step`, `finish_step`, `activity`, `request_permission`, `permission_status`, `expire_permission`) next to the loop that is its only caller; `begin_step` and `finish_step` each do their whole job — sequence, step row, activity event, and for `finish_step` also the artifacts — in ONE Immediate transaction, so a step is never visible without its event. `Artifact::FileChange` writes a `file_changes` row with **`applied=1`**, because the tool has already written the file by the time the loop sees the artifact; the plan-then-approve-then-write shape in the design doc needs a dry-run tool contract, so `PendingChange` and `Tool::plan` stay unused (and warned about) until P1-T11. `Artifact::Plan` goes through the new `storage::write_plan`, which replaced `DbStore::replace_plan` so the plan commits in the same transaction as the step that produced it. Budget: `exhausted()` is checked before every provider call and the resulting answer says in as many words that the task is NOT finished, so an out-of-budget turn cannot read as a success. `context_json` is written once with the first window (as the contract requires) while every `model_call` step stores its own full message array plus the tool **names** — not the schemas, which would multiply ~8 KB of JSON by every step. Unparsable or non-object tool arguments fail the step with `invalid_arguments` before any tool is reached. Text-only turns (no scope root) now run through the same loop with an empty tools array, which is why `memory_agents::chat_prepared` was deleted rather than kept as a second path. All 13 activity kinds are emitted: `answer_saved` from `complete_recording`, `turn_failed` from `fail_recording`, `interrupted` from `recover`, the rest from the loop. The permission gate is built as far as this task can take it — create the row, poll every 500 ms bounded by the turn's wall budget, expire and deny on timeout, hand the denial back to the model as a tool error — while P1-T11 still owns the HTTP endpoints, idempotent approve/deny, the `permission_resolved` event for a human decision, and the tests for a real approval. `cargo test --locked agent_loop` → 7 passed against an in-process scripted provider; `bash scripts/verify_release.sh` → exit 0 (71 Rust tests, clippy, release build, 50 Python contracts, both mock-provider HTTP suites including the SIGKILL/no-replay assertions).

### P1-T11 · Permission gate
- status: done
- depends: P1-T10
- design: docs/design/agentic-turn.md#permissions
- files: src/agent_loop.rs, src/main.rs (GET /permissions?scope=, POST /permissions/{id})
- done-when: in `ask` mode every side-effecting tool creates a `pending` permission row and the loop waits (poll 500 ms, expire after 30 min → tool result "denied: timed out"); `auto_edit` auto-approves edit/write only; `auto_all` auto-approves everything except the bash deny-list; approve/deny is idempotent and recorded as an activity event; denial is returned to the model as a tool error so it can adapt.
- verify: cargo test --locked permissions && python3 tests/recording_integration.py
- note: permission SQL exists in src/agentic_sql.rs; `Registry::requires_permission` (mode × side_effecting × bash deny-list) exists in src/tools/mod.rs. P1-T10 already landed the worker side: `request_permission` writes the `pending` row and the `permission_requested` event, the loop polls every 500 ms, `expire_permission` flips a timed-out row and the denial reaches the model as a tool error. What is left is the HTTP surface (`GET /permissions?scope=`, `POST /permissions/{id}`), idempotent approve/deny with the `permission_resolved` activity event for a human decision, the `auto_edit`/`auto_all` shortcuts end to end, and tests for an approval that actually unblocks a waiting turn — today nothing can approve in `ask` mode, so a side-effecting call waits out the turn's wall budget and is then denied. Note the 500 ms poll is bounded by the turn's remaining wall budget (15 min), which lands before the row's 30 min TTL; if the TTL is meant to be reachable, T11 has to say which one wins.
- note (done): the HTTP surface is `GET /permissions?scope=` (pending rows with the tool's own summary and `args_json` payload) and `POST /permissions/{id}` body `{decision:"approve"|"deny", scope}`. `DbStore::resolve_permission` does the decision in ONE Immediate transaction: the `status='pending'` guard in `PERMISSION_RESOLVE` makes the first decision win, so a replayed click is `200 {"recorded":false}` with **no** second `permission_resolved` event, the opposite decision is `409` and never flips the row, a decision after the loop gave up is `410`, and a wrong scope is `404` rather than an acknowledgement that the row exists. The loop needed no new waiting logic — it already polls `status`, so committing the decision is what unblocks the turn. **The TTL question is answered: the earlier deadline wins.** `permission_ttl()` writes `expires_at` as `min(30 min, remaining wall budget)`, so the pending card cannot advertise a window the turn will not honour, and `request_permission` now takes that TTL from its caller instead of assuming 30 min. `auto_edit`/`auto_all` needed no code (`Registry::requires_permission` already had the matrix) but are now covered end to end. `cargo test --locked permissions` → 8 passed: an approval lets a write reach disk with exactly one `permission_resolved` event, `auto_edit` applies a write with no row at all yet still stops `bash`, `auto_all` still asks before a deny-listed `git push --force`, resolution is idempotent and refuses a flip, an expired row cannot be approved afterwards, the TTL clamp picks the earlier clock, and the endpoints reject an unknown id, a bad decision word and an unauthenticated caller. `bash scripts/verify_release.sh` → exit 0 (79 Rust tests). Not in this task: the Approve/Deny UI card (P1-T13) and a python HTTP approval round-trip (P1-T14); `PERMISSION_GET`, `PERMISSION_RESOLVE` and `PERMISSIONS_PENDING` are no longer dead code.

### P1-T12 · API: steps, plan, activity
- status: done
- depends: P1-T10
- design: docs/design/agentic-turn.md#api
- files: src/main.rs, src/storage.rs
- done-when: `GET /chat/requests/{id}/steps` (ordered steps with bounded output), `GET /sessions/{id}/plan`, `GET /activity?after_seq=N&session_id=` (≤ 200 events) all authenticated and covered by the mock-provider HTTP suite.
- verify: python3 tests/recording_integration.py
- note (done): readers live in `src/storage.rs` (`turn_steps`, `activity_since`, `turn_changes`; `plan` already existed) with thin handlers in `src/main.rs`. They only read rows the loop committed — nothing recomputes a summary or re-renders a diff — so the UI cannot show a version of the turn the record disagrees with. `GET /chat/requests/{id}/steps` 404s on an unknown request (an empty list would read as "this turn did nothing") and reports `previews_capped` separately from `truncated`, because the preview hitting 2 KB and the tool's own output being capped are different facts; `summary` is read back from the finished step's output, so a still-running step has none and its `tool_started` event carries it instead. `GET /activity` returns `next_after_seq` that only advances when rows came back, so a poll finding nothing cannot skip an event committing a moment later; the feed is the agentic log only — `captured`/`generation_started` stay in `recording_events` behind `/context`. `GET /sessions/{id}/plan` treats an unknown session and a session without a plan the same (`{items:[]}`), matching `/sessions/{id}/messages`. I also added `GET /changes?request_id=` from the same design section: it is 10 lines, the last dead SQL constant, and P1-T13/P2-T03 need it; `POST /changes/{id}/revert` remains P2-T03. `python3 tests/recording_integration.py` → PASS with new real-HTTP coverage: a completed turn's single `model_call` step and its four-event feed with cursor paging, a provider failure reading `failed/provider_failed` and ending in `turn_failed`, and a SIGKILL'd step reading `interrupted` rather than `failed`, plus 401/403/404 for the new routes. `cargo test --locked` → 80 passed; `bash scripts/verify_release.sh` → exit 0.

### P1-T13 · Minimal UI for steps and permissions (polling)
- status: done
- depends: P1-T04, P1-T11, P1-T12
- design: docs/design/ui.md#p1-minimal
- files: static/app.js, static/index.html, static/style.css
- done-when: each user turn shows a collapsible step list (`● read src/x.rs 1-120`, `● edit +3 −1`, `● bash cargo test — exit 0`); a pending permission renders an Approve/Deny card at the bottom of the chat; the plan renders as a checklist above the composer; scope settings form (root path, mode, diagnostics command) in the Models tab; all text via `textContent`, never innerHTML.
- verify: node --check static/app.js && CHROMIUM_PATH=… node tests/recording_ui.cjs
- note (done): restored the existing local-first conversation shell and added the P1 turn record panel. It polls `/chat/requests/{id}/steps`, `/sessions/{id}/plan`, and `/permissions?scope=` while a request is pending; steps are native collapsible `<details>` rows with bounded input/output previews, permissions show the tool-owned diff or command and idempotent Approve/Deny actions, and the plan is a checklist with progress. The Models tab is now Project & models and posts root path, permission mode, diagnostics command, and budgets to `/scopes/{scope}`. All model/tool/user-controlled text is built with `textContent`/DOM nodes; no inline styles or `innerHTML` were added. `node --check static/app.js`, `git diff --check`, the HTML-hook check, and `cargo test --locked` (80 passed) are green. The browser harness could not run because this checkout has no `playwright` module installed.

### P1-T14 · Mock provider with tool calls + recovery tests
- status: done
- depends: P1-T10, P1-T11
- design: docs/design/agentic-turn.md#testing
- files: tests/recording_integration.py, tests/mock_provider.py
- done-when: the synthetic loopback provider can be scripted to emit tool_calls; suite covers: happy path read→edit→bash→answer, stale anchor rejection, path escape rejection, permission deny, SIGKILL during a tool with `interrupted` state and no re-execution, budget exhaustion message.
- verify: python3 tests/recording_integration.py
- note (done): added `tests/mock_provider.py`, a reusable OpenAI-style loopback server with per-prompt response scripts and request capture. The real HTTP suite now configures a temporary project scope and covers read → edit → bash → answer with tool-result replay and applied file changes, stale hash anchors with no disk mutation, root escape rejection, a pending write denied through `POST /permissions/{id}`, honest max-steps exhaustion with no extra provider call, SIGKILL during a running bash step with recovery to `interrupted` and no re-execution, plus provider failure and SQLite integrity. `python3 tests/recording_integration.py` → PASS; `bash scripts/verify_release.sh` → exit 0 (80 Rust tests, migrations, schemas, 50 Python contracts, real HTTP suite).

### P1-T15 · First-run project setup (found by using the harness)
- status: done
- depends: P1-T04, P1-T13
- design: docs/design/tools.md#path-rules (scope root), docs/design/agentic-turn.md#prompt
- files: prompts/main_agent.md, src/agent_loop.rs, src/agentic_sql.rs, src/storage.rs, src/main.rs, static/index.html, static/app.js
- done-when: a scope with no `root_path` is visible as such before the user sends anything, the model's tool-less answer names the missing project root as the cause, and a scope can be discovered and configured without leaving the UI.
- verify: cargo test --locked scopes && cargo test --locked prompt_names
- note (done): the first real use of the harness failed on onboarding, not on code. With the default `global` scope unconfigured the loop correctly attached zero tools (P1-T04), but the prompt still said "You have tools" unconditionally, so the model answered "I don't have access to a terminal or file system tools in this conversation" — true about the turn, wrong about the cause, and unfixable by the user, who then had to `curl POST /scopes/...` to get anywhere. Three changes, one per layer: (1) `prompts/main_agent.md` now carries a `{{tools}}` slot; `window()` fills it with `TOOLS_ATTACHED` or `TOOLS_WITHHELD`, and the withheld text makes the model say tool access is off *because this scope has no project root* and name the one place to fix it. (2) `GET /scopes` (new `SCOPES_LIST`, `DbStore::scopes`) lists every configured scope, including a scope with a null `root_path` — it exists, it just cannot run tools — so the scope field is a `datalist` of real choices instead of an unguessable free-text box. (3) the chat view carries a `#setup-banner` that appears whenever the current scope has no root, links straight to the existing project form, and names the scopes that are already set up; the loop also records one `tools_withheld` activity event next to `turn_started` so `/activity` shows the same fact. Deliberately unchanged: tools stay off for an unconfigured scope (the gate is the security property, not the bug) and `POST /scopes/{scope}` keeps its merge semantics. `bash scripts/verify_release.sh` → exit 0 (83 Rust tests, clippy, release build, 50 Python contracts, migrations, 8 schemas, both mock-provider HTTP suites); `node --check static/app.js` → OK. Not covered by the gate: the browser suites (`tests/ui_smoke.cjs`, `tests/recording_ui.cjs`) still need a running server, so the banner and datalist are verified by construction and by hand, not by CI.

## P2 · Streaming and activity rail

### P2-T01 · SSE endpoint over activity_events
- status: done
- depends: P1-T12
- design: docs/design/agentic-turn.md#sse
- files: src/main.rs
- done-when: `GET /activity/stream?after_seq=N` (auth header via fetch, not EventSource) sends `id: <seq>` events, heartbeats every 15 s, resumes from `after_seq` with exactly-once delivery from the DB.
- verify: python3 tests/recording_integration.py
- note (done): the rail is live. New `GET /activity/stream?session_id=&after_seq=N` sits under the same auth and Origin layer as `/activity`: a spawned task polls `EVENTS_AFTER` every 200 ms (batch 200, the statement's own LIMIT) and pushes `id: <seq>` / `event: <kind>` / `data: <the row /activity would return>` frames through a 64-frame channel into `Body::from_stream`, emits a `: heartbeat` comment after 15 s of quiet, and gives up after 25 consecutive read failures. Bounds match the polled feed: 400 on a non-UUID session or a negative cursor, 401 without a bearer token. Exactly-once is the DB sequence, not the socket — the cursor advances only past frames already queued, so reconnecting with the last `id` replays nothing and drops nothing. `static/app.js` swaps `refreshAgentTurn`'s 1 s poll for one `fetch` + `TextDecoder` reader per turn (header auth, so the token never enters a URL; `EventSource` cannot send headers), debounces rail refreshes at 150 ms, marks a hidden tab stale instead of rendering, closes the stream on a terminal receipt state, and falls back to the old poll after 3 failed reconnects (1 s → 5 s backoff). Adds `futures-core` and tokio's `test-util` dev feature. verify passed: `python3 tests/recording_integration.py` → PASS with the new SSE block (streamed frames equal the `/activity` rows for the same session, resuming at `next_after_seq` replays nothing, `after_seq=-1` and a bad session are 400, no token is 401); `cargo test --locked` → 85 (two new: frames-and-resume, plus a 15 s heartbeat under `start_paused` virtual time); `bash scripts/verify_release.sh` → exit 0; `node tests/ui_smoke.cjs` 14/14 and `node tests/recording_ui.cjs` 15/15 with the new `activity_stream_subscribed` check. Deliberately left: the 1 s poll stays as the fallback path, and frames still come from a 200 ms DB poll rather than a write notification.

### P2-T02 · Three-pane layout and activity rail
- status: done
- depends: P2-T01, P1-T13
- design: docs/design/ui.md#p2-layout
- files: static/*
- done-when: sessions/scopes left, chat center, activity right (plan, running step with elapsed time, context meter placeholder, permission prompt); mobile collapses panes; light/dark themes ported from `reference/renewed-ui-original`.
- verify: node --check static/app.js && node tests/ui_smoke.cjs
- note (reorder): owner asked for a full UI redesign before P2-T01, so this runs ahead of the SSE endpoint with the existing P1-T13 polling; T01 then only swaps the transport. Owner direction (2026-09-09): terminal-flat chat (no bubbles), right activity rail, dark-first theme with a light toggle, cozy density. Scope widened from the original three-pane ticket to a full reskin of auth, chat, inbox, imports and settings under one theme. Hard constraints discovered: every element id in static/app.js and the browser suites is a contract; `<link rel="stylesheet" href="/style.css">` must stay byte-identical for tests/recording_ui.cjs snapshots; ui_smoke asserts zero horizontal overflow at 1120px and 390px in light and dark; CSP is style-src 'self' so no inline style attributes anywhere.
- note (done): all done-when met — sidebar sessions/scopes, chat center, right rail with plan, running step with live m:ss elapsed from `started_at`, context meter placeholder showing real tokens-so-far from step receipts, permission prompt above the composer; panes collapse (sidebar drawer ≤920 px, rail overlay ≤1100 px, both toggleable); dark/light themes ported from the reference with a header toggle persisted to localStorage. verify passed: node --check + ui_smoke (14/14) + recording_ui (14/14, needed the same `/scopes` mock fix) + `bash scripts/verify_release.sh` exit 0 (83 Rust, clippy, build, 50 Python contracts, migrations, both HTTP suites). Playwright was not installed on this machine; installed `playwright-core` to /tmp/harness-qa and symlinked it as `playwright`, Chrome via CHROMIUM_PATH. Composer has no auto-grow (CSP blocks inline style); Enter-to-send added per ui.md#keyboard. Assistant markdown rendering intentionally not in this pass.

### P2-T03 · Diff cards with accept/reject and undo
- status: done
- depends: P2-T02
- design: docs/design/ui.md#diff-cards
- files: static/*, src/main.rs (POST /changes/{id}/revert)
- done-when: each `file_changes` row renders as a diff card; revert restores `before` content if the file's current hash equals `after_hash`, else explains why not.
- verify: cargo test --locked changes && node tests/ui_smoke.cjs
- note (done): all done-when met. Every `file_changes` row renders as a diff card (header `path · +A −B · applied HH:MM`, body the unified diff coloured per line by CSS class on spans built from `textContent`, footer Revert or the server's reason). `GET /changes` now enriches each row server-side with `revertable` and a `revert_note`, hashing the file on disk against `after_hash` in `spawn_blocking`, so the footer states a fact instead of the browser guessing. `POST /changes/{id}/revert` restores the previous content behind two proofs: the file must still hash to `after_hash`, and the text rebuilt by reverse-applying the recorded diff must hash to `before_hash`. Otherwise it answers 409 with the reason and writes nothing (already reverted, file changed since, no project root, no recorded previous content). Reverting a file the turn created deletes it again; the undo is single-shot (`FILE_CHANGE_REVERTED` updates only `WHERE reverted_at IS NULL`) and appends a `file_reverted` activity event in the same transaction, so the rail refreshes over the P2-T01 stream. Scope call: the ticket says "accept/reject", but a row only exists after `agent_loop::finish_step` wrote it with `applied=1`, so there is nothing left to accept — an Accept button would be a no-op that lies, and the card offers Revert alone. New `tools::textdiff::reverse` refuses a truncated diff and any hunk whose after-side no longer matches; drift outside the hunks is invisible to a diff by construction, which is exactly why the endpoint proves both hashes. verify passed: `cargo test --locked` 89 passed (4 new), `node --check static/app.js`, ui_smoke 16/16 (new `diff_card_rendered` and `diff_card_revert`, the latter driving a real revert through an XSS-laden diff), recording_ui 15/15, `tests/test_agentic_sql.py` 8/8 with a revert-roundtrip contract, `bash scripts/verify_release.sh` exit 0. Not done: no keyboard shortcut for revert, and cards are not grouped by step.
- note (follow-up, 2026-09-10): closed the unproved create-undo path. The Rust HTTP test now records an `action="create"` row with `before_hash=NULL`, calls the real revert handler, proves the response is `status="deleted"`, the file is absent (not zero-byte), the event is single-shot, and the relisted card says Already reverted. ui_smoke now clicks a second create card and proves the distinct "file this turn created was removed" copy plus the disabled card (17/17); its API is mocked, while the Rust test proves server behavior. `action="delete"` restoration remains forward-compatible code only: no current tool emits delete rows, so that path is not claimed as shipped or end-to-end covered.

## P3 · Context Manager, compaction, repo map

### P3-T01 · Context Manager with per-category budgets
- status: done
- depends: P1-T10
- design: docs/design/context.md
- done-when: window = system rules + tool defs + skills index + repo map + recalled memories + plan + compacted history + recent steps + user message, each with a byte budget; receipt lists included/excluded parts and sizes.
- verify: cargo test --locked context
- note (2026-09-10): added `src/context.rs` with fixed independent UTF-8 source-byte budgets and a deterministic nine-row ledger (`included_parts`, `excluded_parts`, byte totals, state) plus exact first-call provider-array sizes. System rules, current user input, and the configured registry are atomic/fail-closed; optional structured parts are whole-item prefixes; recent history is the newest complete user-led turn suffix. `recording::generate` now uses the manager for tool and chat-only scopes, persists format-v2 `provider_messages` + `provider_tools` + only the included memories before any provider call, and hands that exact tool array to `agent_loop` instead of rebuilding it. Skills/repo-map/compacted-history inputs are typed but intentionally empty until P5-T02/P3-T04/P3-T03. Verified: focused context filter 7 passed; all 93 Rust tests passed; release gate passed (Clippy/build, migrations 001→003, 8 schemas, 51 Python contracts, both HTTP suites). Browser suites were not rerun because no UI asset or UI behavior changed.

### P3-T02 · Tool-result compaction and read cache
- status: done
- depends: P3-T01
- done-when: tool results older than 3 model calls are replaced in the window by `[tool <name> step N, <bytes> bytes, hash <h>; call read again if needed]`; `read` of an unchanged file (same content_hash as an earlier step in the turn) returns a reference.
- verify: cargo test --locked agent_loop::tests::
- note (2026-09-10): current-turn tool bodies stay verbatim for three later model calls, then become stable name/step/byte/hash references in the provider window only. Unchanged repeated reads reference the first durable read step through a path/offset/limit/content-hash cache; audit rows keep full output. Focused and full Rust suites pass.

### P3-T03 · Turn compaction at 70% budget with receipt
- status: done
- depends: P3-T02
- done-when: a `compaction` step summarizes older steps (cheaper model), keeps plan + last 2 tool results verbatim, and the summary is stored as an episodic candidate source.
- verify: cargo test --locked turn_compaction
- note (2026-09-10): observed provider prompt tokens trigger inclusively at 70% of `HARNESS_CONTEXT_TOKENS` (128k default). A text-only `compaction` model role summarizes only the older running replay, preserves the base plan/current prompt and newest two tool results, records source messages/hash/token receipt and usage, and queues the bounded summary as review-only episodic memory. Failure fails the turn rather than using an invented summary.

### P3-T04 · Repo map per scope
- status: done
- depends: P1-T04
- done-when: `.harness/repo_map.txt` (≤ 8 KB) with files and top-level symbols (ctags if present, else heading/regex fallback), refreshed when files change.
- verify: cargo test --locked repo_map
- note (2026-09-10): Git-aware deterministic discovery excludes secrets, symlinks and build/vendor outputs; optional ctags and bounded language fallbacks produce a UTF-8-safe map. A path/size/mtime signature refreshes external changes on the next context build and successful file-changing tools refresh immediately. Generated `.harness/` state is ignored.

## P4 · Memory kinds, hybrid recall, ambient UI

### P4-T01 · Migration 004_memory_kinds.sql
- status: done
- depends: P1-T01
- done-when: `memories.category` and `candidates.category` accept `decision | episodic | procedural` via table rebuild inside one transaction; FTS rebuilt; existing rows preserved.
- verify: python3 tests/test_migrations.py && cargo test --locked storage
- note (2026-09-10): migration 004 rebuilds candidates/memories/revisions transactionally, preserves IDs/rowids/evidence links/history, restores FTS plus triggers, accepts all eight categories, creates constrained `memory_embeddings`, runs foreign-key checks, and advances `user_version` to 4. Populated v3→v4 and constraint tests pass.

### P4-T02 · Local embeddings + hybrid recall
- status: done
- depends: P4-T01
- done-when: embeddings table; bundled deterministic local model (no network/model download at runtime); recall = union(FTS5 top 20, cosine top 20) reranked by scope, recency, prior usefulness; same 6000-byte budget.
- verify: cargo test --locked recall
- note (2026-09-10): selected `harness-local-hash-v1` (256-dimensional normalized word/character/bigram feature hashing) instead of adding an ONNX runtime and binary model to this zero-download local service. Vectors refresh lazily by content hash. Focused tests prove related-spelling cosine recall, scope shadowing, usefulness reranking, vector persistence and the serialized byte cap.

### P4-T03 · Correction detection and decision candidates
- status: done
- depends: P4-T01
- done-when: extraction prompt also proposes `decision` (from plan + user confirmations) and marks user corrections ("no, use X") as high-priority candidates.
- verify: cargo test --locked extraction
- note (2026-09-10): extraction now receives separately labelled current-plan context, but exact user-event substrings remain the only evidence. The prompt proposes decisions/procedures and deterministic correction wording is stored with `priority=high`; plan/assistant/tool text cannot establish a candidate. Three focused tests pass.

### P4-T04 · Inline suggestion tray
- status: done
- depends: P2-T02
- done-when: candidates from the current turn appear under it with Save / Edit / Dismiss; Memory tab becomes "Inbox" for imports only.
- verify: node tests/ui_smoke.cjs
- note (2026-09-10): pending chat/compaction candidates are associated through `chat:{request_id}` or `evidence.request_id` and render beneath their source turn with Save/Edit/Dismiss. Edits validate and update pending values without changing evidence or expected revision. The Inbox reads import-only candidates. Hostile text, conflict recovery, mobile/dark mode and lock clearing pass the 21-check browser suite.

## P5 · Verifier, skills, sub-agents

### P5-T01 · Verifier step
- status: done
- depends: P3-T01
- done-when: after the final answer, a `verification` step (cheaper model) lists file/symbol claims and checks each against tool evidence in the turn; unverified claims produce a badge and an activity event.
- design: verification is an advisory, bounded, text-only model call after the final main-model answer and before `answer_saved`. Its input is the redacted answer plus only durable tool-step evidence from this request. The configured `verification` role falls back to the main turn model. Strict JSON validation rejects unknown evidence step IDs and records provider/parse failures without discarding the otherwise valid answer. The `verified` activity row and UI badge are derived only from the persisted verification step.
- files: `src/memory_agents.rs`, `src/agent_loop.rs`, `src/storage.rs`, `src/agentic_sql.rs`, `static/{index.html,app.js,style.css}`, focused Rust/HTTP/browser tests, and this task journal.
- verify: cargo test --locked verifier && cargo test --locked agent_loop && python3 tests/recording_integration.py && node tests/ui_smoke.cjs && bash scripts/verify_release.sh
- note (2026-09-10): the main answer is produced first and is never rewritten. `Ctx::verify_answer` makes one text-only call in the `verification` role (falling back to the turn model) after `model_call_finished` and before `answer_saved`, sending the redacted answer plus an evidence manifest built only from this request's durable tool steps (at most 24 steps, 12,000 answer chars, and 2,000 argument / 4,000 output chars per step; newest kept, omitted count reported). `parse_verification` rejects tool calls, unknown fields, more than 20 claims, more than 8 evidence ids per claim, more than 10 diagnostics, and any `verified` claim citing a step id outside this turn; providers recognise the audit by the `HARNESS_VERIFICATION_V1` marker in the system prompt. A provider or parse failure fails only the verification step (`verification_failed`, status `unavailable`) and leaves the answer and its receipt intact. `turn_steps()` adds a bounded top-level `verification` projection (20 claims, 8 evidence ids, 10 diagnostics, 500 chars per field, `projection_capped` when trimmed); it is the only source for the header badge and for the `verification_started` / `verified` activity rows. Claim text is model output describing tool output, so the badge writes it with `textContent` plus a title tooltip and the browser suite asserts injected markup stays inert. Two pre-existing fixtures assumed the last provider call was the answer and now route marker calls separately: `tests/mock_provider.py` and `tests/integration_smoke.py`. No migration was needed because `migrations/003_agentic.sql` already allowed the `verification` kind. Verified: `cargo test --locked` 113 passed (4 new), `python3 tests/test_agentic_sql.py` 9 tests, `python3 tests/recording_integration.py` PASS, `bash scripts/verify_release.sh` exit 0, `git diff --check` clean. The browser suites are not part of that gate and were run separately with `NODE_PATH=/tmp/harness-qa/node_modules CHROMIUM_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`: `node tests/ui_smoke.cjs` 24 checks including `verification_badge`, `verification_claim_text_inert` and `verification_model_setting`, and `node tests/recording_ui.cjs` 15 checks.

### P5-T02 · Skills with progressive disclosure
- status: done
- depends: P3-T01
- done-when: `skills/*/SKILL.md` discovered at startup; name+description in the window; `skill` tool loads a body (≤ 16 KB) as a tool result.
- design: deterministic per-scope producer for the existing `skills_index` context category (docs/design/context.md); `skill` tool contract in docs/design/tools.md#skill.
- files: `src/skills.rs`, `src/tools/{mod.rs,skill_tool.rs}`, `src/{main.rs,recording.rs,agent_loop.rs}`, `scripts/gen_tool_schemas.py`, `tools/schemas/skill.json`, `prompts/main_agent.md`, `docs/design/{context.md,tools.md,agentic-turn.md}`, `tests/{test_tool_schemas.py,recording_integration.py}`, and this task journal.
- verify: cargo test --locked skill && cargo test --locked context && python3 tests/test_tool_schemas.py && bash scripts/verify_release.sh
- note (2026-09-10): `src/skills.rs` scans `skills/*/SKILL.md` under the scope root and produces two things: index lines `- name: description (path)` (part id `skill:<name>`) for the existing `skills_index` context category, and `bounded_body`, which drops YAML frontmatter and cuts the body at 16 KiB on a UTF-8 boundary. Bounds: 32 skills, 200-char descriptions, 64-char names, 256 KiB read per file; descriptions pass through `safety::redact`. A name must be one safe path segment, so it can never walk out of `skills/`; only real directories are followed (`symlink_metadata`, no symlink traversal) and `paths::resolve` is still the gate for the read itself. Directories that are unreadable, badly named or past the cap are counted in a trailing `- note:` line rather than silently dropped, and a failed scan degrades to the single part `skills:not_indexed`. The `skill` tool (`src/tools/skill_tool.rs`) takes one `name`, is not side-effecting, and wraps the body in a banner stating that project-provided guidance cannot grant tool permissions, approve a denied command or override system rules; an unknown name is refused with up to 12 available names. Deviation from the done-when wording: discovery runs per turn just before the first provider call, the same shape as `repo_map::load_or_refresh`, not once at process startup, because a scope's `root_path` is configurable at runtime; the cost is one bounded directory scan per turn. Adding a ninth tool broke two magic numbers, both now derived: `src/agent_loop.rs` counts `Registry::standard().schemas()` and `tests/recording_integration.py` counts `tools/schemas/*.json`. This repo ships no `skills/` directory of its own, so the producer is inert here and the end-to-end proof is a `skills/review/SKILL.md` fixture in `tests/recording_integration.py`, which asserts the receipt's `skills_index` carries exactly `skill:review`, that the description reaches the first window and that the body never does. Verified: `cargo test --locked` 121 passed (9 new), `python3 tests/test_tool_schemas.py` 9 schemas, `python3 tests/test_agentic_sql.py` 9 tests, `bash scripts/verify_release.sh` exit 0 (52 Python contracts, migrations 001→004, 9 tool schemas, both mock-provider HTTP suites), `git diff --check` clean. Clippy reports nothing in either new file; the remaining warnings are pre-existing in `context.rs`, `repo_map.rs`, `storage.rs`, `memory_agents.rs` and `tools/mod.rs`. The browser suites were not run because no UI asset or behaviour changed.

### P5-T03 · task tool (read-only explore sub-agent)
- status: done
- depends: P3-T01
- done-when: sub-agent has its own step list under the same request (parent_step_id), read/grep/glob only, returns ≤ 1 KB summary + file refs; budgets inherited and shared.
- design: `task` is registered like any other tool so it appears in the one immutable definition array, but the loop intercepts the call before `Registry::invoke`, because a sub-agent needs the provider and `Tool::run` is synchronous and filesystem-bound. The parent opens one `subagent` step whose `parent_step_id` is the `task` tool-call step; the sub-agent's own model calls and tool calls hang off that step under the same `request_id`, so the turn stays one ordered step list. The sub-agent is offered only `read`, `grep` and `glob`, so nothing it can call is side-effecting and no approval can ever be raised inside it. Steps, tool bytes and the wall deadline are the parent's: the sub-agent draws from what is left and reports what it spent. What returns to the parent model is a bounded summary (≤ 1 KB) plus file references, never the sub-agent's transcript.
- files: `src/subagent.rs`, `src/tools/{mod.rs,task_tool.rs}`, `src/{main.rs,agent_loop.rs,agentic_sql.rs}`, `scripts/gen_tool_schemas.py`, `tools/schemas/task.json`, `prompts/main_agent.md`, `docs/design/{tools.md,agentic-turn.md}`, `tests/{test_tool_schemas.py,test_agentic_sql.py,recording_integration.py}`, and this task journal.
- verify: cargo test --locked subagent && cargo test --locked agent_loop && python3 tests/test_tool_schemas.py && python3 tests/test_agentic_sql.py && bash scripts/verify_release.sh
- note (2026-09-10): `task` is the tenth registered tool, so it rides in the one immutable definition array, but the dispatch loop branches on it before `Registry::invoke`; `Task::run` exists only to return `internal_error` if it is ever reached directly. `src/subagent.rs` is contract-only — the allow-list (`TOOLS = [read, grep, glob]`, deliberately excluding `task` so a sub-agent cannot spawn one), the bounds (8 model calls, 1000-char summary, 12 paths, 80-char description, 2000-char prompt), `parse`, `definitions`, `messages`, `record_file`, `Stop`, `Report`, `report_content` and `label`. Deviation from the design's implication: the orchestration lives in `agent_loop::run_task`/`refuse_delegation`, not in `subagent.rs`, because only the loop owns the provider, the step writer and the budget counters; a second deviation is that the planned `bounded_summary` helper collapsed into `report_content`, which is the only place a summary is ever cut. Nesting: `STEP_BEGIN` now carries `parent_step_id` at `?3` (NULL for every step the main loop owns), `begin_step` delegates to a private `insert_step` and the new `begin_child_step` opens the `subagent` step under the `task` tool-call step and the sub-agent's own `model_call`/`tool_call` steps under that, so one request stays one ordered `seq` list that still reads back as a tree; no migration was needed because 003 already had the column and the `subagent` kind. Read-only by construction rather than by permission: the allow-list is re-checked when the model's call comes back, and a name outside it is refused as `unknown_tool` without ever reaching the registry, so nothing inside a delegation can raise an approval even in `auto_all`. The sub-agent gets a fresh context (its own system prompt plus one `Exploration: <description>\n\n<prompt>` user message), never the parent's history, and only its final message returns; the parent sees `sub-agent report (N model calls, M tool calls[; why it stopped])`, the capped summary and the `files read:` list. Steps, tool bytes and the wall deadline are the parent's remaining budget, and what the delegation spent is added back to both counters when it returns. Verified: `cargo test --locked` 130 passed (9 new: 2 in `tools::task_tool`, 6 in `subagent`, and one end-to-end `agent_loop` test that asserts the nine-row step tree, the `parent_step_id` shape, `subagent_started`/`subagent_finished`, a `write` refused as `unknown_tool` with `notes.md` untouched, and that the report — not the transcript — reaches the parent), `python3 tests/test_tool_schemas.py` 10 schemas / 10,293 bytes, `python3 tests/test_agentic_sql.py` 10 tests, `bash scripts/verify_release.sh` exit 0, `git diff --check` clean. `tests/recording_integration.py` proves the same path over HTTP: a `delegate the search` scenario plus a second scenario keyed on the sub-agent's own exploration message (the mock provider selects on an exact user-message match, so a sub-agent can never consume the parent's scripted replies), asserting the step tree with parent `seq`s, `subagent_started < subagent_finished < answer_saved`, that the sub-agent's request offers exactly `read, grep, glob` with a two-message window, and that `deny.md` is untouched under `auto_all`. Two Clippy warnings this task introduced were cleared (`filter(..).next_back()` → `rfind`, and the unused-but-documented `STEPS_OF_PARENT` statement, which now carries `#[allow(dead_code)]` and a comment saying the Python contract test is what executes it); the rest are pre-existing. Browser suites not run: no UI asset or behaviour changed.

## P6 · Structural tooling

### P6-T01 · ast_edit via ast-grep
- status: done
- depends: P3-T01
- done-when: `ast_edit` rewrites one Rust file from an ast-grep pattern plus a replacement only while the required `content_hash` from `read` is current. It goes through the same planner / permission / `file_changes` path as `edit`, so the diff is approved while disk is unchanged, then the applied change is recorded and stays revertable. A stale hash fails `stale_anchor`, zero matches fail `no_match`, and more matches than the cap fail `ambiguous_match`, all without touching the file.
- design: contract in docs/design/tools.md#ast_edit. Matching runs in-process on the `ast-grep-core` + `ast-grep-language` crates rather than shelling out to `sg`, which is not installed here and would make the tool depend on the host. The planner resolves the path, checks the latest `read` hash, infers the language from the extension (Rust only in this task; the language table is the seam for the rest), collects the matches, refuses at zero or above `max_matches` (default 20), rewrites every match to build the after-text, then reuses `textdiff` for the unified diff, `before_hash`/`after_hash` and the +/− counts. Everything downstream is unchanged: side-effecting like `edit`, the same pre-write permission payload, atomic tmp-then-rename write, applied file-change artifact, `diagnostics_cmd` append, and `paths::resolve` disk gate.
- files: `Cargo.{toml,lock}`, `src/tools/{mod.rs,edit_tools.rs,ast_edit_tool.rs,meta_tools.rs}`, `scripts/gen_tool_schemas.py`, `tools/schemas/ast_edit.json`, `prompts/main_agent.md`, `docs/design/tools.md`, `tests/{test_tool_schemas.py,recording_integration.py}`, and this task journal.
- verify: cargo test --locked ast_edit && python3 tests/test_tool_schemas.py && python3 tests/recording_integration.py && bash scripts/verify_release.sh
- note (2026-09-10): `ast_edit` is the eleventh registered tool. It parses and rewrites one existing Rust file in-process with `ast-grep-core` plus only the `tree-sitter-rust` language feature, then reuses `edit_tools::{describe,apply,payload}` for the bounded diff, atomic write, diagnostics and durable `Artifact::FileChange`. The required eight-hex `content_hash` from `read` is checked both while constructing the permission payload and again after approval; a pending approval therefore cannot drift into a different structural edit if the file changes. Rust-only scope, 512 KiB parse cap, default 20 / maximum 200 matches, `no_match`, `ambiguous_match`, invalid-pattern/language refusals and `stale_anchor` are documented and covered without writing on failure. The old comments that claimed the loop pre-created `file_changes(applied=0)` were corrected: the diff lives in the pending permission row while disk is untouched, and a successful approved run returns the artifact that is recorded as applied and revertable. The generated schema, registry schema-order assertion and main-agent guidance now include `ast_edit` and its hash requirement. Unit coverage has six `ast_edit` tests; the real HTTP fixture proves `read → pending ast_edit → approve → write`, checks the pre-write diff/hashes/counts and empty changes feed, then checks both rewrites and one applied/revertable change row. Verification: focused tests, schema validation, Python compilation, direct HTTP integration and `git diff --check` passed; final `bash scripts/verify_release.sh` exited 0 with 136 Rust tests, Clippy/build, migrations 001→004, 11 schemas, 53 Python contracts and both mock-provider HTTP suites. Browser suites were not run because no UI asset or behavior changed. Next is P6-T02 (`lsp`); nothing blocks it.

### P6-T02 · lsp tool (diagnostics, references, rename)
- status: done
- depends: P6-T01
- done-when: one `lsp` tool asks a local language server for diagnostics or symbol references without approval, and renames a symbol across bounded project files through the ordinary pre-write permission and durable `file_changes` path. Model-facing positions are 1-based; every server-returned path is rechecked by `paths::resolve`. Rename requires current `content_hash` values for every file it would edit, refuses stale or unlisted files without writing, previews one bounded multi-file diff, applies atomically per file with rollback on failure, and records one applied/revertable artifact per changed file.
- design: docs/design/tools.md#lsp. A fresh stdio language-server child is used per call. The command is fixed by file extension (Rust → `rust-analyzer`, C/C++ → `clangd`), never supplied by the model; protocol frames, results and workspace edits are bounded. `Tool::side_effecting_for(args)` lets only `operation=rename` enter the permission gate while diagnostics/references remain read-only.
- files: `Cargo.{toml,lock}`, `src/tools/{mod.rs,lsp_tool.rs,meta_tools.rs}`, `scripts/gen_tool_schemas.py`, `tools/schemas/lsp.json`, `prompts/main_agent.md`, `docs/design/tools.md`, `tests/{test_tool_schemas.py,recording_integration.py}`, and this task journal.
- verify: cargo test --locked lsp && python3 tests/test_tool_schemas.py && python3 tests/recording_integration.py && bash scripts/verify_release.sh
- note (2026-09-10): `lsp` is the twelfth ordinary tool. A fixed extension map starts a fresh bounded stdio server per call (`.rs` → `rust-analyzer`, C/C++ → `clangd`) with no model-controlled command, a cleared environment, 4 MiB frame cap, 20-second default / 60-second maximum deadline and forced child cleanup. Diagnostics and references are read-only even in `ask`; model positions are 1-based Unicode characters and become zero-based UTF-16 internally, while references return current eight-hex hashes. Rename alone uses argument-aware approval. It requires hashes for every possible edited file, re-queries after approval, resolves every returned URI, caps work at 20 existing in-root text files / 200 non-overlapping edits / 2 MiB per file / 64 KiB combined preview, validates all hashes before the first write, rolls prior files back if a later atomic write fails, and returns one durable artifact per file. Seven focused tests cover positions, framing, caps, missing servers, resource-op refusal, stale/unlisted files, pre-write revalidation, multi-file apply and permission routing. A deterministic fake stdio server in the real HTTP gate proves diagnostics/references create no permission, two-file rename leaves disk and `/changes` untouched before approval, then writes both files and records two applied/revertable changes plus two activity events. Final `bash scripts/verify_release.sh` exited 0 with 143 Rust tests, Clippy/build, migrations 001→004, 12 schemas, 53 Python contracts and both mock-provider HTTP suites. Browser suites were not run because no UI changed. The active machine still lacks the actual `rust-analyzer` component (the rustup shim reports it missing); this is surfaced as bounded `lsp_unavailable`, deterministic tests do not depend on it, and `/usr/bin/clangd` is available. Next is P6-T03 (`browser`).
### P6-T03 · browser tool via CDP
- status: done
- depends: P6-T02
- done-when: one `browser` tool can open an HTTP(S) page, return a bounded accessibility snapshot, and reuse that page for snapshot/click/type/press/close calls during the same agent turn. Every interaction is anchored to the current `snapshot_id` and a browser-issued node ref, stale pages or refs fail before input is dispatched, and click/type/press use the ordinary permission gate while open/snapshot/close remain read-only. The model cannot provide a CDP endpoint, browser command, selector, or JavaScript.
- design: docs/design/tools.md#browser. The turn-owned registry holds one mutex-serialized page-target CDP session. It connects only to an operator-configured loopback `HARNESS_CDP_URL` or starts a fixed discovered Chrome/Chromium binary with an isolated temporary profile and loopback debugging port; owned process groups and profiles are removed when the turn ends. Protocol messages, waits, page text, nodes, input and output are bounded. Deterministic fake-CDP coverage must not require a machine browser.
- files: `Cargo.{toml,lock}`, `src/tools/{mod.rs,browser_tool.rs,meta_tools.rs}`, `scripts/gen_tool_schemas.py`, `tools/schemas/browser.json`, `prompts/main_agent.md`, `docs/design/tools.md`, `tests/{test_tool_schemas.py,recording_integration.py}`, and this task journal.
- verify: cargo test --locked browser && python3 tests/test_tool_schemas.py && python3 tests/recording_integration.py && bash scripts/verify_release.sh
- note (done 2026-09-10): Added the turn-scoped CDP browser tool with bounded accessibility snapshots and anchored browser-issued refs. The implementation uses loopback-only operator CDP or a fixed isolated browser path, never model-provided selectors, JavaScript, endpoints, or commands. `open`/`snapshot`/`close` stay read-only while click/type/press use the existing permission gate. Added a deterministic standard-library fake CDP WebSocket fixture and browser-focused tests: 5 passed. The first HTTP gate exposed the new tool definition exceeding the existing context budget; increased `tool_definitions` from 12 KiB to 16 KiB while keeping the category bounded. Final gate: `bash scripts/verify_release.sh` exited 0 with 148 Rust tests, migrations 001→004, 13 schemas, 53 Python contracts, and both mock-provider HTTP suites. Browser suites remain separate because they require a UI runtime.

---

## P7 · Durable streaming continuation

### P7-T01 · Persisted generation stream foundation
- status: doing
- depends: P6-T03
- design: docs/ROADMAP.md#p4
- files: src/streaming.rs, src/storage.rs, src/main.rs, static/app.js, migrations/*
- done-when: Generation output can stream through an authenticated transport where every emitted event is persisted before becoming visible to the client. Events have stable ordering, resumable cursors, and explicit completed/interrupted/failed states.
- verify: cargo test --locked streaming && python3 tests/test_streaming_contracts.py && bash scripts/verify_release.sh
- note (started 2026-09-10): Existing `/activity/stream` provides durable agent activity replay. P7-T01 extends this pattern to generation output events persisted before client delivery.

### P7-T02 · Stream recovery and boundary safety
- status: todo
- depends: P7-T01
- design: docs/ROADMAP.md#p4
- files: src/streaming.rs, tests/*stream*
- done-when: Reconnects resume from the last durable cursor without duplicate generation. Split UTF-8 frames, provider failures, disconnects, and interrupted generations are handled without leaking unredacted partial secrets.
- verify: cargo test --locked streaming && python3 tests/test_streaming_contracts.py

### P7-T03 · Frontend durable stream integration
- status: todo
- depends: P7-T01
- design: docs/ROADMAP.md#p4
- files: static/app.js, static/*.css, src/main.rs
- done-when: Chat UI consumes durable stream events instead of fake typing animation, preserves ordering after reconnect, and clearly distinguishes completed, interrupted, and failed responses.
- verify: node --check static/app.js && bash scripts/verify_release.sh

---

## Ideas parking lot (not scheduled)
- Memory branches per project; memory rehearsal (retrieval diff before approval) — from ROADMAP P6.
- Portable continuation packet export.
- Cost dashboard per scope/day.
- Voice input.
