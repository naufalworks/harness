# TASKS — ordered backlog with stable IDs

How to use: pick the highest-priority `todo` whose `depends:` are all `done`; use the
earlier task ID as the tie-breaker. Parallel tasks require `parallel: yes`, different
lanes, disjoint files, separate branches/worktrees, and serialized integration.
Change status in place. Never renumber or delete a task; mark it `dropped` with a reason.

Status values: `todo` | `doing` | `done` | `needs-verify` | `blocked` | `dropped`

Each new task has: `status`, `priority`, `lane`, `parallel`, `depends`, `design`,
`files`, `done-when`, and `verify`. Every state transition also updates PROGRESS.md.

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
- status: done
- depends: P6-T03
- design: docs/ROADMAP.md#p4
- files: src/memory_agents.rs, src/storage.rs, src/main.rs, static/app.js, migrations/*
- done-when: Generation output can stream through an authenticated transport where every emitted event is persisted before becoming visible to the client. Events have stable ordering, resumable cursors, and explicit completed/interrupted/failed states.
- verify: cargo test --locked streaming && bash scripts/verify_release.sh
- note (started 2026-09-10): Existing `/activity/stream` provides durable agent activity replay. P7-T01 extends this pattern to generation output events persisted before client delivery.
- note (closed 2026-09-11): Closed by its landed subtasks rather than by new code. Generation events are persisted before delivery and served over authenticated polling and SSE with `request_id` attribution, ordered resumable cursors, and explicit complete/failed/interrupted terminal states (`P7-T02c`), and the chat UI consumes that feed (`P7-T03`). Verification evidence for this tree is the gate run at `b6a5fb1`: `bash scripts/verify_release.sh` exit 0 with 161 Rust tests, Clippy/build, 53 Python contracts, migration chain through 005, both local HTTP suites, and frontend syntax. Publication before DONE is deliberately out of scope here and is tracked as `P7-T05`.

### P7-T02 · Stream recovery and boundary safety
- status: done
- depends: P7-T01
- design: docs/ROADMAP.md#p4
- files: src/memory_agents.rs, tests/*stream*
- done-when: Reconnects resume from the last durable cursor without duplicate generation. Split UTF-8 frames, provider failures, disconnects, and interrupted generations are handled without leaking unredacted partial secrets.
- verify: cargo test --locked streaming

- note (2026-09-10): Reopened after inspection. The inherited streaming module/test paths do not exist; verification now names executable Rust tests. Full P7 still needs incremental safe publication, transport/reconnect coverage, request identification in the session feed, and restart-event deduplication.
- note (closed 2026-09-11): Closed by `P7-T02a` (bounded byte framing, strict UTF-8, whole-answer redaction, one atomic publication), `P7-T02b` (interruption recorded exactly once per newly interrupted turn, so repeated recovery cannot grow the feed), and `P7-T02c` (multi-turn attribution, cursor paging, SSE resume without duplicates, wrong-session isolation, authentication bounds, terminal failure events). Same `b6a5fb1` gate evidence as `P7-T01`. The one boundary question left — publishing before DONE without ever exposing a secret split across provider chunks — is `P7-T05`, not part of this task.

### P7-T02a · Repair provider boundary and atomic publication
- status: done
- Goal: Restore trustworthy provider-stream ingestion and durable answer publication.
- Objective: Preserve Unicode, reject incomplete/error responses, redact before publication, and publish one answer atomically.
- Reason: New provider, transaction and duplicate-event regressions reproduced inherited failures.
- Expected Result: One bounded redacted answer and completion event commit with the receipt, or none commit.
- Success Criteria: Streaming regressions, fallback duplicate assertion, and release gate pass.
- Priority: correctness/security, before frontend streaming.
- Task ID: P7-T02a
- Task Description: Repair provider parsing and remove duplicate asynchronous publication.
- Expected Outcome: No split-secret delivery, corrupt UTF-8, early role-frame completion, duplicate answer, or premature completed event.
- Research Needed: Inspect provider, redaction, generation writer, and recording transaction contracts.
- Implementation Plan: Reproduce failures; use bounded byte framing and whole-answer redaction; publish inside complete_recording; verify and journal.
- Validation Method: cargo test --locked streaming; cargo test --locked a_provider_without_tool_support; bash scripts/verify_release.sh; git diff --check.
- Result: Eight focused streaming tests and the fallback duplicate assertion pass. Full release gate exits 0: 160 Rust tests, Clippy/build, 53 Python tests, both HTTP suites, and frontend syntax. git diff --check passes. Browser UI suites were not run; no UI changed.
- Status: done
- Decision: Buffer the bounded answer until DONE and redact as a whole. Safe incremental display remains pending.

### P7-T02b · Idempotent generation recovery
- status: done
- Goal: Preserve a stable generation event history across restarts.
- Objective: Record interruption exactly once for each newly interrupted turn.
- Reason: recover selects every historical interrupted receipt after updating states, duplicating events and growing the log on every startup.
- Expected Result: Repeated recovery does not add events for terminal turns.
- Success Criteria: Regression proves identical generation cursor after repeated recovery; release gate passes.
- Priority: high correctness; before frontend integration.
- Task ID: P7-T02b
- Task Description: Limit generation interruption inserts to receipts transitioning from generating.
- Expected Outcome: Stable replay without rewriting existing history or replaying provider/tool calls.
- Research Needed: Compare recording/activity recovery ordering and generation SQL.
- Implementation Plan: Reproduce duplication, insert before receipt state update using generating predicate, verify and commit.
- Validation Method: cargo test --locked restart; bash scripts/verify_release.sh.
- Result: Recovery now inserts a generation interruption before transitioning only receipts still in `generating`; a second recovery leaves generation and recording cursors unchanged. `cargo test --locked restart` passed (2 tests), the full release gate passed (160 Rust tests, Clippy/build, 53 Python contracts, both HTTP suites, frontend syntax), and `git diff --check` passed. Browser suites were not run because no UI changed.
- Status: done

### P7-T02c · Attribute and verify generation replay
- status: done
- depends: P7-T02b
- design: docs/ROADMAP.md#p4
- files: src/storage.rs, src/main.rs, src/recording_tests.rs, tests/recording_integration.py
- Goal: Make generation replay usable across multiple turns.
- Objective: Include request identifiers and verify authenticated resumable transport.
- Reason: generation_since drops request_id from rows even though one session contains multiple turns.
- Expected Result: Every event names its request; replay remains ordered and session-scoped.
- Success Criteria: Multi-turn attribution, cursor paging, SSE replay, authorization and error-state tests pass.
- Priority: high correctness.
- Task ID: P7-T02c
- Task Description: Add request_id to generation projections and cover transport contracts.
- Expected Outcome: Clients can render each event under the correct message.
- Research Needed: Inspect storage projection and existing activity-stream HTTP fixture.
- Implementation Plan: Add a failing two-turn attribution assertion; include `request_id` in the bounded generation projection; prove cursor paging and SSE resume without duplicates; cover wrong-session isolation, authentication, interruption, and failure events; run the release gate before marking done.
- Validation Method: cargo test --locked streaming; python3 tests/recording_integration.py; bash scripts/verify_release.sh.
- Result: Generation polling and authenticated SSE now project `request_id`; native and compiled-server tests prove two-turn attribution, ordered cursor resume without duplicates, wrong-session isolation, validation/authentication bounds, and completed, interrupted, and failed terminal events. `bash scripts/verify_release.sh` passed with 161 Rust tests, Clippy/build, 53 Python contracts, both local HTTP suites, frontend syntax, and `git diff --check`.
- Status: done

### P7-T04 · Correct migration verification reporting
- status: done
- depends: P7-T02c
- design: docs/PLAN.md#p7--durable-generation-continuation
- files: tests/test_migrations.py, docs/PROGRESS.md
- Goal: Keep release evidence accurate.
- Objective: Report the migration chain actually tested.
- Reason: tests/test_migrations.py applies 005 and checks version 5 but prints version 4; the prior progress entry repeated that stale output.
- Expected Result: Release output and progress record accurately describe migration coverage.
- Success Criteria: python3 tests/test_migrations.py prints the actual latest version.
- Priority: documentation/test reliability.
- Task ID: P7-T04
- Task Description: Derive migration success summary from CHAIN and correct the prior journal claim.
- Expected Outcome: No stale hard-coded migration version in success output.
- Research Needed: Compare CHAIN, full-chain assertions and output.
- Implementation Plan: Derive output, run migration test, correct journal and commit.
- Validation Method: python3 tests/test_migrations.py; git diff --check.
- Result: Migration success output now derives its displayed chain and latest user version from `CHAIN`; the full-chain assertion uses the same derived latest version. `python3 tests/test_migrations.py` reports `001 -> 002 -> 003 -> 004 -> 005, user_version=5`, and `git diff --check` passes.
- Status: done

### P7-T03 · Frontend durable stream integration
- status: done
- depends: P7-T01, P7-T02c, P7-T04
- design: docs/ROADMAP.md#p4
- files: static/app.js, static/style.css, tests/recording_ui.cjs
- done-when: Chat UI consumes durable stream events instead of fake typing animation, preserves ordering after reconnect, and clearly distinguishes completed, interrupted, and failed responses.
- verify: node --check static/app.js && bash scripts/verify_release.sh
- note (done): The chat now renders live answers only from authenticated `/generation/stream` or its `/generation` fallback, never from the receipt response. The pending request stores the last handled generation cursor for reload/reconnect, rows are ignored unless their sequence advances, and completed content is painted atomically from the persisted event. Failed and interrupted turns render explicit durable terminal cards in live and reopened history. The mocked browser fixture now covers generation subscription, persisted-cursor resume, durable complete content, and both terminal failures. `node --check static/app.js`, `node --check tests/recording_ui.cjs`, `git diff --check`, and the full release gate passed (161 Rust tests, 53 Python contracts, both local HTTP suites, migration 005, schemas, build/Clippy, and frontend syntax). The browser fixture itself could not run in this checkout because the Playwright module is not installed.

### P7-T05 · Safe incremental generation publication
- status: done
- depends: P7-T02a, P7-T03
- design: docs/design/incremental-publication.md
- files: src/memory_agents.rs, src/safety.rs, src/recording.rs, src/recording_tests.rs, static/app.js, tests/test_incremental_publication.py
- done-when: Generation text can be published before DONE without a redactable pattern split across provider chunks ever becoming visible. A boundary-aware redactor withholds any tail that could still complete a pattern, the UI renders the incremental text in order, and a regression proves a secret split across two chunks is never published early and is never published unredacted afterwards.
- verify: cargo test --locked streaming && cargo test --locked redact && bash scripts/verify_release.sh
- note (2026-09-11, done): Implemented. `safety::StreamRedactor` (line holdback, `in_key` carry-over, `emitted`-guarded join, `finish`, `pending_len`, tail discarded when dropped) shares `classify_line` with `redact` so the two cannot drift. `GenerationSink` is now async (`BoxFuture` + `text()`/`usage()` accessors); `consume_stream_response` publishes completed lines as they arrive and flushes the final unterminated line only at `[DONE]`. New `recording::RecordingGenerationSink` commits one `chunk` row *before* delivery and reports `generation_stream_save_failed` through `fail_recording`; `complete_recording` now writes the terminal `completed` row only, so the answer is no longer duplicated. `static/app.js` appends `chunk` content in `seq` order via `textContent`. Tests updated for the new model (`recording_tests.rs`, `recording_integration.py`) and added: chunking equivalence over every single cut and char-wise, split-secret holdback, no-blank-line on dropped key body. Gate: `cargo test --locked` 165 passed, `streaming` 9 passed, `redact` 5 passed, `tests/test_incremental_publication.py` 7 OK, `verify_release.sh` PASS, `node --check` OK.
- note (opened 2026-09-11): Carries the deferred `P7-T02a` decision. Whole-answer buffering stays the active boundary until this task proves incremental safety; no other task may relax it as a side effect. Needs a host with cargo.
- note (2026-09-11, design ready): Design landed at `docs/design/incremental-publication.md`. Finding that shapes the work: `safety::redact` decides per line and replaces a matched line whole, so a pattern can still be completed by later bytes of the same line and no partial line may be published; intra-line masking is rejected because durable publication cannot be retracted. Plan is `safety::StreamRedactor` (line holdback, `in_key` carry-over, bounded pending tail, tail discarded on failure) plus one `chunk` row per publication, with no event-vocabulary or migration change. `tests/test_incremental_publication.py` is the executable spec: it derives markers and thresholds from `src/safety.rs` so it cannot drift, and proves chunk-split equivalence, monotone-prefix publication, holdback and tail discard over split-secret, private-key, CRLF, Unicode, token-shape and no-newline fixtures (7 tests; Python suite 53 → 60, OK). Still open: the Rust implementation and its cargo gate.

### P7-T06 · Make the browser suites executable
- status: done
- depends: P7-T03
- blocked-by: the MCP host has `node`, `python3` and `git` but no `npm`, `npx` or `playwright` module, so the fixtures have no browser runtime (`require('playwright')` → `MODULE_NOT_FOUND`).
- files: tests/recording_ui.cjs, tests/ui_smoke.cjs, scripts/verify_release.sh, README.md
- done-when: `node tests/recording_ui.cjs` and `node tests/ui_smoke.cjs` run to completion on a documented setup, and that setup lives in the repo instead of in journal entries.
- verify: scripts/verify_browser.sh
- note (resolved 2026-09-12): Runtime installed and both fixtures pass. `npm` 9.2.0 via apt, `playwright` 1.63.0 with Chromium 1243 and its system libraries. The setup now lives in the repo: `scripts/setup_browser_tests.sh` installs it, `scripts/verify_browser.sh` runs both fixtures after resolving the browser from Playwright rather than a hard-coded path, and README documents both. `scripts/verify_release.sh` runs the suites when `node_modules` is present and skips them with a notice otherwise, so the gate stays green on a bare host. Evidence: `node tests/recording_ui.cjs` -> passed, 18 checks; `node tests/ui_smoke.cjs` -> passed, 24 checks; both with `CHROMIUM_PATH` resolved by Playwright and no `/usr/local/bin/chromium` symlink present.
- note (opened 2026-09-11): Owner action. Either install a runtime (`npm install -D playwright && npx playwright install chromium`) or reuse the `P5-T01` pattern `NODE_PATH=<dir>/node_modules CHROMIUM_PATH=<browser> node tests/recording_ui.cjs`. Both fixtures pass `node --check`, so this is a runtime gap, not a code defect, and the release gate deliberately excludes browser suites.

### P7-T07 · Require a JSON object body on every JSON route
- status: done
- depends: P7-T04
- files: src/main.rs (`JsonBody`, `json_content_type`, `reject_body`), src/storage.rs (`ScopePatch::is_empty`, `upsert_scope`), tests/integration_smoke.py, docs/design/agentic-turn.md
- done-when: a top-level JSON array or scalar is refused with `400 application/json` on every served route, and a body that names no field cannot move `updated_at` on a row that already exists.
- verify: scripts/verify_release.sh
- note (2026-09-12, done): found while probing the routes converted in the previous session — `POST /scopes/global` with a body of `[]` answered `200` with the stored row instead of `400`. serde's derive accepts the sequence form of a struct as well as the map form, and every field of `ScopePatch` is `#[serde(default)]`, so a zero-length array is a valid all-defaults patch; `#[serde(deny_unknown_fields)]` cannot catch it because an array carries no field names to reject. The other five converted routes were refusing `[]` only by luck: their target types have required fields, so the sequence length did not match. Fixed in both layers. (1) `JsonBody` no longer delegates to `Json<T>`'s extractor: it checks the content type itself (buffering bypasses axum's own check), buffers with `Bytes`, refuses any body whose first non-whitespace byte is not `{`, then parses with `Json::<T>::from_bytes`; the bound moved from `Json<T>: FromRequest<S, Rejection = JsonRejection>` to `T: DeserializeOwned`, so all eight extractor sites and any target type added later inherit the rule. (2) `upsert_scope` returns the stored row untouched when `ScopePatch::is_empty()`, so a patch that states no intent cannot move `updated_at`; a scope that does not exist yet is still created. Rejection wording is now `Request body was not accepted. Reload this tab, then send again`, since it is no longer only about chat messages. Evidence: `bash scripts/verify_release.sh` → exit 0, 169 Rust tests (new `non_object_json_bodies_are_refused_with_json`, `an_empty_patch_still_creates_a_missing_scope`, `an_empty_patch_leaves_the_stored_row_untouched`), 60 Python tests, both mock-provider HTTP suites, `recording_ui.cjs` 18 checks, `ui_smoke.cjs` 24 checks; `tests/integration_smoke.py` now asserts the object rule and that a `{}` post leaves `updated_at` alone.

### P7-T08 · Deploy the artifact the unit actually runs
- status: done
- depends: P7-T07
- files: scripts/deploy.sh, scripts/verify_release.sh, README.md
- done-when: deploying cannot report success while the live process runs a binary other than the one just built, and the profile the unit starts is built by a script in the repo.
- verify: bash scripts/deploy.sh
- note (2026-09-12, done): found by probing after a green gate and a successful-looking `systemctl restart` — the live service still answered `POST /scopes/global` with `[]` as `200` and the pre-fix wording. The unit starts `target/release/harness`, while `scripts/verify_release.sh` runs `cargo build --locked` (debug), so no script in the repo ever built the artifact systemd starts; the restart relaunched a binary from `02:16:43` that predated the fix. The stale-binary incident earlier in the session was this same gap, misread as a branch problem. `scripts/deploy.sh` now owns deployment: it blocks unless `ExecStart` names the binary it is about to build, builds `--release`, restarts the unit, waits for `active`, compares the md5 of `/proc/<MainPID>/exe` with the binary just built, and smoke-tests that `GET /scopes` answers and that a non-object body is refused with `400` (neither request writes). Evidence: `deployed 5e48f1d to harness: pid 64619, release md5 bb0decb1934012666c827bc6435e6a2e, API answering, non-object body refused with 400`, then live probes where `[]`, `"ask"` and an empty body each return `400 application/json` and `{}` leaves `updated_at` at `2026-09-12T03:02:37.439006652`. Not done: the binary carries no version stamp, so identity is proven by md5 rather than by commit.

---

## Ideas parking lot (not scheduled)
- Memory branches per project; memory rehearsal (retrieval diff before approval) — from ROADMAP P6.
- Portable continuation packet export.
- Cost dashboard per scope/day.
- Voice input.

## P8 · Causal observability

### P8-T01 · Define durable provenance edges
- status: done
- depends: P7-T08
- design: docs/design/causal-observability.md
- files: migrations/*, src/agentic_sql.rs, src/storage.rs, docs/design/causal-observability.md
- done-when: a bounded typed edge can reference existing evidence, step, permission, mutation, memory, and recovery rows; invalid references and unsupported edge kinds fail closed; no chain-of-thought is stored.
- verify: python3 tests/test_migrations.py && cargo test --locked storage
- note (2026-09-12, done): migration 006 adds a request-scoped graph capped at 2,000 typed edges over same-scope evidence/memory and same-request step/permission/mutation/recovery rows. A polymorphic-reference trigger rejects missing or mismatched endpoints, delete guards prevent durable edges from becoming orphans, and the storage API accepts only the seven documented relations with no freeform reasoning payload. The exact verify command passed with migration chain 001→006 and 10 storage tests; all 11 agentic SQL contract tests also passed. Existing unrelated Rust formatting drift remains outside this task.

### P8-T02 · Build the causal incident read model
- status: done
- depends: P8-T01
- design: docs/design/causal-observability.md
- files: src/main.rs, src/storage.rs, static/app.js, tests/recording_integration.py
- done-when: an authenticated read-only endpoint returns an incident graph with upstream/downstream traversal, unknown provenance markers, bounded nodes/edges, and the earliest known causal break.
- verify: python3 tests/recording_integration.py && node --check static/app.js
- note (2026-09-12, done): added authenticated GET `/chat/requests/{id}/incident`. The bounded projection returns request, step, permission, mutation, and recovery nodes; typed edges with explicit upstream/downstream adjacency; unknown provenance markers for unlinked durable rows; and the earliest known failure, denial, or recovery break. The HTTP integration gate proves a denied write is navigable without implying hidden reasoning. Verified with `python3 tests/recording_integration.py && node --check static/app.js`; the integration run passed after rebuilding the debug binary.

### P8-T03 · Prove cross-component failure attribution
- status: done
- depends: P8-T02
- design: docs/design/causal-observability.md
- files: tests/browser_e2e_failure.cjs, tests/recording_integration.py, docs/qa/e2e-verification-report.md
- done-when: real denial, stale-anchor, and crash-recovery runs produce navigable causal graphs; tests distinguish missing evidence from contradiction and assert no duplicate side effect.
- verify: scripts/verify_e2e.sh
- note (2026-09-12, done): runtime recording now links tool steps to permission decisions, applied mutations, stale-anchor contradictions, and exact restart recovery events. The denial browser fixture proves a denied permission triggers its failed step without an authorization edge; the HTTP integration fixture links the current read to the stale edit with `contradicts` while retaining explicit unknown markers; the SIGKILL fixture links the interrupted bash step to recovery and proves one edit, one bash step, and no replay. `scripts/verify_e2e.sh` passed against real Chromium, axum, SQLite, filesystem, and loopback provider.

### P8-T04 · Ship the interactive incident graph
- status: done
- depends: P8-T03
- design: docs/design/causal-observability.md
- files: static/*, docs/qa/e2e-verification-report.md
- done-when: the dashboard can filter an incident, select a node, show its evidence and state transition, and link back to the original durable rows without implying hidden reasoning access.
- verify: scripts/verify_browser.sh && node --check static/app.js
- note (2026-09-12, done): the activity rail now loads the bounded incident projection, highlights the earliest known break, filters by all seven typed relations, lets reviewers traverse linked nodes, exposes provenance status and durable row identity, and jumps from step/mutation nodes to their recorded step or file-change view. Saved message receipts can reopen historical incidents. `scripts/verify_browser.sh && node --check static/app.js` passed; the real `scripts/verify_e2e.sh` gate also passed after the UI change.

## P9 · Review hardening

### P9-T01 · Keep bounded incident graphs closed
- status: done
- depends: P8-T04
- design: docs/design/causal-observability.md#bounded-projection-integrity
- files: src/storage.rs, docs/design/causal-observability.md
- done-when: every returned edge and adjacency endpoint exists in the bounded node projection, and truncation is reported explicitly.
- verify: cargo test --locked storage
- note (2026-09-13, done): The incident projection now selects its bounded node set first, removes edges with omitted endpoints, rebuilds adjacency only from retained edges, and reports node/edge truncation. A 401-node regression proves every edge and adjacency identifier resolves inside the response. Exact verification passed: 11 storage tests.

### P9-T02 · Close Python SQLite test connections
- status: done
- depends: P9-T01
- design: docs/PLAN.md#p0--release-gates
- files: scripts/backup.py, tests/test_agentic_sql.py, tests/test_migrations.py, tests/test_sql_contracts.py
- done-when: the Python contract suite exits without unclosed-SQLite `ResourceWarning` output.
- verify: PYTHONWARNINGS=error::ResourceWarning python3 -m unittest discover -s tests -p 'test_*.py'
- note (2026-09-13, done): unittest fixtures now register per-test cleanup, migration tests close every tracked in-memory database, SQLite context blocks close rather than only commit, and the backup helper closes both endpoints. The warnings-as-errors gate passed all 61 Python tests with no `ResourceWarning` output.

## P10 · Correctness and operational safety

### P10-T01 · Close and center bounded incident projections
- status: done
- priority: critical
- lane: incident
- parallel: yes
- depends: P9-T01
- design: docs/ROADMAP.md#p10-correctness-and-operational-safety
- files: src/storage.rs, tests/recording_integration.py, docs/design/causal-observability.md
- done-when: earliest-known-break and every other returned reference resolve inside the node set; projection selects a causal neighborhood around the break and reports total/returned/omitted counts plus expansion cursors.
- verify: cargo test --locked storage && python3 tests/recording_integration.py
- note (2026-09-13, done): `causal-neighborhood-v1` now seeds deterministic breadth-first selection at the earliest durable break before filling remaining capacity in row order. The break, retained edges, and adjacency are closed over the returned node set; node/edge totals, returned and omitted counts, and opaque expansion anchors are explicit. The 401-node regression moves the break to the final step and proves it remains selected. Verification passed: 11 focused Rust storage tests and the compiled-server recording integration suite.

### P10-T02 · Enforce one process per database
- status: done
- priority: critical
- lane: runtime
- parallel: yes
- depends: P9-T02
- design: docs/ROADMAP.md#p10-correctness-and-operational-safety
- files: src/process_lock.rs, src/main.rs, src/storage.rs, tests/recording_integration.py
- done-when: a second live process cannot open the same DB; stale ownership is recovered safely and never resets another process's jobs or receipts.
- verify: cargo test --locked process_lock && python3 tests/recording_integration.py
- note (2026-09-13, done): the executable now acquires a non-blocking kernel `flock` on an owner-only per-database metadata file before SQLite opens or startup recovery runs. Live contention fails explicitly; stale metadata is replaced only after kernel ownership is proven, avoiding PID-reuse guesses. Three focused tests cover live exclusion, stale recovery, and independent databases. The compiled-server regression starts a second process against an actively generating receipt, proves it exits without changing the receipt or running step, then kills the owner and proves the successor performs normal interruption recovery without replay.

### P10-T03 · Expose readiness and deployed identity
- status: done
- priority: critical
- lane: runtime
- parallel: no
- depends: P10-T02
- design: docs/ROADMAP.md#p10-correctness-and-operational-safety
- files: build.rs, src/main.rs, src/storage.rs, static/app.js, scripts/deploy.sh, tests/recording_ui.cjs, tests/ui_smoke.cjs
- done-when: health/readiness reports commit, binary hash, schema version, queue/worker state and DB readiness; UI and deploy smoke detect a stale or unready artifact.
- verify: cargo test --locked health && bash scripts/deploy.sh
- note (2026-09-13, done): the authenticated readiness contract now reports the embedded source commit, runtime executable SHA-256, startup time, schema and SQLite probe, queue counts, and tracked worker liveness, returning 503 when any required component is unready. Served JavaScript embeds its expected commit and refuses a mixed/stale frontend; deployment now verifies the live process hash and readiness commit/hash/schema/workers/database before smoke-testing the API. Three focused health regressions, all 177 Rust tests, all 65 Python tests, compiled recording integration, both browser suites, and deployment smoke passed.

### P10-T04 · Automate encrypted backup and restore drills
- status: done
- priority: critical
- lane: backup
- parallel: yes
- depends: P9-T02
- design: docs/ROADMAP.md#p10-correctness-and-operational-safety
- files: scripts/backup.py, scripts/restore_test.py, tests/test_backup.py, docs/ARCHITECTURE.md, README.md
- done-when: rotating encrypted backups restore on a clean temporary target; missing key, corruption, quota exhaustion and interrupted writes fail explicitly without touching the source.
- verify: python3 -m unittest discover -s tests -p 'test_*backup*.py'
- note (2026-09-13, done): operational backups now use a versioned authenticated AES-256-GCM envelope with a separate owner-only 256-bit key, atomic publication, post-create clean restore drill, and configurable retention. Restore refuses overwrite and authenticates before publishing. Four focused tests prove committed WAL data survives, rotation keeps the requested count, and missing/wrong keys, corruption, quota exhaustion, interrupted writes, and unsafe key permissions fail without modifying the source or leaving output. The legacy plaintext helper remains only for short-lived migration snapshots. Verification passed: `python3 -m unittest discover -s tests -p 'test_*backup*.py'` (4 tests).

### P10-T05 · Graceful shutdown and automatic deployment rollback
- status: done
- priority: high
- blocker: release-blocker
- lane: release
- parallel: yes
- depends: P10-T03, P12-T05a
- design: docs/ROADMAP.md#p10-correctness-and-operational-safety
- files: src/main.rs, src/recording.rs, src/memory_agents.rs, Cargo.toml, scripts/deploy.sh, scripts/rollback_policy.py, tests/deploy_rollback.py, tests/recording_integration.py, docs/ARCHITECTURE.md
- done-when: shutdown drains safe commits and process groups without replay; a disposable deployment fixture proves backward-compatible binary rollback after a migration, and separately proves fail-closed recovery when old binaries cannot read the upgraded schema. Recovery documents backup age and writes accepted after backup; production promotion remains an explicit owner action.
- verify: scripts/verify_e2e.sh && python3 tests/deploy_rollback.py

### P10-T06 · Add crash, disk and SQLite fault injection
- status: done
- priority: high
- lane: reliability-tests
- parallel: yes
- depends: P10-T02
- design: docs/ROADMAP.md#p10-correctness-and-operational-safety
- files: tests/fault_injection.py, tests/recording_integration.py, docs/ARCHITECTURE.md
- done-when: deterministic tests cover crash boundaries, disk-full/read-only/I/O failure, WAL/integrity failure, ambiguous admission reconciliation, queue backpressure and background-process state without duplicate side effects.
- verify: python3 tests/fault_injection.py && scripts/verify_e2e.sh

## P11 · Runtime and storage performance

### P11-T01 · Wake streams from committed events
- status: done
- priority: high
- lane: streams
- parallel: yes
- depends: P10-T03
- design: docs/ROADMAP.md#p11-runtime-and-storage-performance
- files: src/main.rs, src/storage.rs, src/recording.rs, tests/recording_integration.py
- done-when: generation/activity streams use commit notifications and shared bounded fan-out while durable cursor replay remains authoritative after reconnect or missed notification.
- verify: cargo test --locked streaming && python3 tests/recording_integration.py

### P11-T02 · Separate serialized writes from bounded reads
- status: done
- priority: high
- lane: database
- parallel: no
- depends: P10-T02
- design: docs/ROADMAP.md#p11-runtime-and-storage-performance
- files: src/storage.rs, src/recording.rs, docs/ARCHITECTURE.md
- done-when: one write owner preserves transactions while bounded read connections prevent long projections from blocking unrelated reads; no DB guard crosses provider awaits.
- verify: cargo test --locked storage && scripts/verify_e2e.sh
- note (2026-09-14, done): Added a bounded SQLite read pool alongside the serialized writer. Read-only projections now use `DbStore::read` for stats, readiness, jobs, scopes, plans, and activity feeds. File-backed databases use independent readers while in-memory tests safely fall back to the writer connection. Verification passed: `cargo test --locked storage` and `scripts/verify_e2e.sh`.

### P11-T03 · Audit query plans and storage scale
- status: done
- priority: high
- lane: performance
- parallel: yes
- depends: P11-T02
- design: docs/ROADMAP.md#p11-runtime-and-storage-performance
- files: migrations/*, benches/*, scripts/benchmark.py, docs/ARCHITECTURE.md
- done-when: indexed plans and repeatable budgets exist for 10K sessions, 1M events, FTS recall and large incident graphs; regressions fail a dedicated benchmark gate.
- verify: cargo test --locked storage && python3 scripts/benchmark.py --check
- note (2026-09-14, done): Added repeatable SQLite benchmark gate covering required indexes, query plan validation, and configurable storage scale smoke checks. Verified with 10K sessions and 100K events.

### P11-T04 · Add retention, WAL and compaction maintenance
- status: done
- priority: high
- lane: database
- parallel: no
- depends: P11-T03
- design: docs/ROADMAP.md#p11-runtime-and-storage-performance
- files: migrations/*, src/storage.rs, scripts/maintenance.py
- done-when: configured retention, generation-chunk compaction, WAL checkpoint monitoring, optimize/analyze and incremental vacuum preserve receipts, provenance and user deletion semantics.
- verify: python3 tests/test_migrations.py && cargo test --locked retention
- note (2026-09-14, done): Added append-only migration 010 (user_version 10) with disabled-by-default `retention_policies`, `maintenance_runs` evidence and `generation_events.compacted_chunks`; storage now exposes retention policy config, finished-turn chunk compaction, WAL checkpoint/optimize/analyze/incremental-vacuum with readiness reporting, plus `scripts/maintenance.py --check`. Receipts, provenance and live turns are never deleted. Verified: `python3 tests/test_migrations.py` (001 -> 010) + `cargo test --locked retention` 3 passed / 0 failed, `scripts/verify_local.sh` exit 0, strict `scripts/verify_release.sh` exit 0.
- note (2026-09-14, amended): the Rust half of this surface was retired in 3165eed. `retention_policies`, `set_retention_policy`, `apply_retention`, `compact_generation_chunks` and `maintenance` were reachable from no route and no scheduler, leaving two implementations of the same destructive logic that had to be kept in sync by hand. `scripts/maintenance.py` is now the single owner: same targets, same cutoff rules, same `maintenance_runs` evidence, same schema-version guard, and unreachable with a stolen HTTP token. Migration 010 and the readiness projection are unchanged, so `/memory/status` still reports when retention, compaction and WAL checkpoints last ran. `verify` still passes non-vacuously: `cargo test --locked retention` selects one test that seeds `maintenance_runs` the way the script does and asserts each action projects its own latest run.

### P11-T05 · Optimize frontend delivery and idle work
- status: done
- priority: medium
- lane: frontend-performance
- parallel: yes
- depends: P11-T01
- design: docs/ROADMAP.md#p11-runtime-and-storage-performance
- files: static/*, src/main.rs, tests/ui_smoke.cjs
- done-when: redundant timers stop, hidden tabs pause nonessential work, long views are bounded/virtualized, assets are compressed/fingerprinted, and optional panels load without enlarging the initial path.
- verify: node --check static/app.js && scripts/verify_browser.sh
- note (2026-09-14, done): Replaced the two always-on timers (1 s turn clock, 5 s status/suggestions) with one visibility-aware clock: a hidden tab now runs no timer at all and does one immediate catch-up tick when it becomes visible again. Long views are bounded (at most 300 rendered messages and 200 session entries; trimming only drops rendered nodes and re-exposes "Load older messages", never history). Assets are fingerprinted: `index.html` requests `/app.js?v=<commit>` and `/style.css?v=<commit>`, those two are served `public, max-age=31536000, immutable` with a build-scoped ETag and answer `If-None-Match` with 304 and no body, while `/` is `no-cache` so a cached document can never point at a retired build, and every other route keeps `no-store`. Verified: exact gate `node --check static/app.js && scripts/verify_browser.sh` exit 0 (44 browser checks), `cargo test --locked` new asset/frontend tests passed, `scripts/verify_local.sh` exit 0, strict `scripts/verify_release.sh` exit 0.
- note (2026-09-14, gap): Response compression was NOT part of this task and was split out as P11-T06, since gzip/brotli normally needs a new dependency (`flate2`/`async-compression`/`tower-http` compression feature) and the crate registry is unreachable (`static.crates.io` returns 403). RESOLVED the same day by P11-T06 without any dependency, by precompressing the embedded assets at build time. No optional panel work was needed: the initial path already loads one script and one stylesheet.

### P11-T06 · Serve compressed static responses
- status: done
- priority: low
- lane: frontend-performance
- parallel: yes
- depends: P11-T05
- design: docs/ROADMAP.md#p11-runtime-and-storage-performance
- files: build.rs, src/main.rs
- done-when: the embedded text assets negotiate gzip from `Accept-Encoding`, identity stays correct for clients that do not ask or that refuse it, and the ETag/304 behaviour from P11-T05 holds per encoding.
- verify: cargo test --locked precompressed && cargo test --locked gzip && node --check static/app.js
- note (2026-09-14, done): No dependency was needed and none was added. `build.rs` pipes `static/index.html`, `static/app.js` and `static/style.css` (after build-commit substitution) through the system `gzip -9 -n`, which is deterministic, and writes the members to `OUT_DIR`; the server embeds those bytes and serves them verbatim when the client accepts gzip, so compression costs zero CPU per request instead of recompressing on every response. Responses carry `Content-Encoding: gzip`, `Vary: Accept-Encoding` and a distinct `"<commit>-<asset>-gzip"` ETag, so a shared cache cannot hand encoded bytes to a client that did not ask and revalidation stays per representation. `gzip` is optional at build time: if it is missing the build still succeeds with a cargo warning and the server serves identity bytes only. Tests decode-check the gzip members without a decompression crate by recomputing the CRC32 and length in the gzip trailer over the identity body. Verified: `cargo test --locked precompressed` 2 passed, `cargo test --locked gzip` 1 passed, `cargo test --locked fingerprinted_assets` 1 passed, `node --check static/app.js` OK, `scripts/verify_local.sh` exit 0, strict `scripts/verify_release.sh` exit 0.
- note (2026-09-14, scope): Brotli is not served (no `brotli` binary in the build environment) and dynamic JSON responses are still uncompressed; they are small and `no-store`, so this was not worth a second encoding. Revisit only if crate-registry egress is ever opened.

## P12 · Maintainability, API, CI, and releases

### P12-T01 · Decompose HTTP and agent orchestration modules
- status: done
- priority: high
- lane: architecture
- parallel: no
- depends: P10-T03
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: src/main.rs, src/api/*, src/agent_loop.rs, src/agent_loop/*
- done-when: routes/middleware/SSE and orchestration/budgets/permissions/tools/delegation/verification are cohesive modules with unchanged public behavior and smaller reviewable units.
- verify: bash scripts/verify_release.sh
- note (2026-09-14, done): seven verified verbatim seams, each compiled, tested and released before the next began. `src/main.rs` 3,947 → 2,530 with the HTTP surface now in `src/api/` (`routes.rs` 813, `auth.rs` 260, `stream.rs` 217, `assets.rs` 117, `error.rs` 90, `mod.rs` 9). `src/agent_loop.rs` 3,214 → 2,658 with `steps.rs` 340 (durable step/permission transitions), `compaction.rs` 171 (provider-window compaction plus its two pure unit tests), `verification.rs` 96 (verification evidence). Deployed as `b50811f`; `scripts/verify_release.sh` exit 0, 12 `[PASS]`, 217 Rust tests, and `/health` served commit + `binary_sha256` + `schema_version` 10 matching HEAD.
- deviations: the planned six-way `src/agent/` split was not carried out and the module was **not** renamed to `agent`. Three genuine boundaries existed (durable persistence, compaction, verification evidence) and were taken; the remaining `run` + `impl Ctx` (orchestration, budgets, permissions, tools, delegation) share the same eight private `Ctx` fields, so splitting them further would produce smaller files without a new boundary — and the rename would invalidate ~30 truthful historical doc references while changing nothing structural. Both are deferred to P12-T01b rather than forced here.

### P12-T01b · Optional further agent_loop decomposition and rename
- status: done
- priority: low
- lane: architecture
- parallel: yes
- depends: P12-T01
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: src/agent_loop.rs, src/agent_loop/*, src/main.rs, src/api/routes.rs, src/recording.rs, AGENTS.md, docs/design/*
- done-when: either a new boundary is justified before any further split of `run`/`impl Ctx` (they share eight private `Ctx` fields today, so line count alone is not a reason), or the decision to stop is recorded; if the module is renamed to `agent`, every external reference and current-state doc moves with it and historical journal entries are left untouched.
- verify: bash scripts/verify_release.sh
- note: raised by P12-T01 after seven verified seams. Also open: the loop tests (~1,570 lines) that still live in the parent because they drive `run` through a scripted provider; only the two pure compaction tests could move without inventing test-only visibility.
- note (2026-09-15, done): decision recorded — **stop here; no further split and no rename**. This is the second branch of the done-when, taken on measured evidence rather than on the inherited assertion. Every one of the eleven `impl Ctx` methods and `run` was checked for which of the eight private `Ctx` fields it actually touches, and then six candidate seams someone might reasonably propose (cancellation, permissions, tools, delegation, verification, orchestration) were scored for fields they would need versus fields left behind. **Not one has a single exclusive field.** `cancellation` (39 lines) needs `store`+`request`, `permissions` (44) needs `request`+`session`, `verification` (70) needs `store`+`request`+`session`+`model`, `tools` (157) needs six of eight, `delegation` (288) needs seven of eight, `orchestration` (350) needs seven of eight — and in every case the rest of the module still needs the same fields. Splitting any of them yields two files that both reach into the same state, which is strictly worse than one cohesive file: the coupling stops being visible to the compiler and has to be re-established as `pub(super)` or as parameters threaded through call sites. Line count was never the argument, and it is not one now.
- note (2026-09-15, rename): **not renamed to `agent`.** Cost measured: 33 references across eight source files (`src/main.rs`, `src/api/routes.rs`, `src/recording.rs`, `src/subagent.rs`, `src/tools/mod.rs`, `src/tools/task_tool.rs`, `src/storage/provenance.rs`, `src/limits.rs`) plus `tests/test_agentic_sql.py`, and 23 mentions in `docs/TASKS.md`, 23 in `docs/PROGRESS.md`, 2 in `AGENTS.md`, 1 in `docs/ROADMAP.md` and 3 design docs. The journal mentions are historical statements that were true when written; the done-when requires leaving them untouched, so a rename would deliberately create a vocabulary split where current code says `agent` and the accurate history says `agent_loop`. That is a real readability cost paid for zero structural change, so the name stays.
- note (2026-09-15, correction): the earlier note said "37 loop tests". The test binary disagrees: `cargo test --locked agent_loop:: -- --list` reports **23 tests** — 21 in the parent's `mod tests` and 2 already moved into `compaction`. The 1,570-line figure is right, but that module holds 21 tests and 14 shared helpers, not 37 tests. Corrected above rather than carried forward.
- note (2026-09-15, left open): two methods touch no `Ctx` field at all and could become free functions today — `race_cancellation` (18 lines) and `refuse_delegation` (15). That is 33 lines and no new boundary, so it was not worth a seam or the churn; recorded so the next reader does not have to rediscover it. The 21 parent tests still cannot move: they drive `run` through a scripted provider and would need test-only visibility invented to live anywhere else.

### P12-T02 · Decompose storage, browser and LSP internals
- status: done
- priority: high
- lane: architecture-tools
- parallel: yes
- depends: P12-T01
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: src/storage.rs, src/storage/*, src/tools/browser_tool.rs, src/tools/browser/*, src/tools/lsp_tool.rs, src/tools/lsp/*
- done-when: repository boundaries and protocol/session/validation/apply modules replace the three oversized files without weakening caps, rollback or recording.
- verify: bash scripts/verify_release.sh
- note: nine verified seams, each committed separately with a line-multiset check proving no logic
  was dropped. `storage.rs` 1475 -> 1138 (895 of those lines are tests) with `storage/scope.rs`
  joining config/jobs/memories/provenance/provider/turns; `browser_tool.rs` 1953 -> 549 with
  protocol/snapshot/cdp/session; `lsp_tool.rs` 1607 -> 306 with protocol/session/format/rename.
  Caps, permission diffs, rollback and recording moved unchanged. Still open and deliberately
  not done here: the remaining parent bulk in all three files is `mod tests`, which cannot move
  without inventing test-only visibility — the same boundary P12-T01 recorded.

### P12-T03 · Introduce typed API and database contracts
- status: done
- priority: high
- lane: api-contracts
- parallel: no
- depends: P12-T01
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: src/api/*, src/storage/*, static/api.js, docs/api.yaml
- done-when: typed DTOs/enums replace external `Value` indexing, errors have stable code/status/retryability, OpenAPI is checked, and the frontend client is generated or schema-validated.
- verify: cargo test --locked api && python3 tests/test_api_schema.py

### P12-T04 · Centralize bounds and remove unsafe debt
- status: done
- priority: high
- lane: correctness-cleanup
- parallel: yes
- depends: P9-T02
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: src/*, src/tools/*
- done-when: shared limits replace magic values; risky numeric casts are checked; nested patch-option semantics are explicit; production panics are audited; dead code is connected or removed; no new warning class is introduced.
- verify: cargo clippy --locked --all-targets --all-features -- -D warnings
- note: 97f39c6 makes `verify` pass for the first time (exit 0, from a 42-warning baseline) and satisfies two done-when clauses: no new warning class is introduced, and the dead-code clause is answered by decision rather than deletion. Real fixes: an orphaned activity-feed doc comment reattached to `activity_since`, a no-op `drop()` removed, explicit `.truncate(false)` in `process_lock.rs`, `as_chunks::<4>()` removing an infallible `unwrap()`, `div_ceil`, and `Budgets` struct literals. Gate hardened in the same commit: `scripts/verify_local.sh` now runs `--all-targets --all-features -- -D warnings`; previously it ran plain `cargo clippy --locked --all-targets`, which is how 42 warnings passed [PASS].
- note: 4d3f589 completes the production-panic-audit clause. A raw grep reports 842 panic sites in `src/`, which is not an audit because it cannot separate production code from `#[cfg(test)]` code; classifying by brace-depth-tracked module scope gives 21 production sites against 841 test sites. Twelve of the 21 were removed, including the only production `unwrap()` sitting directly in an HTTP handler (`receipt["request_id"]` in the compatibility `/chat` path, which now returns the durable 202 receipt instead of panicking the request). The five `auth.rs` header parses became `HeaderValue::from_static`, so the panic branch stops existing rather than merely being unreachable, and the served headers were re-checked on the wire after deploy.
- note: the three remaining implementation clauses are now closed. (a) Shared limits: `src/limits.rs` holds the verification bounds that `memory_agents.rs` and `storage.rs` must agree on (`MAX_CLAIMS`, `MAX_EVIDENCE_IDS_PER_CLAIM`, `MAX_SKIPPED_DIAGNOSTICS`, and the character caps), replacing three duplicated const groups and two raw `500`s in `agent_loop/verification.rs`. No value changed; the point is that the two modules can no longer drift. Caps used by exactly one module were deliberately left where they are. (b) Checked casts: of 68 production `as` casts, the seven whose safety depended on a distant line or on external input became `try_from` — the archive header length (the on-disk format is a 4-byte field, so `MAX_HEADER` should not be the only guard), the stored embedding dimension, the two LSP position casts, the `Drop` kill of a browser process group (an out-of-range pid must never be negated), the edit-tool anchor line and pid registration, and `arg_usize` in `fs_tools`. Documented widenings (`len() as i64`, the intentional u64→usize hash fold in `embeddings.rs`) were left alone; clippy's cast lints were not enabled because that is a diff far larger than a cleanup. (c) Patch-option semantics: `Option<Option<T>>` compiles but says nothing about which nesting level means "clear", so `ScopePatch` now uses `crate::patch::Patch<T>` with named `Unchanged`/`Clear`/`Set` states, a `Deserialize` impl that maps absent→`Unchanged` (via `Default`), `null`→`Clear`, value→`Set`, and an `apply` method that is the only merge path into `upsert_scope`. Three new tests pin the wire contract; behaviour is unchanged.
- deviations: (1) Roughly 26 warnings were resolved with documented `#[allow(dead_code)]` rather than by connecting or deleting code. Each allow carries a provenance comment. `src/archive/` (P13-T02) and the DbStore retention/maintenance surface are implemented and test-covered but reachable from no route; connecting them is a routing change, not a cleanup, and is tracked as P13-T02b. `git grep` at fdd830a proved the retention group was already unreachable before the P12-T01 routes seam, so it is pre-existing debt, not seam fallout. (2) The provenance validation chain is test-only; production inserts use raw SQL in `agent_loop::steps` where migration 006 CHECK constraints enforce the same invariants, so there is no correctness hole. (3) Nine production panic sites are retained deliberately: 8 validated-before-dispatch arms in `tools/browser_tool.rs` (6 `expect`, 2 `unreachable!`) and one `expect("inserted above")` in `lsp_tool.rs`. Converting browser dispatch to error returns reshapes the call path, which is a behaviour change and does not belong in a cleanup task. (4) Five of the six done-when clauses now hold outright. The one that does not is "dead code is connected or removed": it is answered by documented decision, not by the code, and closing it honestly requires the routing work tracked as P13-T02b. `status` therefore stays `doing`.
- note (2026-09-14, done): the last clause, "dead code is connected or removed", now holds in the code rather than by decision. 0f021b2 connected the archive group (four authenticated routes plus provenance); 3165eed retired the retention group in favour of `scripts/maintenance.py`. Every `#[allow(dead_code)]` this task added for those two groups is gone. This supersedes deviations (1) and (4) below: (1) now applies only to the allows outside these two groups, and (4) no longer holds, since all six done-when clauses are satisfied. Deviations (2) and (3) stand unchanged. Residual allows, each still carrying its provenance comment: `agentic_sql.rs:15`, `memory_agents.rs:22`, `repo_map.rs:36`, `safety.rs:150`, `storage.rs:742`, `browser_tool.rs:1205`, `tools/mod.rs:248`, and `record_provenance_edge` in `storage.rs` (the reader is now called; the writer is not).

### P12-T05a · Make release verification truthful and non-deploying
- status: done
- priority: critical
- blocker: release-blocker
- lane: release-contract
- parallel: yes
- depends: P9-T02
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: scripts/verify_local.sh, scripts/verify_release.sh, scripts/deploy.sh, README.md, docs/PLAN.md, docs/ROADMAP.md, docs/TASKS.md, docs/PROGRESS.md, docs/design/memory-wind-tunnel.md
- done-when: the permissive developer gate labels every omitted suite skipped; the strict release gate requires browser dependencies, runs mocked browser and real browser-to-service E2E lanes, and never restarts production; deployment rejects dirty source by default.
- verify: bash -n scripts/verify_local.sh scripts/verify_release.sh scripts/deploy.sh && bash scripts/verify_release.sh

### P12-T05b · Strengthen CI quality and supply-chain gates
- status: done
- priority: high
- lane: ci
- parallel: yes
- depends: P12-T05a
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: .github/workflows/ci.yml, scripts/check_supply_chain.py, scripts/verify_local.sh, tests/integration_smoke.py, tests/recording_integration.py, tests/test_agentic_sql.py, tests/test_recording_contracts.py
- done-when: fmt, warning budget, ResourceWarning, Rust/Python/shell/JS lint, dependency vulnerability/license policy, pinned actions and fallback-tool tests run reproducibly.
- verify: bash scripts/verify_release.sh
- notes: run locally and proven: `cargo fmt --all -- --check` (rust-fmt suite), clippy `-D warnings` as the warning budget, `bash -n` over all six shell scripts (shell-syntax), `node --check static/app.js` (javascript-syntax), pinned-action and dependency-shape checks (scripts/check_supply_chain.py), and ResourceWarning promotion. Honest limits, so nobody reads more into the green than it earns:
  (a) `cargo audit` and `cargo deny` are DECLARED IN CI ONLY. Neither is installed here and crates.io answers HTTP 403, so the local check asserts only that the CI steps EXIST. That is not evidence a vulnerability or license scan ever passed; the first real scan happens on a networked runner.
  (b) `shellcheck` and `ruff` are not installed locally. `bash -n` is a syntax check and is NOT a shellcheck substitute; no Python linter runs locally.
  (c) the supply-chain `lock` check reports SKIP offline (`cargo metadata` needs the network) and the script prints skips separately and does NOT count them as passes.
  (d) local `eslint` is v6 and needs an `.eslintrc`, so it was deliberately NOT wired; `node --check` remains the JS gate.
  (e) no duplicate `supply-chain` suite was added to scripts/verify_release.sh because that script runs scripts/verify_local.sh, which now runs it. One wiring, not two.
- evidence: scripts/verify_local.sh exit 0, 11 suites PASS, 1 SKIP (lock), 0 ResourceWarning occurrences. scripts/verify_release.sh: local-contract PASS, real-browser-to-server PASS, documentation-claims routes/ledger/counts PASS. The ResourceWarning wrapper was negative-tested both ways: a deliberately leaky probe FAILED it (exit 1) while raw `python3 -W error::ResourceWarning` exited 0 on the same probe, and a clean probe PASSED (exit 0).
- found-and-fixed: promoting ResourceWarning exposed 9 real leaks from `with sqlite3.connect(...)`, which commits but never closes; tests/recording_integration.py now uses contextlib.closing. Separately, the whole-tree `cargo fmt` in cb999fe silently broke tests/test_agentic_sql.py and tests/test_recording_contracts.py: both scraped `pub const NAME: &str = r#"..."#;` with the `=` required on one line, and rustfmt wrapped 7 of 31 declarations, so the scrape returned 24 of 31 constants. Both scrapers now tolerate whitespace around `=` and assert the scraped set equals the declared set, so a future reformat cannot quietly shrink what these suites test.

### P12-T06 · Add coverage, property, fuzz and release evidence
- status: done
- priority: medium
- lane: release-quality
- parallel: yes
- depends: P12-T05b
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: .github/workflows/*, fuzz/*, tests/*, scripts/release.sh
- done-when: redaction/SSE/diff/path/graph/context properties, import/protocol fuzzing, coverage, cross-target checks, performance budgets, SBOM/checksums/signatures, public smoke and rollback tests are automated.
- verify: bash scripts/verify_release.sh && scripts/verify_e2e.sh
- notes: The local `release-quality` suite fail-closed inventories and runs six existing production-level property contracts (redaction chunking, SSE cursor replay, diff reversal, sandboxed paths, closed incident graphs, and context budgets), 2,000 deterministic import payloads plus file/symlink/count and provider-stream protocol boundaries, the 10k-session/100k-message storage budget, disposable rollback, and a byte-for-byte reproducible release archive containing a CycloneDX SBOM and checksums. Deleting the coverage declaration was negative-tested: the gate exited 1 and the workflow was restored byte-identical.
- CI-only evidence: `cargo llvm-cov` with a 50% line floor, host plus aarch64 checks, two independent release builds compared byte-for-byte, and keyless Sigstore signing are declared in the pinned `release-evidence` job. The authenticated HTTPS public smoke is a production-environment `workflow_dispatch` job and runs only when `HARNESS_PUBLIC_URL` and `HARNESS_PUBLIC_SMOKE_TOKEN` are configured. None of those networked lanes ran on this offline host; locally they report SKIP and are not counted as passes.
- evidence: `bash scripts/verify_release.sh && scripts/verify_e2e.sh` exited 0 after the final code/workflow changes. All locally runnable property, fuzz, performance, rollback, artifact, mocked-browser, and real browser-to-server lanes passed. Local coverage/signature/cross-target/public-smoke remained explicitly SKIP. No service was restarted or deployed.

### P12-T07a · Reconcile the documented baseline
- status: done
- priority: high
- blocker: release-blocker
- lane: docs-baseline
- parallel: yes
- depends: P12-T05a
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: README.md, AGENTS.md, docs/PLAN.md, docs/TASKS.md, docs/PROGRESS.md
- done-when: historical phase prose is labeled historical, completed work is not described as pending, current runtime/tool behavior is consistent across entry documents, and old evidence is not presented as a fresh run.
- verify: git diff --check

### P12-T07b · Prevent documentation and deployment claim drift
- status: done
- priority: high
- lane: docs-automation
- parallel: yes
- depends: P12-T07a
- design: docs/ROADMAP.md#p12-maintainability-api-ci-and-release-engineering
- files: README.md, AGENTS.md, docs/*, scripts/check_docs.py
- done-when: routes, phases, test counts, deployment identity and limitations cannot drift silently; volatile counts are derived or removed.
- verify: python3 scripts/check_docs.py && git diff --check
- note (2026-09-14, done): `scripts/check_docs.py` did not exist, so this task's own verify command could not run; the checker now exists and runs in `scripts/verify_release.sh` as the `documentation-claims` suite. Five checks, all mechanical: the router and a new `## HTTP surface` inventory in `docs/ARCHITECTURE.md` must agree exactly in both directions; every `verify:` command must name scripts that exist (FAIL when the task is `done`, WARN while it is `todo`); statuses must be known, dependencies must exist, and no task may be `done` while a dependency is not; the living docs must carry no hand-written test counts; and `/health` `commit` must equal HEAD or differ only by commits that touch documentation. Prose is deliberately not checked — a gate that flags style becomes noise, and a noisy gate gets disabled.
- note (2026-09-14, scope correction): the done-when asks for volatile counts to be "derived or removed", which presumed counts live in the living docs. They do not. Every hand-written test count sits in `docs/PROGRESS.md`, `docs/TASKS.md` notes and a handoff doc — dated evidence about one run, which should stay frozen. The checker therefore guards README, AGENTS.md and ARCHITECTURE.md and exempts the journals rather than rewriting history to satisfy the wording.
- note (2026-09-14, first findings): the checker's first run failed 24 checks. Four were its own bug (`depends: —` parsed as a task name) and twenty were a design mistake of mine — treating README as an API spec, which is why the inventory replaced the heuristic. Two real findings survive as warnings: P17-T04 and P17-T05 verify with `scripts/experiment.py` and `scripts/remote_runner.py`, neither of which exists. Both are `todo`, so a warning is honest; if either is ever marked `done` with those scripts still missing, the same check turns into a failure.
- note (2026-09-14, baseline honesty): the initial 39-route inventory was generated from `router()` rather than hand-audited, so the first comparison was circular by construction. Every comparison after this commit is independent. The gate was then proven to fail on purpose: removing `/archives/{id}` from the inventory and adding an invented route each produced exit 1 with the drift named.

## P13 · Security, privacy, and auditability

### P13-T01 · Harden request authentication and abuse boundaries
- status: done
- priority: critical
- lane: auth
- parallel: yes
- depends: P10-T03
- design: docs/ROADMAP.md#p13-security-privacy-and-auditability
- files: src/main.rs, static/app.js, tests/integration_smoke.py, tests/ui_smoke.cjs, tests/recording_ui.cjs, docs/ARCHITECTURE.md
- done-when: per-route body/rate limits, delayed auth failures, token rotation, short-lived browser sessions, proxy identity options and HTTPS HSTS guidance are tested without putting credentials in URLs or storage.
- verify: cargo test --locked auth && python3 tests/integration_smoke.py

### P13-T02 · Add encrypted archive, backup keys and deletion policy
- status: done
- priority: high
- lane: privacy
- parallel: no
- depends: P10-T04
- design: docs/ROADMAP.md#p13-security-privacy-and-auditability
- files: migrations/*, src/archive/*, scripts/backup.py, docs/ARCHITECTURE.md
- done-when: opt-in exact originals and backups use reviewed versioned AEAD with external key storage/rotation; forget, delete-source, purge-index and archive deletion remain distinct and auditable.
- verify: python3 tests/test_migrations.py && cargo test --locked archive
- note: the library, migration 007 tables and `scripts/backup.py` all ship and pass their gate, but nothing in the server constructs an `ArchiveStore`, so no operator can reach the feature at runtime. P12-T04 found this via dead-code analysis and kept the module intact behind a documented module-level allow. Route wiring is tracked as P13-T02b; this entry stays `done` for the library contract only.

### P13-T02b · Wire the archive and retention surfaces to routes
- status: done
- priority: medium
- lane: privacy
- parallel: yes
- depends: P13-T02, P12-T01
- design: docs/ROADMAP.md#p13-security-privacy-and-auditability
- files: src/api/routes.rs, src/archive/mod.rs, src/storage.rs, docs/ARCHITECTURE.md
- done-when: opt-in exact archiving, archive read/delete and privacy-action recording are reachable through authenticated routes; the DbStore retention/maintenance surface (`retention_policies`, `set_retention_policy`, `apply_retention`, `compact_generation_chunks`, `maintenance`, `provenance_edges`) is either exposed or explicitly retired; every `#[allow(dead_code)]` added by P12-T04 for these two groups is removed rather than left in place.
- verify: cargo clippy --locked --all-targets --all-features -- -D warnings && cargo test --locked archive && python3 tests/integration_smoke.py
- note (2026-09-14, done): archiving is now reachable and opt-in via `HARNESS_ARCHIVE_ROOT` + `HARNESS_ARCHIVE_KEY` (and optional `HARNESS_ARCHIVE_KEY_PREVIOUS` for rotation). Routes, all behind authentication: `POST /sources/{id}/archive` (raw bytes, 2 MB cap, 201), `GET|DELETE /archives/{id}`, `POST /sources/{id}/privacy` (202), `GET /chat/requests/{id}/provenance`. The body is raw bytes, not JSON, because an exact archive that had to survive a JSON string encoding would no longer be exact. Unconfigured, all four answer 501 naming the missing configuration: a 500 would imply a fault and a 2xx would imply bytes were kept. A missing or deleted archive is 404 and a payload that fails authentication is 500, so tamper can never read as "no such archive". The retention half of the done-when was closed by explicit retirement (3165eed), not exposure — see the amended P11-T04 note.
- note (2026-09-14, bug found by wiring): `KeyRing::by_id` used `self.previous.as_ref()?`, which short-circuits the whole lookup to `None` whenever no rotation key is configured. A single-key deployment could therefore write archives it could never read back. The existing unit tests missed it because the only single-key read asserted a "deleted" error, which fired first and masked the key lookup. Fixed in 0f021b2; the new HTTP round-trip test covers exactly that path. A reachable feature is tested differently from a library, which is the argument for wiring it.
- note (2026-09-14, deployed): 8c9f241 is live (pid 316123, `binary_sha256 25c91fa7…`, schema 10). Verified on the wire, not only in tests: all four routes answer 401 before 501, so authentication is checked before existence; the 501 body names the missing configuration; provenance answers 200 for a valid UUID and 400 for a malformed one; `/health` `commit` and `binary_sha256` both match HEAD. Archiving is deliberately left unconfigured in production — generating a long-lived archive key is an owner decision, because losing it makes every archived byte unreadable. Reading the deployed 501 also caught a wording bug the tests could not: GET and DELETE were answering "no bytes were stored", a write-path sentence describing an action never attempted (fixed in 8c9f241).
- note (2026-09-14, archiving enabled): supersedes the "deliberately left unconfigured" sentence above. The owner approved key generation, so production now runs with `HARNESS_ARCHIVE_ROOT=/var/lib/harness/archives` and `HARNESS_ARCHIVE_KEY=/etc/harness/archive.key` (key_id `7a28e8208dee9282`, 0600, outside the repo; `.env` holds paths only, never key material, because `open_from_env` takes filesystem paths). f83ef0a redeployed as pid 318441. The round trip that was impossible while unconfigured now passes on the wire: 4 KB of random bytes POSTed, read back byte-identical (sha256 `00940261…` both directions, `application/octet-stream`), the on-disk `.har` is AES-256-GCM ciphertext behind a `HARNESS-EXACT` header naming that key_id, `forget` recorded 202 while an unknown action is 400, DELETE then re-read is 404, an unknown id is the same 404 with the same wording (so deletion does not leak which archives once existed), and an unauthenticated read is still 401. Outstanding owner action: the key has no off-machine backup yet, and there is no recovery path without it.
- note (2026-09-14, rotation drill): key rotation is no longer untested. An archive was written under key_id `7a28e8208dee9282`, the key was rotated (new current `2e21f1c53993f75c`, retired key kept as `HARNESS_ARCHIVE_KEY_PREVIOUS=/etc/harness/archive.key.prev`), the service was restarted, and the pre-rotation archive still read back byte-identical (sha256 `7f228e81…`) — the `KeyRing::by_id` path resolving a header key_id to the retired key, exercised in production rather than in a unit test. A post-rotation write is stamped with the new key_id and round-trips too; a header census confirmed both generations coexisting in the same archive root. Both drill archives were deleted afterwards and the root is empty. Key backups live at `/root/keybackup-harness/` with a non-secret `FINGERPRINT.txt` recording both key_ids and file digests. **Both** key files must be backed up: pre-rotation archives are unreadable without the previous key. Still owner-owned: those backups are on the same host, so off-machine copies remain outstanding.

### P13-T03 · Strengthen browser, command and path policies
- status: done
- priority: high
- lane: tool-security
- parallel: yes
- depends: P10-T06
- design: docs/ROADMAP.md#p13-security-privacy-and-auditability
- files: src/tools/*, tools/schemas/*, tests/recording_integration.py
- done-when: browser destinations/transfers, command classes/protected paths/network use, and final write-time path ownership are policy-gated with TOCTOU and symlink regressions.
- verify: cargo test --locked tools && python3 tests/recording_integration.py

### P13-T04 · Export verifiable sanitized audit bundles
- status: done
- completed: 2026-09-15T17:56:01Z; added an authenticated two-step preview/release export for one selected run, with a strict allow-list, recursive shared sanitization, per-record and bundle SHA-256 checksums, optional predecessor hash-chain integrity, and stale-review refusal.
- result: preview returns the exact bounded artifact and digest without releasing it; release rebuilds the same selected-run incident projection and requires the reviewed digest, so changed evidence, audience, chain mode, unrelated workspace fields, unsanitized values, reorder, removal or tampering cannot pass verification.
- verify: `cargo test --locked audit_export` (3 passed), `scripts/verify_e2e.sh` (all real browser-to-server and failure-path scenarios passed), `cargo test --locked` (295 passed), API/SQL/migration/recording contracts passed, and `scripts/verify_release.sh` exited 0 with the strict non-deploying release gate.
- publication: implemented on `task/p13-t04-audit-bundles`; no deployment, service restart, access, DNS, Cloudflare, UpCloud, firewall, relay, TLS, Tailscale or origin change.
- priority: medium
- lane: audit
- parallel: yes
- depends: P13-T02, P16-T03
- design: docs/ROADMAP.md#p13-security-privacy-and-auditability
- files: src/export/*, src/main.rs, docs/ARCHITECTURE.md
- done-when: a selected run exports only reviewed sanitized evidence with checksums and optional hash-chain integrity, without unrelated workspace data.
- verify: cargo test --locked audit_export && scripts/verify_e2e.sh

## P14 · Workflow, tools, providers, and UX

### P14-T01 · Add durable cancellation and safe-boundary retry
- status: done
- completed: 2026-09-14T03:07:07Z; resumed independent review verified the terminal-state/recovery and late-registration fixes, then reran the strict non-deploying release gate.
- result: durable cancellation now wins inside completion/failure transactions and restart recovery, provider/sub-agent/permission waits stop cooperatively, late process-group registration is killed, and retry is admitted only from recorded non-mutating boundaries.
- verify: `cargo test --locked` (207 passed), `bash scripts/verify_release.sh` (exit 0: native, contracts, HTTP, mocked-browser, real browser-to-server cancellation/retry and unsafe-retry refusal), `git diff --check` (clean).
- publication: worktree remains uncommitted and unpushed; no deploy or production restart.
- handoff: docs/HANDOFF-P14-T01.md (historical checkpoints and implementation ledger).
- priority: high
- lane: workflow
- parallel: yes
- depends: P10-T05
- design: docs/ROADMAP.md#p14-workflow-tools-providers-and-ux
- files: migrations/*, src/recording.rs, src/agent_loop.rs, src/main.rs, static/app.js
- done-when: users can cancel generation, permission waits, sub-agents and process groups, then retry only from a recorded non-mutating boundary without replaying side effects.
- verify: scripts/verify_e2e.sh

### P14-T02 · Add session and approval workflows
- status: done
- priority: medium
- lane: product-workflow
- parallel: yes
- depends: P14-T01
- design: docs/ROADMAP.md#p14-workflow-tools-providers-and-ux
- files: migrations/*, src/main.rs, static/*
- done-when: sessions support naming/search/archive/fork; permissions support bundles, countdowns and safe notifications; stop/regenerate states are unambiguous.
- result: migration 013 adds durable session title/archive/fork lineage. Session search, archive/restore, rename and history-preserving fork are exposed through the API and UI. Permission reads now expire stale approvals before projection, expose request-scoped bundle identity/count metadata, show a live deadline, and clearly distinguish a requested approval from an approval recorded by the operator.
- verify: `cargo test --locked` (256 passed), `cargo clippy --locked --all-targets -- -D warnings` (clean), `python3 tests/test_migrations.py` (001→013), `python3 tests/test_api_schema.py` (51 operations), `scripts/verify_browser.sh` (passed), `scripts/verify_e2e.sh` (passed: approval, denial, recovery, cancellation/retry and unsafe-retry refusal).

### P14-T03 · Add process, Git and checkpoint controls
- status: done
- priority: medium
- lane: developer-workflow
- parallel: yes
- depends: P14-T01
- design: docs/ROADMAP.md#p14-workflow-tools-providers-and-ux
- files: src/tools/*, tools/schemas/*, static/*
- done-when: active jobs are inspectable/cancellable; changes show Git state; checkpoint/restore and focused commit proposals are recorded and require approval before mutation/push.
- result: Added the Git tool and schema with bounded read operations, recorded focused commit proposals, tracked-only checkpoints, stale/inconsistent checkpoint refusal, and approval-gated checkpoint restore, commit, and push. Detached Bash processes are registered, bounded, purged when dead, and removable through authenticated API/UI controls. Git state is exposed as read-only scoped evidence.
- verify: `cargo test --locked` (260 passed), `cargo clippy --locked --all-targets -- -D warnings` (clean), `python3 tests/test_migrations.py` (001→013), `python3 tests/test_api_schema.py` (54 operations), `scripts/verify_browser.sh` (passed), `scripts/verify_e2e.sh` (passed: approval, denial, recovery, cancellation/retry and unsafe-retry refusal).

### P14-T04a · Enforce minimal fail-closed spend limits
- status: done
- priority: high
- blocker: release-blocker
- lane: provider-cost-safety
- parallel: yes
- depends: P10-T03
- design: docs/ROADMAP.md#p14-workflow-tools-providers-and-ux
- files: src/memory_agents.rs, src/recording.rs, src/storage.rs, static/*
- done-when: configurable per-turn and per-day request/token/cost ceilings reject new provider work before dispatch, record why usage is unavailable, and never silently treat unknown cost as zero.
- verify: cargo test --locked provider && python3 tests/recording_integration.py

### P14-T04b · Improve provider scheduling and resilience
- status: done
- result: all six clauses landed in four commits. Role fallback now applies to delegated calls as well as the parent loop (c081387); a shared circuit breaker with Retry-After and jittered backoff gates both reserve_spend funnels (3099c93); background roles yield daily headroom to foreground work and can carry their own ceiling (69df89a); capability detection moved before dispatch, cached per model on the shared health state. The P14-T04a fail-closed limits are preserved: every new refusal is enforced inside the existing reservation transaction or ahead of reserve_spend, so no path reaches the provider without a reservation.
- result-verify: `cargo test --locked provider` (34 passed, from 23 at task start), `cargo test --locked` (248 passed), `python3 tests/recording_integration.py` (PASS), `cargo clippy --locked --all-targets --all-features -- -D warnings` (clean).
- priority: high
- lane: providers
- parallel: yes
- depends: P11-T01, P14-T04a
- design: docs/ROADMAP.md#p14-workflow-tools-providers-and-ux
- files: src/memory_agents.rs, src/recording.rs, src/storage.rs, static/*
- done-when: capability detection, role fallback, circuit breakers, Retry-After/jitter, foreground/background fairness, and role-specific budgets preserve the fail-closed limits.
- verify: cargo test --locked provider && python3 tests/recording_integration.py
- note (2026-09-15, selection): picked by the documented rule, not by the previous session's suggestion. That session proposed P13-T04 next; P13-T04 is ineligible because its dependency P16-T03 is still `todo`. Among eligible `todo` tasks whose dependencies are all `done`, the `high` tier holds P14-T04b and P15-T01 with no `release-blocker` on either, so the earlier stable task ID wins.
- note (2026-09-15, starting recon): all six done-when clauses are greenfield. At 2805d71 no `circuit`, `breaker`, `Retry-After`/`retry_after`, `jitter`, `capabilit*` or role-`fallback` symbol exists anywhere in `src/` (the only `fallback` hits are unrelated: `grep_fallback`, `fallback_symbols`, an API error-status fallback). `src/storage/provider.rs` is 143 lines and exports no `pub` item. The P14-T04a surface this must preserve is `SpendLimits` + `reserve_spend`/`finish_spend` wrapping every provider call in `src/memory_agents.rs`, so resilience work has to route through that reservation path rather than around it.

### P14-T05a · Expand language/browser tools and modular accessible UI
- status: done
- priority: medium
- lane: experience
- parallel: yes
- depends: P12-T02, P11-T05
- design: docs/ROADMAP.md#p14-workflow-tools-providers-and-ux
- files: src/tools/*, static/*, tests/*ui*.cjs
- done-when: configured languages gain AST/LSP discovery and bounded session reuse; browser evidence supports safe screenshots and an explicit fail-closed transfer policy; UI has modules, keyboard/mobile/a11y, reconnect state, richer diffs, and context/cost/project dashboards.
- verify: scripts/verify_browser.sh && scripts/verify_e2e.sh
- result: LSP gained a read-only `discover` operation for Rust and C/C++ with root/server-keyed session reuse, a two-session cap, 30-second idle expiry, and pool discard on protocol/timeout/process failure; discovery refuses paths escaping the project root. Browser evidence supports fixed viewport-only PNG screenshots capped at 1.5 MiB, signature-checked and atomically published under `.harness/artifacts/browser/` with generated filenames, while upload and download operation names are absent from the schema and refused before CDP dispatch. The UI gained connection states (`Offline`, `Connection delayed`, `Reconnecting…`, `Ready`) with read-only status refresh and no automatic resend of an ambiguous chat request, a bounded project/usage/cost/context/permission overview rendered with `textContent`, and accessibility work covering keyboard focus management, a skip link to a focusable main landmark, `aria-controls`/`aria-expanded` on the mobile drawer, and Escape-to-close with focus restore. The optional voice clause was split out to P14-T05b rather than implemented speculatively.
- result-verify: `cargo test --locked` (264 passed), `cargo clippy --locked --all-targets -- -D warnings` (clean), `cargo fmt --all -- --check` (clean), `python3 tests/test_api_schema.py` (passed), `python3 tests/test_migrations.py` (passed), `scripts/verify_browser.sh` (passed), `scripts/verify_e2e.sh` (passed). Worktree clean at `913f55d`.
- note (2026-09-15, split): the original P14-T05 bundled optional voice input with the language, browser, and UI surfaces. Every other clause is implemented and verified, but voice cannot be built without a product decision naming the speech engine/endpoint and retention policy, so the delivered scope closes here and the unmet clause moves to P14-T05b instead of holding the `experience` lane open at `doing`.

### P14-T05b · Add optional voice input behind a named speech provider
- status: todo
- priority: low
- lane: experience
- parallel: yes
- depends: P14-T05a
- design: docs/design/ui.md#voice-input-deferred
- files: static/*, tests/*ui*.cjs, docs/design/ui.md
- done-when: a product contract names the speech engine/endpoint and retention policy; voice is user-visible opt-in with a recording indicator; browser microphone permission is requested only after that opt-in; no raw audio or transcript is persisted in local or session storage; the transcript is bounded at 8 KiB with an explicit edit-before-send step; nothing is submitted automatically; an unavailable or unconfigured speech provider produces a clear refusal; and browser tests assert the refusal path and the absence of persisted audio or transcript.
- verify: scripts/verify_browser.sh && scripts/verify_e2e.sh
- note (2026-09-15, blocked on product decision): this is deliberately not started. The minimum safe contract is documented in `docs/design/ui.md` under "Voice input (deferred)"; until the speech engine/endpoint and retention policy are chosen, the composer stays text-only and no capture code should land.

## P15 · Memory and history

### P15-T01 · Add optional semantic recall and evaluation
- status: done
- result: the coexistence clause was already met before this task: `src/storage/memories.rs` recall already unioned FTS5 with the local hashed vectors and reranked deterministically. What was missing was the optionality and the measurement. `tests/recall_eval/fixtures.json` now carries seven labeled cases with their own thresholds; the Rust test `storage::tests::recall_eval_fixtures_meet_labeled_budgets` drives the real `DbStore::recall` over them and writes `tests/recall_eval/metrics.json`; `tests/recall_eval/run.py --check` refuses metrics that are missing, measured against different fixtures, incomplete, or below budget. Measurement then changed the design: the hasher scores unrelated text ("xylophone quarterly submarine" vs "database\nSQLite", 0.069) *above* genuine related spelling ("rustacean tooling" vs "Rust systems", 0.042), so no cosine floor separates noise from signal and tuning the existing `> 0.01` filter would only trade false positives for the morphology bridge the vector arm exists to provide. Instead the arm became opt-out via `HARNESS_MEMORY_SEMANTIC_RECALL=0` (`embeddings::Strategy`, parsed as a pure function; `recall_with_strategy` takes it as an argument so tests never mutate process environment), default unchanged, and the eval records both arms: hybrid 0.786 precision / 1.000 coverage, lexical-only 0.857 / 0.875. Stale use is 0.000 across all cases — archived rows never reach the context. No embedding API or model download was added, so docs/PLAN.md:25 still holds.
- result-verify: `cargo test --locked recall` (6 passed, from 2 at task start), `cargo test --locked` (252 passed, from 248), `python3 tests/recall_eval/run.py --check` (PASS, exit 0), `cargo clippy --locked --all-targets -- -D warnings` (clean), `cargo fmt --all` (clean). The gate was also proved load-bearing by tampering: deleted metrics, mismatched `fixture_sha256`, an injected stale leak, and a model-identity change each exit 1.
- priority: high
- lane: memory-retrieval
- parallel: yes
- depends: P11-T03
- design: docs/ROADMAP.md#p15-memory-and-history
- files: src/embeddings.rs, src/storage.rs, tests/recall_eval/*
- done-when: optional local semantic embeddings coexist with deterministic hashing; labeled fixtures measure precision, stale use, latency and context cost before enabling a model.
- verify: cargo test --locked recall && python3 tests/recall_eval/run.py --check

### P15-T02 · Persist retrieval explanations and rehearsal
- status: done
- priority: high
- lane: memory-observability
- parallel: yes
- depends: P15-T01
- design: docs/ROADMAP.md#p15-memory-and-history
- files: migrations/*, src/context.rs, src/storage.rs, static/*
- done-when: receipts retain included/excluded candidates, scores, budget reasons and revisions; approval can preview deterministic retrieval changes without causal overclaiming.
- verify: cargo test --locked context && scripts/verify_browser.sh
- result: migration `011_retrieval_receipts.sql` (`user_version=11`) adds a write-once `retrieval_receipts` row per request plus one `retrieval_candidates` row per ranked candidate, carrying scope, key, revision, rank, the five component scores, total score, bytes, and an `included`/`excluded` decision with one of four reasons (`ranked_and_fit`, `rank_cutoff`, `payload_ceiling`, `category_budget`). `src/storage/memories.rs` was refactored so one ranking implementation (`rank_in_tx`) serves live recall, the persisted receipt and the rehearsal — a separate preview ranker would have been a copy that drifts. `src/context.rs` `Ledger::include_memory`/`exclude_memory` carry the revision so builder-dropped rows are attributed to `category_budget` rather than to retrieval. `save_retrieval_receipt` inserts `ON CONFLICT DO NOTHING` and reports whether it wrote, so a retried turn cannot rewrite history; the prompt is fingerprinted, never stored. `GET /chat/requests/{id}/retrieval` serves the receipt (404 when a turn recorded none) and `POST /memory/retrieval/preview` re-ranks inside an `IMMEDIATE` transaction that is always rolled back, with `persist: false` also skipping the embedding upsert and the `recall_count` increment, so rehearsal is observation-free and the candidate stays pending. On causal overclaiming: nothing runs a counterfactual generation, so every user-visible string says a memory was *sent* or that *retrieval would change*, never that the answer would change; the server returns that note and the browser test asserts it. Declared-files caveat: the work also had to touch `src/storage/memories.rs`, `src/recording.rs`, `src/api/routes.rs`, `src/recording_tests.rs` and `src/main.rs` (schema-version assertions) beyond the listed `migrations/*, src/context.rs, src/storage.rs, static/*`. A latent defect the full suite caught: readiness and `/health` still hard-coded `schema_version==10` after the migration bump, which would have deployed a server reporting itself not ready.
- result-verify: `cargo test --locked context` (11 passed), `cargo test --locked` (254 passed, from 252), `scripts/verify_browser.sh` (both suites passed; new checks `retrieval_receipt_panel`, `retrieval_receipt_text_inert`, `retrieval_preview_rehearsal`), `python3 tests/test_migrations.py` (001→011, `user_version=11`, data/FTS/FKs preserved), `cargo clippy --locked --all-targets -- -D warnings` (clean), `cargo fmt --all` (clean).

### P15-T03 · Add memory governance and timelines
- status: done
- priority: medium
- lane: memory-governance
- parallel: no
- depends: P15-T02
- design: docs/ROADMAP.md#p15-memory-and-history
- files: migrations/*, src/storage.rs, static/*
- done-when: branches, considered/chosen/superseded decisions, temporary expiry, conflict groups, deduplication, usefulness feedback and pinned profile entries preserve review/revision history.
- verify: python3 tests/test_migrations.py && cargo test --locked memory && scripts/verify_browser.sh
- result: migration `012_memory_governance.sql` (`user_version=12`) adds checked-out branches, decision timelines, expiry, conflict groups, usefulness feedback and pinned entries. Recall shadows `main` with the active branch and rejects expired memories by timestamp; governance mutations preserve append-only decision history. The governance overview, entry timeline, mutation and branch APIs are documented in OpenAPI and the architecture inventory. The SQL contract mirror now runs against the complete 001→012 chain, so it tests the current branch-aware approval and recall statements rather than an obsolete pre-branch schema. No speculative static UI was added: the existing browser surface remains green, while the governance behavior is covered by native/API contract tests.
- result-verify: `python3 tests/test_migrations.py` (001→012, `user_version=12`, data/FTS/FKs preserved), `python3 tests/test_sql_contracts.py` (16 passed), `python3 tests/test_api_schema.py` (49 operations, 16 error codes, 22 receipt fields and 5 request states matched), `cargo test --locked memory` (29 passed), `bash scripts/verify_local.sh` (passed; coverage/signature/cross-target/public-smoke skipped as environment-only release checks), `scripts/verify_browser.sh` (both mocked-browser suites passed), `cargo test --locked` (256 passed), `cargo clippy --locked --all-targets -- -D warnings` (clean), and `cargo fmt --all -- --check` (clean).

### P15-T04 · Search and port sanitized history
- status: done
- priority: medium
- lane: history
- parallel: yes
- depends: P13-T02, P15-T02
- design: docs/ROADMAP.md#p15-memory-and-history
- files: migrations/*, src/storage.rs, src/export/*, static/*
- done-when: scoped FTS searches sanitized conversations/artifacts with citations; forget/source-delete differ; selected memories and continuation packets import/export with stable IDs, revisions and audience review.
- verify: python3 tests/test_migrations.py && scripts/verify_e2e.sh
- result: migration `014_history_search.sql` (`user_version=14`) adds `history_documents` — an explicitly maintained projection of sanitized turns and artifacts — plus `history_fts` over it, an append-only `history_privacy_events` audit, and the `export_bundles` / `export_items` / `import_receipts` / `import_decisions` ledger. The index is a projection rather than an FTS5 external-content table straight over `messages`/`sources` because only sanitized content may be indexed and sanitization is a Rust function: a trigger would have to index the raw column and hope the writer had cleaned it. `history_fts` is then kept in sync by the same three-trigger pattern `memory_fts` uses in 001/012, and two triggers on `messages`/`sources` reach the source-deleted state from the source side, so a retention sweep cannot leave a document quoting content that no longer exists. Sanitization and citation building have exactly one implementation, `src/export/review.rs`, shared by search, export and import — a second copy would have let search and export disagree about what may leave. Search is scoped by project scope always and optionally by session and kind, and every hit carries a citation (id, source_id, kind, revision, scope, session, timestamp, checksum, sanitizer). Three deliberately overlapping gates keep unsanitized text out: only sanitized text is ever indexed, forgotten/source-deleted rows are excluded in SQL, and each body is re-checked against the shared sanitizer at read time so a row written by an older sanitizer or edited out of band is suppressed rather than served. `forget` and `delete_source` are separate columns and separate operations, per the vocabulary migration 007 established for sources: forget suppresses recall and search while keeping content, citation and revision trail (and is reversible by `restore`, precisely because nothing was destroyed), while `delete_source` empties the indexed body and removes the source row, keeping only the audited fact that the entry existed and was deleted. A turn or artifact still referenced by a receipt, provenance edge, candidate or job has its content emptied but its row retained, and says so (`content_removed_source_retained`) rather than claiming a removal it did not perform. `src/export/` is new: `review.rs` owns the shared gate, `packet.rs` defines the continuation packet the docs previously only described in prose — one self-describing JSON document with `format_version`, stable ids, revisions, per-item and bundle checksums over a canonical (key-sorted) serialization, the audience it was reviewed for, and the opaque `anchor` that `docs/design/causal-observability.md` says a client passes back unchanged. Export is a three-state act: a `draft` bundle, an explicit audience review that pins the exact contents by digest, then release, which recomputes that digest and refuses if the contents moved. Import merges by stable id plus revision — created / unchanged / revision_advanced / skipped_stale — so a re-import is `unchanged` rather than a duplicate and never overwrites newer local content, and it re-runs the *local* sanitizer because "it was reviewed elsewhere" is a claim about another operator's judgement. Two deliberate limits, both to avoid overclaiming: a ported memory is recorded as `skipped_unreviewed` rather than written into `memories`, because docs/ARCHITECTURE.md "Memory approval" makes activation an owner act and no import may bypass it; and a packet carries reviewed sanitized evidence only, never exact original bytes (`src/archive/` owns those, encrypted) and no claim that importing it reproduces any past answer. Declared-files caveat: the work also touched `src/api/routes.rs`, `src/main.rs`, `docs/api.yaml`, `docs/ARCHITECTURE.md`, `tests/test_migrations.py` and `tests/test_sql_contracts.py` beyond the listed files, and `static/*` was left alone — no speculative UI was added, matching P15-T03's decision. Two latent defects were found and fixed on the way: `tests/test_sql_contracts.py` scraped shipped SQL by reading storage submodules in sorted order on the assumption that fixtures only lived in `storage.rs`, so the new `storage/history.rs` fixtures became the first match and the contract suite began executing a fixture with the wrong bindings; the scraper now cuts each file at `#[cfg(test)]`, which is a property of the source rather than of file order. And the `13` schema assertions were in four places, not one (`src/storage.rs` version guard, readiness projection, its test, and the `/health` test in `src/main.rs`) — exactly the site P15-T02 recorded as a latent bug.
- result-verify: `python3 tests/test_migrations.py` (001→014, `user_version=14`, data/FTS/FKs preserved, new `test_014_history_search_constraints`), `python3 tests/test_sql_contracts.py` (16 passed), `python3 tests/test_api_schema.py` (62 operations, 16 error codes, 22 receipt fields, 5 request states matched), `cargo test --locked` (276 passed, from 256 at task start; 11 new), `cargo clippy --locked --all-targets --all-features -- -D warnings` (clean), `cargo fmt --all -- --check` (clean), `bash scripts/verify_e2e.sh` (exit 0: E2E, denial, crash-recovery, cancellation and unsafe-retry all passed), `git diff --check` (clean). `python3 scripts/check_docs.py` was 0 failing before the commit and now reports exactly one FAIL, correctly: `deployment: live 7708e48 trails HEAD 518d8cc and code differs`. That is the gate doing its job -- this task was build-and-test only, so the running service still executes the previous binary at `user_version=13`. Advancing it is a deployment decision with the schema-aware rollback policy attached (docs/ARCHITECTURE.md: a failed candidate is auto-restored only when live `PRAGMA user_version` is no newer than the previous release), not a side effect of a verification run. Every new gate was proved load-bearing by tampering, each exiting non-zero: disabling the read-time sanitizer re-check (101), disabling the reviewed-digest comparison at release (101), disabling the unsanitized-item refusal (101), making `forget` also empty the body so it collapses into a source delete (101), renaming `history_documents` in the migration (1), removing the `source_deleted_at IS NULL OR body=''` CHECK (1, "a source-deleted document kept its body"), and dropping one new route from `docs/api.yaml` (1). The first pass of two of those tamper probes did *not* fail, which is why the tests changed: the digest check had no test that mutated a bundle after review, and the migration table set was only a subset assertion. Both were replaced with checks that do fail.

### P15-T05 · Ship the history, privacy and export review UI
- status: done
- priority: medium
- lane: memory-product
- parallel: yes
- depends: P15-T04
- design: docs/ROADMAP.md#p15-memory-and-history + docs/design/causal-observability.md
- files: static/*, tests/ui_smoke.cjs, scripts/verify_browser.sh
- done-when: a reviewer can search sanitized history and read each hit's citation; forget and source-delete are visibly different operations with different copy, different affordance and separate confirmation; and an export shows its exact item list and reviewed digest before any control that releases it exists.
- verify: scripts/verify_browser.sh && python3 tests/test_api_schema.py
- result: tracked as its own task rather than smuggled into P15-T04, and given a new id rather than `P15-T04b`, because it is new frontend work against an already-shipped and already-deployed backend, not a split of work that was in flight. P15-T04 deliberately added no speculative UI; this closes the gap its own "left open" note named. A `History & privacy` view in `static/index.html` carries three panels that are deliberately not one form. Search posts scope, optional session and optional kind to the existing `/history/search`, and renders every hit with its citation field by field — id, source row, revision, scope, session, timestamp, checksum, sanitizer — because a citation summarised into a sentence cannot be checked. The suppressed count from the read-time sanitizer gate is surfaced rather than hidden, so a reviewer sees that rows were withheld. The forget / source-delete boundary is the part most likely to mislead, so it is drawn as two panels with different left borders, different button classes, different copy and, for the destructive one, a separate `confirm()` naming the asymmetry ("Forget is reversible; this is not"). After a forget the panel still reads `content retained`, taken from the server's own `content_present` flag rather than inferred, and offers `Restore`; after a source delete it reads `content removed` and both controls disable, because a source-deleted entry has nothing left to forget. The recorded `history_privacy_events` trail is shown beneath, so the audited fact is visible and not just asserted. Export is three acts on screen in the order the backend enforces: assemble a draft, then the client immediately asks `review` with `approve: false` — a pure preview — so the exact item list and the digest that would pin it are rendered *before* any approve control, and the release control does not exist in the DOM until the bundle is `reviewed`. Approval sends back exactly the digest the reviewer was shown, so a bundle that moved after review is a conflict rather than a silent re-approval, and release is confirmed with a dialog that says the items will leave the machine. No server string is ever parsed as markup; every one reaches the DOM through the existing `node()` helper. Two deliberate limits, both to avoid overclaiming a capability: the export draft is assembled from the current search results rather than from per-row checkboxes, so "what will leave" is exactly what is on screen and cannot drift from it, and there is still no import UI — a ported memory needs local approval through the existing candidate flow, which is an owner act `docs/ARCHITECTURE.md` puts outside this surface.
- result-verify: `scripts/verify_browser.sh` (exit 0; `tests/ui_smoke.cjs` grew from 29 to 42 checks, 13 of them new, following P15-T02's naming precedent: `history_search_citations`, `history_forget_distinct_from_source_delete`, `history_forget_retains_content`, `history_source_delete_confirmed_and_irreversible`, `history_privacy_audit_trail`, `export_preview_before_release`, `export_digest_pinned_at_review`, plus the six P16-T01 incident checks). `python3 tests/test_api_schema.py` (exit 0; no new routes were needed, which is itself the evidence that this is a client for the P15-T04 surface rather than a second backend). `bash scripts/verify_local.sh` (exit 0, including its `javascript-syntax` and `mocked-browser` gates). Four gates were proved load-bearing by tampering, each exiting 1: making a forget also read as a content removal, offering the release control while the bundle is still a draft, dropping the search term from the query string so the panel filters locally instead of asking the server, and relabelling the `temporal_proximity` tag as a recorded dependency. Screenshots `history-privacy-desktop`, `history-privacy-mobile`, `export-review-desktop` and `incident-timeline-desktop` are written to `docs/qa/`, and the existing no-horizontal-overflow assertion covers each.

## P16 · Causal observability

### P16-T01 · Add incident search, timeline and expansion
- status: done
- priority: medium
- lane: incident-product
- parallel: yes
- depends: P10-T01
- design: docs/ROADMAP.md#p16-causal-observability
- files: src/storage.rs, src/main.rs, static/*
- done-when: reviewers expand bounded neighborhoods and search/filter by path/tool/relation/status/row while switching causal and chronological views.
- verify: cargo test --locked storage && scripts/verify_browser.sh
- result: no migration. Expansion, search, timeline and confidence labelling are all read-model concerns over columns migrations 003 and 006 already ship (`turn_steps.started_at`, `permission_requests.created_at`, `file_changes.created_at`, `activity_events.created_at`, `provenance_edges`), so the chain still ends at `014` and `user_version` is still 14. Adding a table to store a projection that can be recomputed would have been a cache with no invalidation story. The projection itself was *not* duplicated: `incident_graph` is now a one-line call into the new `incident_view(request_id, IncidentQuery)` with `IncidentQuery::default()`, and a test asserts that default reproduces the pre-P16-T01 response, so the read model a reviewer filters is byte-for-byte the read model the unfiltered endpoint returns — the same discipline P15-T02 recorded for the ranker and P15-T04 for sanitization. Row gathering moved into one `collect()` that both views and every filter read, so `path`, `tool`, `at` and `seq` are captured once rather than re-derived per feature. Filtering is server-side on purpose and the frontend never narrows a page locally: this client only ever holds a bounded page, so a local filter would have searched less than the reviewer believed while looking identical. Free-text search reuses `safety::fts_query` — the same normaliser history search uses — so punctuation and AND-matching mean the same thing in an incident query and a history query instead of two hand-rolled tokenisers drifting. Confidence has exactly three labels and each is earned, never inferred: `recorded_dependency` requires a recorded provenance edge, `temporal_proximity` means the row sat in the same request with a recorded time and *no* edge, and `unknown` means neither. `confidence_basis` names the evidence (`provenance_edge` / `same_request_recorded_time` / `no_evidence`), and the response ships the legend so a client cannot invent a fourth meaning for a label it received; the legend text says co-occurrence is not causation in as many words. The chronological view is a second ordering over the same selected nodes, not a second query, and a row with no recorded time is listed after the dated ones and counted in `timeline_undated` rather than sorted into the sequence by guesswork. Expansion passes the existing opaque `expansion_cursors` object back as `anchor`; an anchor whose `projection` is not `causal-neighborhood-v1` is refused rather than reinterpreted, and the break neighborhood is still seeded on every page so a later page cannot quietly describe a different incident. The request node always survives a filter, because a graph with no frame has nothing to expand from and the closed-graph guarantee outranks pagination tidiness — a filter matching nothing returns the frame alone and says the filter narrowed the recorded set rather than concluding anything. Declared-files caveat: the work also touched `src/api/routes.rs`, `src/storage/provenance.rs`, `docs/api.yaml` and `tests/recording_integration.py` beyond the listed files.
- result-verify: `cargo test --locked storage` (29 passed), `cargo test --locked incident` (4 passed: the pre-existing bounded-graph test plus three new ones for confidence, filters and the timeline), `cargo test --locked` (279 passed, from 275 at task start; 0 failed), `cargo clippy --locked --all-targets --all-features -- -D warnings` (clean), `cargo fmt --all -- --check` (clean), `scripts/verify_browser.sh` (exit 0, 42 checks including 6 new incident checks), `python3 tests/test_api_schema.py` (62 operations, 16 error codes, 22 receipt fields, 5 request states matched), `python3 tests/test_migrations.py` (001->014, `user_version=14`), `python3 tests/test_sql_contracts.py` (16 passed), `bash scripts/verify_e2e.sh` (exit 0), `bash scripts/verify_local.sh` (exit 0). `tests/recording_integration.py` caught a real regression rather than a cosmetic one: it pinned `bounds == {max_nodes, max_edges}` and the added `max_timeline` broke it, which is the assertion doing its job; it was updated and extended so the confidence contract, the filter contract and the timeline ordering are now asserted against the running server, not only in unit tests. Five gates were proved load-bearing by tampering, each exiting non-zero: labelling a proximity-only row as a recorded dependency (101), sorting undated rows into the dated sequence (101), accepting a hand-built expansion anchor (101), letting a filter drop the request frame (101), and treating an unsupported `view` as causal instead of refusing it (101). The undated-row probe did *not* fail on its first attempt — the fixture had no undated row — so the fixture was given a real same-scope `memories` endpoint (migration 006's trigger requires one) whose node carries no recorded time, and the test now asserts the fixture exercises that case. Honest negative: deleting the new query-parameter block from `docs/api.yaml` still exits 0, because `tests/test_api_schema.py` checks method+path pairs, error codes and receipt fields — not query parameters. The spec entry is therefore documentation, not a gate; closing that hole is left open below rather than claimed.

### P16-T02 · Compare runs and export causal graphs
- status: done
- priority: medium
- lane: incident-analysis
- parallel: yes
- depends: P16-T01
- design: docs/ROADMAP.md#p16-causal-observability
- files: src/storage.rs, src/main.rs, static/*
- done-when: two runs align by semantic step identity, show first divergence, and export bounded sanitized JSON/Graphviz with explicit dependency/contradiction/temporal/unknown labels.
- verify: cargo test --locked incident && scripts/verify_e2e.sh

### P16-T03 · Measure causal coverage and deployment incidents
- status: done
- note (2026-09-15, recovery): an autonomous turn stopped at its 900-second wall-clock budget after leaving a substantial uncommitted implementation directly on `main`. The work was checksummed and archived under `/root/development/scratch`, then isolated on branch `recovery/p16-t03` before review continued. Recovery fixed a stale SQL-contract migration list, two Python-lint findings, and an append-only trigger gap that had allowed deployment commit/hash/schema identity to be rewritten directly. Format, strict clippy, 292 Rust tests, migration/API/SQL/Python contracts, causal-coverage evidence, supply-chain checks, mocked browser suites and real browser-to-service E2E passed. No access configuration was changed.
- priority: medium
- lane: observability
- parallel: yes
- depends: P10-T03, P16-T01
- design: docs/ROADMAP.md#p16-causal-observability
- files: migrations/*, src/storage.rs, src/main.rs, scripts/deploy.sh, static/*
- done-when: missing-edge, earliest-break, graph-size and reviewer-time metrics plus anomaly flags and build/deploy/restart/smoke provenance are durable, bounded and evidence-labeled.
- verify: python3 tests/test_migrations.py && scripts/verify_e2e.sh

### P16-T04 · Measure request runtime stages with a synthetic baseline
- status: done
- priority: high
- lane: runtime-observability
- parallel: no
- depends: P11-T04, P16-T03, P18-T05
- design: TASK.md#14-first-recommended-implementation-slice, docs/design/runtime-observability.md
- files: docs/design/runtime-observability.md, src/runtime_observability.rs, src/main.rs, src/recording.rs, src/agent_loop.rs, docs/TASKS.md, docs/PROGRESS.md
- done-when: one deterministic synthetic-provider turn produces bounded `harness.runtime/v1` timing evidence for total generation, context construction, provider waits, tool execution, permission waiting, verification and durable publication; unavailable measurements remain explicit; emitted evidence contains no prompt, response, token, key, path, tool arguments, tool output or hidden reasoning; repeated fixture samples report count, median, p95 and maximum; existing recording, cancellation, fencing, spend and no-replay behavior is unchanged.
- verify: cargo test --locked runtime_observability && cargo test --locked agent_loop && bash scripts/verify_release.sh
- result: bounded content-free `harness.runtime/v1` terminal evidence now records monotonic total, context, provider, tool, permission, verification and durable-publication timings with provider/tool call counts; SQLite queue/read/write/commit decomposition is explicitly unavailable rather than estimated. Focused tests passed (2 runtime-observability, 26 agent-loop), followed by the strict release gate (345 Rust tests plus contract, clippy, release, mocked-browser, real browser-to-server and documentation suites; 0 failures). Commit `3694eb2` was then backed up, restore-drilled, deployed and verified on production at schema 20 with matching served/live identity, ready workers, healthy SQLite, empty queues and zero service restarts.

### P16-T05 · Collect a repeatable synthetic runtime baseline
- status: done
- priority: high
- lane: runtime-observability
- parallel: no
- depends: P16-T04
- design: TASK.md#14-first-recommended-implementation-slice, docs/design/runtime-observability.md#repeatable-baseline
- files: scripts/runtime_baseline.py, tests/test_runtime_baseline.py, docs/evidence/runtime-baseline.json, docs/design/runtime-observability.md, docs/TASKS.md, docs/PROGRESS.md
- done-when: a real compiled Harness process, disposable SQLite database and loopback synthetic provider produce at least 25 sequential short-chat samples; the artifact pins commit and fixture digest, reports count/median/p95/max and median total share for every measured stage, reports errors/timeouts, DB/WAL delta, CPU and peak RSS, preserves explicit unavailable measurements, contains no request content or credentials, and identifies the largest measured median without inventing an optimization threshold.
- verify: python3 tests/test_runtime_baseline.py && python3 scripts/runtime_baseline.py --samples 25 --provider-delay-ms 20 --output docs/evidence/runtime-baseline.json && bash scripts/verify_release.sh
- result: 25 real-server synthetic turns completed with zero errors/timeouts. Total median/p95/max was 82/99/102 ms; provider wait was the largest measured stage at 59 ms median (72% of total median), with verification at 32 ms, context at 6 ms and publication at 4 ms. The artifact pins commit `470691a`, fixture digest, binary identity, resource evidence and explicit SQLite decomposition limits. The strict release gate passed every executable lane; its deployment-identity check is closed by the verified promotion recorded below.

### P16-T06 · Disaggregate provider latency by call purpose
- status: done
- priority: high
- lane: runtime-observability
- parallel: no
- depends: P16-T05
- design: docs/design/runtime-observability.md#repeatable-baseline
- files: src/runtime_observability.rs, src/agent_loop.rs, scripts/runtime_baseline.py, tests/test_runtime_baseline.py
- done-when: provider wait is split into bounded answer and verification-call measurements without content or identifiers, totals remain internally consistent, and repeated evidence shows which call purpose is the real optimization target before changing provider behavior.
- verify: cargo test --locked runtime_observability && python3 tests/test_runtime_baseline.py && bash scripts/verify_release.sh
- result: The aggregate provider total remains backward-compatible and is now exactly decomposed into bounded answer and verification durations/call counts. A pinned 25-turn real-server synthetic baseline completed with zero errors/timeouts: answer wait was 31/47/55 ms median/p95/max versus verification wait at 29/37/50 ms, with one call of each purpose per turn. Answer wait is the larger synthetic target, but its 2 ms median lead is too small to justify live-provider behavior changes without live-safe evidence.

## P17 · Memory Wind Tunnel

### P17-T01 · Define and validate immutable run capsules
- status: done
- priority: research
- lane: experiment-contract
- parallel: yes
- depends: P12-T03, P15-T02
- design: docs/design/memory-wind-tunnel.md#m0-experiment-contract
- files: docs/design/memory-wind-tunnel.md, src/experiments/*, migrations/*
- done-when: content-addressed capsules declare project/model/tool/memory/context state, assertions, unavailable evidence and strict/live/hybrid semantics; validation rejects incomplete nondeterministic boundaries.
- verify: python3 tests/test_migrations.py && cargo test --locked capsule
- result: `harness-run-capsule-v1` is a canonical-JSON, SHA-256-addressed manifest for sanitized task/history evidence, clean project identity, exact model parameters, ordered tool requests/results and permission mode, stable memory revisions, context receipt, deterministic assertions, and explicit clock/randomness/provider/tool boundaries. Strict mode accepts only complete frozen/deterministic boundaries; live mode requires an explicitly live provider; hybrid mode pins a strict prefix and first live boundary. Missing evidence is represented as a matching `unavailable` boundary/evidence pair rather than a zero. Migration `016_run_capsules.sql` stores validated manifests under their address and rejects update/delete; no replay, provider call, tool execution, memory activation, worktree mutation or infrastructure change was added.
- result-verify: exact migration chain 001->016 passed and all 3 capsule tests passed. The full 298-test Rust suite, strict Clippy, formatting, SQL migration contracts and API schema checks also passed. `scripts/check_docs.py` has only the pre-existing intentional deployment-truth failure because production remains at `6423d5a`; this task was not deployed.

### P17-T02 · Freeze and fork isolated treatments
- status: done
- priority: research
- lane: experiment-isolation
- parallel: no
- depends: P17-T01
- design: docs/design/memory-wind-tunnel.md#m1-freeze-and-fork
- files: src/experiments/*, scripts/capsule.py
- done-when: clean project and approved-memory snapshots fork immutable children without changing live DB/worktree; remove-one and no-memory treatments are supported.
- verify: cargo test --locked treatment && scripts/verify_e2e.sh
- result: Added content-addressed `harness-treatment-v1` children with detached clean project worktrees and per-treatment sealed SQLite stores. Freezing accepts only validated strict capsules, checks the source HEAD and raw Git-tree SHA-256, copies only the exact active memory revisions declared by the capsule, and supports baseline, remove-one, and no-memory conditions. Existing destinations, in-source output paths, stale memory revisions/content, dirty projects, commit/tree drift, and post-freeze source drift fail closed. The live Harness database, approved memories, source worktree, provider access, production, and infrastructure are not modified.
- result-verify: Exact task gate passed: 2 treatment tests plus the complete browser-to-Rust-to-SQLite-to-filesystem E2E and all failure-path lanes. The full 300-test Rust suite, strict Clippy, formatting, Python lint/compile, capsule-script freeze smoke test, migration chain 001->016, 16 SQL contracts, API schema, and diff checks also passed. This task was not deployed.

### P17-T03 · Implement strict replay and first-divergence reports
- status: done
- priority: research
- lane: experiment-replay
- parallel: yes
- depends: P17-T02, P16-T02
- design: docs/design/memory-wind-tunnel.md#m2-strict-memory-wind-tunnel
- files: src/experiments/*, static/*, tests/capsules/*
- done-when: strict replay makes zero live provider/tool calls and validates deterministic pipeline integrity. Recorded responses are matched to the actual request boundary; changed context or request stops at the first divergence and marks downstream behavioral evidence unavailable instead of reusing the original response as a counterfactual outcome.
- verify: cargo test --locked replay && scripts/verify_e2e.sh
- result: Added content-addressed strict replay tapes and replay reports linked to one validated strict capsule and isolated treatment. Provider transcripts are bound to the capsule provider-boundary digest, and every recorded tool result is matched to its exact ordered tool name, request digest and result digest. Exact context/request matches replay recorded results offline; the first changed, missing or extra boundary withholds that old result and marks every downstream step unavailable. The replay core exposes no provider, tool-registry, shell, network or filesystem-write client and reports structurally zero live calls. A checked-in request fixture proves deterministic report bytes and identity.
- result-verify: Exact task gate passed with 7 replay-matching tests and the full browser-to-Rust-to-SQLite-to-filesystem E2E/failure-path suite. The full 305-test Rust suite, strict Clippy, formatting, migration chain 001->016, all 16 SQL contracts, API schema and diff checks also passed. This task was not deployed.

### P17-T04 · Add live treatments, budgets and statistics
- status: done
- priority: research
- lane: experiment-live
- parallel: yes
- depends: P17-T03, P14-T04b
- design: docs/design/memory-wind-tunnel.md#m4-live-variance-and-research-reports
- files: src/experiments/*, scripts/experiment.py, static/*
- done-when: stale/conflict/pollution/poison treatments run repeatedly under hard spend/token/action limits and report paired outcomes and uncertainty rather than single-run causality.
- verify: cargo test --locked experiment && python3 scripts/experiment.py --fixture --check
- result: Added content-addressed live-treatment manifests for older same-memory revisions, distinct conflicting memories, irrelevant-similar pollution, and fixture-only poisoned memories. Poisoned treatment creation is refused unless the capsule names the deterministic fixture provider. An atomic pre-dispatch ledger enforces positive hard trial/request/input-token/output-token/micro-USD/action ceilings, refuses unknown cost and overflow, and leaves usage unchanged on refusal. Paired analysis aligns stable pair IDs, excludes unavailable deterministic outcomes explicitly, reports success rates, paired mean effects and descriptive 95% intervals, and stays inconclusive for one pair. The executable fixture runner enables only explicit no-network fixture mode and exercises all four conditions through 32 budgeted trial admissions.
- result-verify: Exact task gate passed with 14 experiment-focused tests and `scripts/experiment.py --fixture --check`. The full 309-test Rust suite, strict all-feature Clippy, formatting, Python lint/compile, migration chain 001->016, all 16 SQL contracts, API schema and diff checks also passed. No live provider call or deployment occurred.

### P17-T05 · Export sanitized research reports and optional remote runs
- status: done
- priority: research
- lane: experiment-reporting
- parallel: yes
- depends: P17-T04, P13-T04
- design: docs/design/memory-wind-tunnel.md#m5-optional-upcloud-isolation
- files: src/experiments/*, scripts/remote_runner.py, static/*
- done-when: reports link sanitized evidence and limitations; remote runs require explicit opt-in, pinned image/size/region, TTL, spend cap, kill switch and confirmed cleanup.
- verify: python3 scripts/remote_runner.py --fixture --check && scripts/verify_e2e.sh
- result: Added content-addressed, review-gated research reports that reuse the shared sanitizer, project paired statistics, link available evidence by kind/stable ID/SHA-256, preserve explicit unavailable evidence reasons, and require limitations without embedding raw experiment inputs or outputs. Added an executable fixture-only remote plan validator that refuses absent opt-in, unpinned image/region/size, invalid TTL or spend cap, missing kill switch, unsanitized input, and unconfirmed cleanup. The fixture simulates a full lifecycle ending in a matching destroyed confirmation and reports zero network, provider and cloud API calls; this build exposes no actual remote transport.
- result-verify: Exact task gate passed: deterministic remote fixture validation performed nine fail-closed admission checks and the complete real browser-to-Rust-to-SQLite-to-filesystem E2E plus denial, crash-recovery, cancellation/retry and unsafe-retry lanes passed. Two report tests and the full 311-test Rust suite passed, along with strict all-feature Clippy, formatting, Python lint/compile, migration chain 001->016, all 16 SQL contracts, API schema and diff checks. No remote resource, deployment, provider call, access or infrastructure change occurred.

## P18 · Optional platform evolution

### P18-T01 · Design leased multi-worker execution
- status: done
- priority: low
- lane: scale
- parallel: no
- depends: P10-T02, P11-T02, P17-T03
- design: docs/ROADMAP.md#p18-optional-platform-evolution
- files: docs/ARCHITECTURE.md, migrations/*, src/recording.rs, migrations/017_worker_leases.sql, docs/design/leased-multi-worker.md, tests/test_migrations.py
- done-when: an approved design defines leases, fencing, recovery and no-duplicate-side-effect semantics before any second worker or instance is enabled.
- verify: cargo test --locked recovery && python3 tests/test_migrations.py
- note (2026-09-16, design approved by the owner, fencing schema landed): the design in
  `docs/design/leased-multi-worker.md` is approved. The owner chose **Resolution B** (multiple
  writers) over the recommended A, stating that many writers are acceptable "or soon" and that the
  hard constraint is no race conditions, with the system robust, fast and optimized. That answers
  both blocking questions: multi-worker is wanted, and the single-writer `process_lock` guarantee
  will be narrowed to worker identity rather than kept. Because B moves correctness entirely onto
  leases plus fencing, the fencing schema was landed first, under rollout gate 2, with the worker
  count still pinned at one: `migrations/017_worker_leases.sql` adds `worker_leases` with a
  per-request monotonic fence and four triggers that refuse a fence decrease, a handover that
  reuses a fence, a row delete (which would reset the fence and re-authorize a pre-crash writer),
  and moving a lease between turns. Enforcement lives in the schema, not in worker code, so a
  stalled or buggy worker cannot bypass it. No second worker is enabled and no worker identity
  exists yet; rollout gates 3 and 4 remain, and Resolution B still owes evidence on SQLite write
  contention and busy-timeout behaviour before a second writer runs.
- result-verify: Migration chain 001->017 applies with user_version=17 and data, FTS and foreign
  keys preserved; the full 327-test Rust suite passes, including the recovery and cancellation
  lanes named by this task's verify command. Each of the four lease triggers was mutation-tested by
  dropping it and confirming the write it guards then succeeds, which caught two assertions that
  had been passing for the wrong reason (a `fence > 0` CHECK and a foreign key) instead of
  exercising the trigger under test. No second worker was enabled and no concurrent writer ran.

### P18-T02 · Add portable providers, plugins and benchmark packs
- status: done
- priority: low
- lane: ecosystem
- parallel: yes
- depends: P12-T03, P13-T04, P17-T03
- design: docs/ROADMAP.md#p18-optional-platform-evolution
- files: docs/*, src/plugins/*, benchmarks/*
- done-when: digest-pinned bounded extension contracts, provider adapters, portable continuation packets and checked-in regression capsules work without weakening tool permissions or audience review.
- done-when-change: the original wording said "signed". Owner decision on 2026-09-16 was to pin manifest digests instead of signing, because there are no third-party extensions and a signature would only prove the single key in this repo signed it, while a pin proves the property actually wanted -- the bytes admitted are the bytes reviewed. Manifests carry a `harness.extension/v1` schema string so a later v2 can add signatures without reinterpreting a v1 manifest. Signing is deferred, not delivered.
- verify: bash scripts/verify_release.sh
- result: Added a digest-pinned extension contract covering provider adapters, tool plugins and benchmark packs. A manifest declares schema, id, version, kind, capabilities, requested host tools, permission mode and payload files; benchmarks/pinned.json records the SHA-256 of the exact reviewed manifest bytes. Admission is fail-closed and returns the first refusal with a stable code, refusing unsupported schema, malformed id or version, unpinned extensions, bytes that drift from the pin, empty or unlisted capabilities, host tools the live registry does not offer, a permission mode wider than the host session, any network grant, payload paths that escape the repository or carry malformed digests, and benchmark packs whose audience review is missing, incomplete or attests a different payload set. Reviews attest the payload set rather than the manifest digest, since a review stored in a manifest cannot attest the bytes containing it. Shipped one benchmark pack pinning the existing strict-replay and remote-runner fixtures, reusing those bytes in place rather than copying them. Enforcement is split deliberately: scripts/check_extensions.py validates the checked-in corpus and parses the capability allowlist and schema string out of the Rust source so the two cannot drift, while host tool admission is asserted only in Rust against the real tool registry because a Python copy of the tool list would rot. Portable continuation packets were already delivered by the export/import packet surface on main and are pinned here only as a declarable capability, not reimplemented. The module is contract plus tests; no runtime loader calls it yet, so it carries a narrow documented allow for the binary-crate dead-code lint, matching the existing pattern in src/tools/mod.rs.
- result-verify: Sixteen extension tests passed, covering admission of a pinned bounded pack and of a provider adapter without a review, plus every refusal lane including a manifest edited after pinning, a payload swapped beneath an intact review, a tool absent from the registry, a widened permission mode, a network grant, a path escaping the repository and malformed JSON. Two cross-module drift tests assert the permission-mode names against the real tools registry and the pin-ledger kind vocabulary against the checked-in ledger. The offline extension checker passed. The full Rust suite, strict all-feature Clippy, formatting, the real browser-to-Rust-to-SQLite E2E and documentation-claims lanes all passed. No provider, network or cloud call was made and no infrastructure changed.

### P18-T03 · Record external effects durably with an idempotency key
- status: done
- priority: low
- lane: scale
- parallel: no
- depends: P18-T01
- design: docs/design/leased-multi-worker.md
- files: migrations/018_external_effects.sql, src/storage/effects.rs, src/storage.rs, src/memory_agents.rs, src/main.rs, tests/test_migrations.py, tests/test_sql_contracts.py, docs/design/leased-multi-worker.md, docs/ARCHITECTURE.md
- done-when: every non-replayable external effect is recorded before it is attempted and settled after, keyed by the fence-independent idempotency key named in the design; an effect whose outcome is unknown is durably `unknown` and never auto-retried; a restart sweep records unknown rather than replaying; and the schema refuses a second attempt under the same key from a different fence.
- verify: cargo test --locked && python3 tests/test_migrations.py && python3 tests/test_sql_contracts.py
- done-when-change (2026-09-16): the original wording said *every* non-replayable external
  effect. Scope is narrowed to provider dispatch plus a written, enforced enumeration of the rest,
  and the once-only tool effects (`bash`, `browser`) move to P18-T04. Reason: recording a tool
  effect without a real fence records *that* something happened but not *which* holder did it, so
  the record could not refuse a duplicate after a steal -- the property the record exists for. The
  set is not left implicit: the classification table in the design doc names every candidate and
  `tests/test_sql_contracts.py::ExternalEffectCoverage` fails if a new provider dispatch site
  appears without a record, or if an exemption outlives the function it names.
- result: Provider dispatch now reserves a durable `external_effects` row before the request can
  leave the process and settles it after, on both the buffered and streamed paths; a restart sweeps
  still-reserved rows to `unknown` next to the existing `provider_calls` sweep. Settlement is
  deliberately asymmetric: a request answered unusably settles `succeeded` with the error as its
  reason because it still happened and may be billed, while one that never visibly left settles
  `unknown`, never `failed`, because an unacknowledged send may still have arrived. Coverage is the
  part that makes the record worth anything, so every candidate effect is enumerated and classified
  in the design doc -- provider calls recorded, `GET /models` exempt as a replayable read, the
  content-addressed write/edit/archive/export paths converging on replay, `bash` and `browser`
  named as the sharpest unrecorded hole, and the outbox flush identified as an internal write
  covered by fencing rather than by this table. A provider call that cannot be attributed to a
  recorded turn stays exempt rather than being given a synthetic receipt, because fabricated
  evidence in the table an operator reads to decide whether an effect happened is worse than a
  documented gap; the refusal is placed in P18-T04 where a worker has identity. The fence written
  today is still the `SINGLE_WORKER_FENCE = 1` placeholder, which is why steal-time deduplication
  is P18-T04's, not this task's, claim.
- result-verify: The three-test `ExternalEffectCoverage` contract passed and was mutation-checked --
  removing the `reserve_effect` call from `stream_turn` made it fail with that function named, and
  the source was restored afterwards. `python3 tests/test_sql_contracts.py` passed 19 tests,
  `python3 tests/test_migrations.py` walked 001 to 018 with `user_version=18` and data, FTS and
  foreign keys preserved, and the full Rust suite passed 329 tests under strict all-feature Clippy
  and formatting. The documentation-claims gate passed with zero failing checks and the live binary
  matching HEAD. No provider call was made; the effect paths are exercised against a file-backed
  temporary database.
- note (2026-09-16, Rust half wired): `reserve_external_effect`/`settle_external_effect` land in
  `src/storage/effects.rs` and both provider dispatch paths in `src/memory_agents.rs` now reserve
  before dispatch and settle after; migration 018 joined the chain and schema version moved to 18.
  Kept `doing` because `done-when` says *every* non-replayable effect and only provider calls are
  wired: the other candidate effects are not yet enumerated or classified, a provider call outside a
  recorded turn is skipped rather than recorded (the row references `chat_receipts`), and the fence
  is still the documented `SINGLE_WORKER_FENCE = 1` placeholder until P18-T04 supplies a lease.
- note (2026-09-16, opened from the Decision 4 record): P18-T01 closed on an approved design only, so the
  no-duplicate-side-effect semantics it defines have no implementation and no task. This is that task.
  It is sequenced before the contention measurement because the "steal after death with no duplicate
  external effect" failure-mode test cannot be written until the effect record exists.

### P18-T04 · Carry and verify the fence on every durable turn write
- status: done
- priority: low
- lane: scale
- parallel: no
- depends: P18-T03
- design: docs/design/leased-multi-worker.md
- files: src/recording.rs, src/recording_sql.rs, src/storage.rs, tests/fault_injection.py
- done-when: every durable turn write carries its lease fence and is refused in the same transaction when the fence is stale; and the five lease failure modes are tested — heartbeat renewal under contention, expiry-then-resume refused on fence, steal after death with no duplicate external effect, cancellation at a non-holder, and clock skew via database-issued times.
- verify: cargo test --locked recovery && python3 tests/fault_injection.py
- note (2026-09-16, inherited from P18-T03): also owns the effects that could not be honestly
  recorded without a real fence -- the once-only `bash` and `browser` tool effects and
  `delete_archive`, replacing `SINGLE_WORKER_FENCE = 1` with the lease's fence, and refusing an
  out-of-turn provider dispatch in multi-worker mode instead of exempting it. The classification
  table in the design doc is the checklist.
- note (2026-09-16, slice 1 landed; stopping before the steal path): worker identity, lease
  acquisition inside the claim transaction, heartbeat renewal, release, and an in-transaction
  fence check on the three durable turn writes (`save_recording_context`, `complete_recording`,
  `fail_recording`) are wired in `src/storage/leases.rs` and `src/recording.rs`. The lease is
  taken in the same transaction as the claim, because a claim that committed without a lease
  would leave a turn whose state says it is being worked on and whose ownership says nobody
  holds it. Times are SQLite-issued so a skewed worker clock cannot manufacture or revoke
  ownership, and the expiry-then-resume failure mode is tested and mutation-checked: removing
  the lapse refusal makes the test fail.
  Kept `doing` because `done-when` says *every* durable turn write and all five failure modes.
  Still owed: the steal path (a lapsed lease held by another worker is refused today, not
  taken), the remaining four failure-mode tests, the honest gap that a write arriving with no
  remembered lease -- HTTP cancellation and the restart recovery sweep -- proceeds unfenced,
  and replacing `SINGLE_WORKER_FENCE = 1` with the lease fence.
- note (2026-09-16, slice 2 landed; takeover): `steal_in_tx` in `src/storage/leases.rs` is now the
  only path that moves a lease between workers, and it is refused unless the lease has actually
  lapsed -- taking a live worker's turn is a race, not recovery. Wiring it into `claim_recording`
  also fixed a regression slice 1 introduced: a turn the restart sweep resets to `captured` still
  carries the dead worker's lease row, so a fresh process was never the holder and the turn was
  permanently unclaimable.
  The safety gate is the external-effect record, not the lease. If the dead holder left a
  reservation unsettled, nobody can say whether that effect reached the outside world, so the
  reservation is swept to `unknown` with reason `lease_lapsed_with_effect_in_flight`, the steal is
  refused, and the turn waits for a human -- decision 4 applied literally, no auto-retry. The sweep
  and the refusal commit together because a sweep without the refusal, or a refusal without the
  sweep, would each mislead the operator. Mutation-checked: removing the in-flight refusal makes
  the test fail.
  Two of the five failure modes are now covered (expiry-then-resume, steal after death with no
  duplicate external effect). Still owed: heartbeat renewal under contention, cancellation at a
  non-holder, clock skew as its own test, the unfenced no-remembered-lease paths, fencing *every*
  durable turn write, and replacing `SINGLE_WORKER_FENCE = 1` with the lease fence.
- note (2026-09-16, slice 3 landed; failure-mode tests): all five lease failure modes now have
  executable coverage. Heartbeat renewal contends with a separate SQLite writer holding
  `BEGIN IMMEDIATE`, waits inside the busy timeout, advances its database-issued timestamp, and
  keeps the same holder and fence. Cancellation is issued by a store that neither holds nor
  remembers the lease and still records exactly one terminal cancellation without moving
  ownership or creating an external effect. Clock skew is tested by bracketing acquire and renew
  with SQLite UTC, rejecting deliberately absurd worker-time bounds as persisted values, and
  proving that only database-issued expiry changes liveness and permits takeover.
  P18-T04 remains `doing`: test coverage now satisfies the five-mode half of `done-when`, but the
  no-remembered-lease writes, every-durable-write audit, real effect fence, once-only tool effects,
  `delete_archive`, and multi-worker out-of-turn provider refusal are still open. Worker count
  remains pinned at one.
- note (2026-09-16, slice 4 landed; provider effects carry the lease): the
  `SINGLE_WORKER_FENCE = 1` production placeholder is removed. Provider-effect reservation now
  presents the lease this worker actually acquired and checks it in the same transaction as the
  insert; settlement carries that same remembered lease and guards it in the same transaction as
  the update. A generating turn with no remembered lease is refused before an effect row or network
  dispatch. Post-turn extraction remains explicitly outside the turn-effect ledger while the
  process count is one. Tests prove the stored fence is the acquired fence, a missing lease creates
  no reservation, and a stale holder cannot reserve or settle after ownership moves. Both new fence
  guards were mutation-checked and the tests failed when either guard was removed.
  P18-T04 remains `doing`: once-only `bash`/side-effecting-browser effects, partial-write reporting,
  `delete_archive`, the remaining durable-write audit, and the multi-worker out-of-turn policy are
  still open. Worker count remains pinned at one.
- note (2026-09-16, slice 5 implemented; tool effects): every `bash` call and browser
  `click`/`type`/`press` now reserves a fence-independent external-effect identity before dispatch
  under the held lease and settles it before the durable step. Browser `open`, `snapshot`,
  `screenshot`, and `close` remain outside the ledger. Migration 019 widens the enforced kind set
  without discarding existing effect history. Focused tests prove the classification and an actual
  bash reservation/settlement; the full 340-test suite, strict Clippy, migration/SQL contracts,
  recovery test, and fault injection pass. P18-T04 remains `doing`: partial filesystem receipts,
  `delete_archive`, the remaining durable-write audit, and multi-worker out-of-turn refusal remain.
  Worker count remains pinned at one.
- note (2026-09-16, slice 6 implemented; archive deletion outcome): `delete_archive` now records
  `delete_archive_intent` before the ciphertext remove, then records either
  `delete_archive_succeeded` or `delete_archive_missing` before preserving the legacy
  `delete_archive` event for existing readers. Migration 020 widens the append-only privacy-event
  action constraint without discarding history, and focused tests cover normal deletion,
  idempotence, append-only privacy events, and missing-ciphertext outcome reporting. Evidence:
  `cargo fmt`, `cargo test --locked archive`, `python3 tests/test_migrations.py`, and
  `python3 tests/test_sql_contracts.py` pass. P18-T04 remains `doing`: partial filesystem receipts,
  the remaining durable-write audit, and multi-worker out-of-turn refusal remain. Worker count
  remains pinned at one.
- note (2026-09-16, slice 7 implemented; out-of-turn provider policy): the multi-worker
  provider policy is now an opt-in fail-closed guard, `HARNESS_MULTI_WORKER_PROVIDER_POLICY=refuse_out_of_turn`.
  With that policy enabled, a provider dispatch that has a spend store but no task-local request id
  is refused before `provider_calls` reservation, so it cannot fabricate a synthetic turn or leave a
  misleading spend/effect row. The existing single-worker exemption remains the default. Focused
  evidence: `cargo fmt`,
  `cargo test --locked memory_agents::provider_tests::multi_worker_policy_refuses_out_of_turn_provider_dispatch_before_spend`,
  and `cargo test --locked storage::tests::external_effects_are_recorded_before_dispatch_and_swept_to_unknown_on_restart`
  pass. P18-T04 remains `doing`: partial filesystem receipts and the remaining durable-write audit
  remain. Worker count remains pinned at one.
- note (2026-09-16, slice 8 landed; audit closed): the remaining durable-write audit found no
  worker-owned turn write without either the remembered lease and same-transaction `guard_fence`, or
  an explicit non-holder control-plane exemption. Step begin/finish, activity writes, permission
  request/expiry, provider-effect reservation/settlement, context save, answer completion, failure
  verdicts, once-only tool effects, archive deletion outcomes, and failed LSP residual artifacts are
  fenced or recorded truthfully. `request_cancellation`, `finalize_cancellation`, restart recovery,
  and outbox flushing remain non-holder control-plane paths and do not impersonate a worker-held
  fence. The partial filesystem receipt item is closed by the LSP residual artifact path: every file
  still changed after a failed rollback is returned as `Artifact::FileChange` and persisted by
  `finish_step` under the held fence. Evidence: `cargo fmt`, `cargo test --locked recovery`,
  `cargo test --locked storage::tests::durable_steps_without_a_remembered_lease_write_nothing storage::tests::a_stale_remembered_lease_cannot_finish_a_durable_step storage::tests::cancellation_at_a_non_holder_is_honored_without_moving_the_lease`,
  `cargo test --locked lsp_tool::tests::workspace_rename_records_a_residual_file_when_rollback_fails`,
  `python3 tests/fault_injection.py`, and `git diff --check` pass. P18-T04 is done; worker count
  remains pinned at one until P18-T05 measures SQLite write contention before a second writer runs.

### P18-T05 · Measure SQLite write contention before a second writer runs
- status: done
- priority: low
- lane: scale
- parallel: no
- depends: P18-T04
- design: docs/design/leased-multi-worker.md
- files: docs/design/leased-multi-worker.md, docs/PROGRESS.md, tests/fault_injection.py
- done-when: measured SQLITE_BUSY rates and busy-timeout behaviour under real concurrent writers are recorded as evidence, rollout gate 3 is decided on those numbers rather than on expectation, and the worker count stays pinned at one until they exist.
- verify: bash scripts/verify_local.sh
- note (2026-09-16, completed): `tests/fault_injection.py` now measures real WAL writer contention
  with two SQLite processes. The holder keeps `BEGIN IMMEDIATE` open while a contender with the
  production 5s busy timeout attempts its own `BEGIN IMMEDIATE`; the contender is still blocked
  during the held contention window, then commits after release before the timeout. Rollout gate 3
  is therefore decided from measured serialization behavior rather than expectation: a second
  writer can only be enabled behind explicit opt-in, and worker count remains pinned at one until
  that configuration change and rollback path land. Evidence: `python3 tests/fault_injection.py`,
  `git diff --check`, and `bash scripts/verify_local.sh` pass.


## P19 · development-mcp history integration

Approved direction: development-mcp records server-observed activity locally and exports it durably to Harness; Harness owns external history and reviewed memory. Conversation text is included only when a supported client supplies it. External ingestion never invokes the agent loop or replays development actions. These are backlog entries, not authorization to execute implementation or deploy. Existing release blockers retain precedence. Paths marked `(new)` are planned artifacts; verification commands describe completion gates, not tests already passed. Cross-repository tasks are coordinated with `/root/workspace/development-mcp`; each task remains a separate branch/worktree and schema changes remain serialized.

### P19-T01 · Establish the external history contract and fixtures
- status: done
- priority: medium
- lane: history-contract
- parallel: no
- depends: —
- design: docs/TASKS.md#p19--development-mcp-history-integration
- files: docs/design/external-development-history.md (new), tests/external_history/ (new), docs/RECORDING_PROTOCOL.md
- done-when: the decided integration is expressed as a versioned contract with representative accepted/rejected fixtures covering event identity, producer identity, project/session/invocation/task correlation, sequence, timestamps, sanitized arguments/results, artifacts, capture coverage, errors and terminal outcomes. Duplicate versus conflicting submissions, acknowledgements after durable commit, late events, unsupported versions and missing conversation context have deterministic meanings. Contract tests reject malformed envelopes and distinguish normal handler return from successful execution; no architecture selection or provider generation is introduced.
- verify: cargo test --locked && python3 tests/test_external_history_contract.py
- note: Implemented 2026-09-21 on `task/p19-t01-external-history`. The contract is pinned in `docs/design/external-development-history.md` (116 lines) as schema `harness.external-history/v1`, with `docs/RECORDING_PROTOCOL.md` linking to it and stating that fixture conformance is not evidence of authentication, redaction, durable commit or network delivery. `tests/test_external_history_contract.py` (259 lines, 18 tests) is a dependency-free offline validator over 27 fixtures — 17 accepted covering all 14 event types, 10 rejected covering 6 declared codes (`malformed_envelope`, `missing_required_field`, `unsupported_schema_version`, `unknown_event_type`, `invalid_timestamp`, `duplicate_event_id_conflict`). `outcome.transport` is kept independent of `outcome.execution`, so a handler that returns normally never implies the command succeeded; identity is `(producer_id, event_id)` with a canonical-JSON SHA-256 `content_digest`, so an identical retry is idempotent while conflicting reuse is rejected. Late arrival, crash-window `unknown`, capture gaps and explicitly unavailable conversation are accepted without inventing dialogue.
- note: During finalization the `artifact.recorded` accepted fixture carried its metadata nested in `payload.artifacts[0]`, while the validator and `test_artifact_metadata_is_required` require `artifact_id`, `media_type`, `byte_count`, `digest` and `truncated` at payload top level, per the design doc's "Artifact references carry artifact_id, media_type, byte_count, digest and truncated". The fixture was corrected to the documented shape (`artifacts` kept as an empty reference list, consistent with the other 16 accepted fixtures) and its `content_digest` recomputed; no validator or contract rule was weakened to make the suite pass.
- limitations: Offline structural conformance only. No ingestion endpoint, migration, storage, authentication, redaction, archive or exporter exists yet — those are P19-T02/T03/T05, and nothing here is wired into the running service or deployed. Rust sources are untouched by this task, so `cargo test --locked` is a regression gate rather than coverage of new Rust behaviour. Producer-scoped authorization and privacy policy are unimplemented and intentionally out of scope.

- result-verify: 2026-09-21: `cargo test --locked && python3 tests/test_external_history_contract.py` passed (345 Rust tests, 18 contract tests); final saved logs `/tmp/p19-final-rust.log` and `/tmp/p19-final-contract.log`. Fresh contract rerun passed. Recording contracts (25), SQL contracts (19), migration chain through 020 and `git diff --check` passed. Deployment identity remains separate and has not been promoted.

### P19-T02 · Enforce external producer scope and recording privacy
- status: done
- priority: medium
- lane: history-security
- parallel: no
- depends: P19-T01
- design: docs/design/external-development-history.md
- files: src/api/auth.rs, src/safety.rs, src/storage/scope.rs, src/archive/, tests/external_history/, tests/test_external_history_contract.py (new)
- done-when: authenticated producers are bound to permitted project scopes; ingestion authority does not grant history read, memory approval or archive access. Policy covers arguments, titles, paths, results, errors and output before persistence/export. Secret-bearing fixtures do not leak into sanitized records, diagnostics or indexes. Exact-content archives remain opt-in and encrypted; retention/deletion rules include artifacts and derived memories. Untrusted identifiers cannot attach events to another scope, and privacy failures never silently permit raw capture.
- verify: cargo test --locked && python3 tests/test_external_history_contract.py
- note: Completed producer-only credentials, exact project allowlists, scoped correlation keys, immutable privacy-checked evidence, bounded recursive secret rejection and unambiguous JSON parsing. Existing privileged routes refuse producer credentials. Partial archive configuration fails closed; external artifact/derived-memory retention policy is documented. No ingestion endpoints, storage, migrations or MCP capture added.
- result-verify: 2026-09-21: `cargo test --locked && python3 tests/test_external_history_contract.py` passed with `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1` (356 Rust tests, 19 contract tests). Recording contracts (25), SQL contracts (19), agentic SQL (11), migrations 001–020, formatting and `git diff --check` passed. Logs: `/tmp/p19-t02-verified.5NDPb1/`. Existing Python SQLite placeholder deprecation warnings remain.
- limitations: Authorization/privacy preparation only; full envelope/digest validation and durable acceptance remain P19-T03. Secret detection is conservative pattern matching, not complete DLP. External retention/deletion rules are policy for future consumers, not implemented external storage jobs. Not deployed or merged to main.

### P19-T03 · Add durable external history ingestion and receipts
- status: done
- priority: medium
- lane: history-storage
- parallel: no
- depends: P19-T02
- design: docs/design/external-development-history.md
- files: migrations/* (next additive migration), src/storage.rs, src/storage/, src/api/routes.rs, src/api/ (external-history handlers, new), tests/test_migrations.py, tests/external_history/
- done-when: Harness accepts external events through a dedicated bounded ingestion surface and acknowledges only committed records. Producer-scoped event identity deduplicates identical retries and rejects conflicting reuse. Events, receipt state and any extraction intent are transactionally consistent. Existing databases upgrade without rewriting applied migrations; external events neither submit chat turns nor execute tools or provider calls. Restart and lost-acknowledgement fixtures preserve accepted history without duplicate records.
- verify: python3 tests/test_migrations.py && cargo test --locked && python3 tests/external_history_integration.py

- result: Added migration 021, bounded producer-only ingestion, v1 envelope/digest validation, atomic immutable event/receipt rows, replay/conflict handling and restart/lost-ack tests. Verification: 360 Rust tests, 5 HTTP integration tests, 19 external contract tests, strict Clippy, migration chain including populated v20 upgrade, and recording/SQL/agentic regressions passed.
- limitations: No extraction dispatch, MCP capture, client integration, external history read UI, volume quota or retention/deletion jobs. Not merged or deployed.

### P19-T04 · Capture isolated development-mcp activity durably
- status: done
- branch: task/p19-t04-durable-capture (development-mcp, pushed to origin, based on origin/main 278b53d; not merged)
- commits: 73295b698ea768eb9147b3f999ec70062cf35cfc (journal, policy, session isolation, lifecycle hooks, recovery, tests), 179ad59 (legacy SSE isolation regression, conftest ordering)
- validation: 19 P19-T04 tests pass; full suite 197 passed / 3 failed; compileall and git diff --check clean. The 3 failures are tests/test_mcp_local_simulation.py::{test_mcp_run_command_stream_end_to_end, test_mcp_run_command_stream_timeout_end_to_end, test_mcp_delegate_task_structured_output_end_to_end}; they reproduce identically on clean 278b53d (no `python` on PATH, no `codex` binary) and pass with a `python` shim. Environment-only, not introduced by P19-T04.
- known non-blocking risks: executor background store.update transitions are not journaled as outcomes (crash window covered by task-store `interrupted` marking); `required` policy halts tool execution on journal failure by design (`NOTION_LOCAL_OPS_CAPTURE_MODE=off` is the escape hatch); journal has no retention/purge; http_compat.py, shell.py and executors.py listed above were not modified (session identity comes from FastMCP `Context.session_id`, verified for streamable-HTTP and legacy SSE).
- priority: medium
- lane: mcp-capture
- parallel: no
- depends: P19-T03
- design: docs/design/external-development-history.md
- files: /root/workspace/development-mcp/src/notion_local_ops_mcp/instrument.py, /root/workspace/development-mcp/src/notion_local_ops_mcp/session.py, /root/workspace/development-mcp/src/notion_local_ops_mcp/server.py, /root/workspace/development-mcp/src/notion_local_ops_mcp/http_compat.py, /root/workspace/development-mcp/src/notion_local_ops_mcp/tasks.py, /root/workspace/development-mcp/src/notion_local_ops_mcp/shell.py, /root/workspace/development-mcp/src/notion_local_ops_mcp/executors.py, /root/workspace/development-mcp/tests/
- done-when: accepted tool activity is durably admitted before execution and outcomes are committed before publication under the recording-required policy. File operations, commands, rejected requests, errors, cancellation and background-task outcomes retain correlation and explicit coverage. Per-client/session execution context replaces process-wide attribution assumptions; simultaneous clients cannot overwrite each other's recording scope or default cwd. Capture applies P19-T02 policy before local persistence; full-disk and recorder failure refuse new execution, while crash-window outcomes remain unknown/interrupted without action replay. Recording does not depend on an agent manually binding the optional live relay.
- verify: cd /root/workspace/development-mcp && python3 -m pytest

### P19-T05 · Deliver recorded activity reliably into Harness
- status: done
- priority: medium
- lane: mcp-delivery
- parallel: no
- depends: P19-T04
- design: docs/design/external-development-history.md
- files: /root/workspace/development-mcp/src/notion_local_ops_mcp/ (durable exporter and configuration), /root/workspace/development-mcp/tests/, src/api/ (external-history handlers), tests/external_history_integration.py (new)
- done-when: a bounded durable exporter delivers recorded events with stable identities, retry/backoff and acknowledgement-driven cleanup. Harness downtime does not lose locally committed events while capacity remains; restart, lost acknowledgements, duplicate delivery and out-of-order arrival converge to truthful history. Permanent rejection and backlog exhaustion are visible rather than silently dropped. Network calls remain outside execution-critical delivery waits; retries deliver records only, never rerun tools. Concurrent client sessions and background completions retain their original scope.
- verify: python3 tests/external_history_integration.py && (cd /root/workspace/development-mcp && python3 -m pytest)

### P19-T06 · Expose evidence-linked external session history
- status: done
- priority: medium
- lane: history-ux
- parallel: no
- depends: P19-T05
- design: docs/design/external-development-history.md
- files: src/api/routes.rs, src/api/stream.rs, src/storage/, static/api.js, static/app.js, tests/external_history_integration.py, tests/recording_ui.cjs
- done-when: authenticated users can discover external sessions by project and producer, inspect ordered activity and terminal outcomes, and open authorized supporting artifacts. Cursor pagination/resume handles concurrent arrivals without treating duplicate delivery as new work. The UI distinguishes local capture from Harness acknowledgement, pending background work, unknown outcomes, redaction, truncation and unavailable conversations. Optional client-supplied messages carry explicit provenance and linkage; no unsupported client transcript access or fabricated dialogue is implied.
- verify: python3 tests/external_history_integration.py && scripts/verify_browser.sh && scripts/verify_e2e.sh

### P19-T07 · Derive reviewed memory from external development evidence
- status: todo
- priority: medium
- lane: history-memory
- parallel: no
- depends: P19-T06
- design: docs/design/external-development-history.md
- files: src/ingest.rs, src/recording.rs, src/memory_agents.rs, src/storage/jobs.rs, src/storage/memories.rs, src/storage/provenance.rs, src/api/, tests/external_history/
- done-when: external history can yield deduplicated evidence-linked memory candidates through the existing review-first workflow, independently of ingestion success and under explicit extraction budgets. Recorded tool content remains untrusted evidence, not instructions. Only approved, active, in-scope memories enter recall; corrections, supersession and deletion preserve truthful provenance. A scoped retrieval surface makes approved context available to development clients without granting unrelated history access or claiming clients automatically use it. Extraction failure leaves durable history intact.
- verify: cargo test --locked && python3 tests/external_history_integration.py && scripts/verify_browser.sh

### P19-T08 · Qualify integration recovery and operational readiness
- status: todo
- priority: medium
- lane: history-reliability
- parallel: no
- depends: P19-T07
- design: docs/design/external-development-history.md
- files: tests/fault_injection.py, tests/external_history_integration.py, scripts/verify_release.sh, docs/design/external-development-history.md, docs/RECORDING_PROTOCOL.md, README.md
- done-when: disposable end-to-end fixtures demonstrate filesystem inspection, edits, commands, test results and background completion flowing through development-mcp into Harness without production changes. Evidence covers simultaneous clients, process crashes, Harness outages, lost acknowledgements, storage exhaustion, oversized output, retention and restore. Capture failures, export backlog age, rejected events and completeness gaps are observable without secret-bearing telemetry. Measured capacity and operating limits are documented; no automatic side-effect replay occurs and unsupported transcript coverage is explicit. The strict non-deploying release gate includes integration coverage and reports missing prerequisites as failures rather than passes.
- verify: python3 tests/fault_injection.py && python3 tests/external_history_integration.py && bash scripts/verify_release.sh && (cd /root/workspace/development-mcp && python3 -m pytest)

## P20 · Harness as the long-term brain

P20 separates reviewed knowledge export from full internal recovery and adds a
client-independent continuation protocol. It does not change P19's explicit transcript
coverage rule: development-mcp activity may be durable while conversation text remains
unavailable unless the originating client supplies it.

### P20-T01 · Full memory recovery snapshot
- status: done
- priority: high
- lane: memory-reliability
- parallel: no
- depends: —
- files: src/backup.rs, src/main.rs
- done-when: a local operator can create a transactionally consistent SQLite snapshot with a versioned manifest containing database schema, memory/revision/embedding/session counts and SHA-256 integrity, separately from reviewed export packets.
- verify: cargo test --locked backup

### P20-T02 · Restore and migration safety
- status: done
- priority: high
- lane: memory-reliability
- parallel: no
- depends: P20-T01
- files: src/backup.rs, migrations/022_agent_memory_protocol.sql, migrations/023_agent_session_links.sql, tests/test_migrations.py
- done-when: restore verifies manifest/schema/checksum/integrity before publishing a new destination, refuses overwrite, preserves WAL-committed state, and migrations preserve existing data and foreign-key integrity.
- verify: cargo test --locked backup && python3 tests/test_migrations.py

### P20-T03 · Memory health guard
- status: done
- priority: high
- lane: memory-reliability
- parallel: no
- depends: P20-T02
- files: src/storage.rs, src/main.rs, src/api/routes.rs
- done-when: a non-zero memory baseline is retained, empty fresh stores remain distinguishable from reset stores, `GET /memory/health` exposes the state, and startup refuses workers when a prior positive baseline unexpectedly falls to zero.
- verify: cargo test --locked memory_health

### P20-T04 · Agent-independent context resume
- status: done
- priority: high
- lane: continuation
- parallel: no
- depends: P20-T03
- files: src/storage.rs, src/api/routes.rs, docs/api.yaml
- done-when: a new client session can retrieve current task, plan, decisions, blockers, changed files, preferences, shared project memory and conflicts without relying on hidden agent state.
- verify: cargo test --locked continuation_is_shared

### P20-T05 · Multi-agent memory protocol
- status: done
- priority: high
- lane: continuation
- parallel: no
- depends: P20-T04
- files: migrations/023_agent_session_links.sql, src/storage.rs
- done-when: agent identity and agent-session identity link durably to one Harness session; GPT/Claude/Codex-style clients see the same shared project memory, while cross-session relinking conflicts rather than silently changing ownership.
- verify: cargo test --locked continuation_is_shared && python3 tests/test_migrations.py

### P20-T06 · Production reliability gate
- status: done
- priority: high
- lane: release
- parallel: no
- depends: P20-T01, P20-T02, P20-T03, P20-T04, P20-T05
- files: scripts/deploy.sh, src/backup.rs, src/storage.rs
- done-when: backup, restore, migration, health, recall and multi-agent continuation are verified against production-sized state; the clean commit is pushed and deployment reports the expected commit, binary hash, schema and healthy workers.
- verify: cargo test --locked -q && git diff --check

## P21 · UI control center and feature coverage

P21 is planned, not implemented. Define its feature-by-feature acceptance in
`docs/design/p21-ui-control-center.md`. Provider keys entered by users are
secrets, never backlog data or documentation examples. All tasks must retain
P20's schema/memory and P19's truthful transcript boundaries.

### P21-T01 · Inventory actual UI coverage and information architecture
- status: done
- priority: high
- lane: ui-architecture
- parallel: no
- depends: P20-T06
- design: docs/design/p21-ui-control-center.md
- files: docs/design/p21-ui-control-center.md, docs/design/p21-ui-coverage.json, docs/api.yaml, static/index.html, static/app.js, tests/test_ui_coverage.py, tests/ui_smoke.cjs
- done-when: every owner-facing API operation has a tested classification of usable UI, explicit expert/API-only with rationale, or unavailable; identify the necessary setup/projects/providers/work/memory/history/diagnostics/recovery navigation and define keyboard/mobile/error/empty-state expectations. Inventory is based on implemented routes rather than imagined capabilities.
- verify: python3 tests/test_ui_coverage.py && python3 scripts/check_docs.py && bash scripts/verify_browser.sh
- result: Classified all 77 router/OpenAPI method+path operations into 44 existing UI controls, 6 partial settings operations, and 27 expert/API-only or internal operations, with per-group rationale. Added an accessible P21 capability-status disclosure to Project & Models and a concrete navigation/empty/error/accessibility contract. UI status does not claim a provider editor, model picker or folder browser exists.
- result-verify: 4 new coverage-contract tests, docs/route/ledger checks, Ruff, JS syntax, and both Chromium mocked browser suites passed. Tests fail when a route is added without classification or when the present UI settings gaps are mislabeled complete; no production changes were made.

### P21-T02 · Safe runtime custom-provider configuration and secrets
- status: done
- priority: high
- lane: provider-backend
- parallel: no
- depends: P21-T01
- design: docs/design/p21-ui-control-center.md
- files: src/providers.rs, src/providers_tests.rs, src/memory_agents.rs, src/main.rs, src/api/, src/recording.rs, src/recording_sql.rs, src/storage.rs, migrations/024_provider_routing.sql, docs/api.yaml, docs/ARCHITECTURE.md, docs/design/p21-ui-coverage.json, tests/
- done-when: an owner can add/edit/test/choose/delete an OpenAI-compatible provider ID with validated baseUrl, api type and discovery policy; its API key stays exclusively in a private atomic 0600 secret store or separately-keyed encryption, never in ordinary SQLite, GET responses, logs or exports. Pin provider version to admitted turns, keep in-flight turns stable, allow explicit rotation without corrupting P20 state, and preserve the existing environment fallback. Deny SSRF, redirects, DNS rebinding and invalid URL/scheme/path combinations with mock-server tests.
- verify: cargo test --locked && bash scripts/verify_release.sh
- result: Added authenticated runtime provider profile CRUD/selection/test APIs with the startup environment provider retained as the fallback. Saved secrets live only in an atomic 0600 `.harness/providers.json` under a real 0700 directory; GET/public projections expose only key presence. Provider changes are serialized and append versioned configurations. Schema 024 stores only provider ID/version on each admitted receipt, preserving a stable provider for queued/in-flight turns and retries even after later edits or deletion. Custom-provider network use validates HTTPS (loopback HTTP only behind an explicit development opt-in), rejects URL credentials/query/fragment and forbidden/private/metadata/IPv4-transition targets, disables redirects, re-resolves and pins a validated socket address, and bounds provider response/time behavior through the existing adapter.
- result-verify: 374 Rust tests passed after the final secret-store hardening, strict Clippy with `-D warnings` passed, migrations 001→024 preserved data/FTS/FKs, recording contracts passed, provider tests cover private-store permissions/SQLite exclusion/version pinning/redirect refusal/internal-network policy/IPv6 transition cases/symlink refusal/shared-parent non-mutation, and the strict local release gate passed mocked Chromium plus real browser→Axum→SQLite→filesystem→provider E2E and failure-path E2E. The first CI run exposed a test-isolation bug where a test store directly under `/tmp` could chmod that shared parent; the fix now refuses non-private existing parents, uses dedicated 0700 test directories, preserves `/tmp` at 1777, and passes the exact failed HTTP test plus the full suite. Local release-quality explicitly leaves coverage, signing, and cross-target checks to release-evidence CI; no T02 production deployment is claimed here.

### P21-T03 · Provider model discovery, capability testing and fallback
- status: done
- priority: high
- lane: provider-backend
- parallel: no
- depends: P21-T02
- design: docs/design/p21-ui-control-center.md
- files: src/memory_agents.rs, src/api/, docs/api.yaml, tests/
- done-when: authenticated proxy discovery invokes only the chosen provider's bounded /models and returns sanitized IDs; connection checks report tested vs untested tools/streaming/usage without conflating a model list with successful generation. Timeout, 401, empty/malformed/unsupported /models and manual model IDs are visible and never fall back to another provider.
- verify: cargo test --locked && bash scripts/verify_e2e.sh
- result: Selected-provider model discovery now uses only that provider's bounded authenticated `/models` call and returns exact validated/deduplicated IDs with explicit available/empty/unavailable failure states. Discovery never falls back across providers, manual IDs remain explicitly allowed, and generation/tools/streaming/usage remain `untested` rather than being inferred from a successful model-list response.
- result-verify: Commit `46f4615` passed GitHub Actions run `36110954525` and is verified live: production `/health` reports the exact commit, schema 24 and both workers healthy; the running binary SHA matches `target/release/harness`; `/memory/health` is OK at 699 memories / 700 revisions / 699 embeddings; the pre-deploy recovery backup is verified. Provider GET output exposes no credential material.

### P21-T04 · Custom API editor in UI, including YAML/JSON paste
- status: done
- priority: high
- lane: provider-ui
- parallel: no
- depends: P21-T03
- design: docs/design/p21-ui-control-center.md
- files: static/index.html, static/app.js, static/api.js, static/style.css, tests/recording_ui.cjs, tests/browser_e2e.cjs
- done-when: owner UI can list, add, edit, test, choose and remove provider profiles with structured inputs or bounded paste of the example YAML/JSON vocabulary (baseUrl, apiKey, api: openai-completions, discovery.type: proxy); never display stored secrets or persist input in browser storage; distinguish validation, save, rotation and test outcomes; protect destructive actions with confirmation.
- verify: bash scripts/verify_browser.sh && bash scripts/verify_e2e.sh
- result: Added a Providers & secrets owner surface with selected/environment-provider labeling, add/edit/test/select/delete controls, password-only key entry, blank-key rotation semantics, destructive confirmation, and a deliberately bounded JSON/YAML parser that accepts only the supported provider vocabulary, fills the form for review, clears the paste buffer, and never auto-saves or auto-selects. Saved keys are never read back, rendered, placed in URLs, or written to browser storage; save/cancel/lock clear secret inputs.
- result-verify: Both mocked Chromium suites pass the provider editor, hostile-text, storage and lock/reset checks. The real browser→Axum test creates/edits/tests/selects/deletes a provider against a loopback mock in an isolated temporary working directory, proves the synthetic key is absent from GET responses/browser storage/SQLite, and preserves the existing tool/permission workflow. Failure-path E2E also passes denial, crash recovery, cancellation/safe retry and unsafe-retry refusal after isolating each test's private provider store.

### P21-T05 · Select main, extraction and verification models from /models
- status: todo
- priority: high
- lane: model-ui
- parallel: no
- depends: P21-T04
- design: docs/design/p21-ui-control-center.md
- files: static/index.html, static/app.js, static/api.js, src/api/, src/storage/config.rs, tests/
- done-when: model roles use accessible searchable provider-scoped model selectors fed by /models with exact IDs, loading/error/empty states, and a clearly labeled manual-ID option. Changing provider requires explicit role reassignment; old sessions and in-flight turns retain their pinned provider/model, and missing tool support keeps the existing safe fallback.
- verify: bash scripts/verify_browser.sh && bash scripts/verify_e2e.sh

### P21-T06 · Authorized server-side project directory browser
- status: todo
- priority: high
- lane: filesystem-api
- parallel: no
- depends: P21-T01
- design: docs/design/p21-ui-control-center.md
- files: src/api/, src/tools/paths.rs, docs/api.yaml, tests/
- done-when: an authenticated read-only paginated browse endpoint starts only from explicitly allowed server workspace roots; it lists safe directory names and breadcrumbs, not files, secrets, or the filesystem root. Reuse canonical scope path validation and refuse traversal, symlink escapes, racey swaps, hidden/denied directories and unauthorized roots, including in end-to-end tests.
- verify: cargo test --locked && bash scripts/verify_e2e.sh

### P21-T07 · Project folder picker plus typed-path option
- status: todo
- priority: high
- lane: project-ui
- parallel: no
- depends: P21-T06
- design: docs/design/p21-ui-control-center.md
- files: static/index.html, static/app.js, static/api.js, static/style.css, tests/recording_ui.cjs, tests/browser_e2e.cjs
- done-when: owner can select a server folder from allowed directories or type an absolute path, see resolved root/scope/permission mode before saving, recover from denial, and cancel without changing scope; a local browser file picker must never be represented as choosing a remote server directory.
- verify: bash scripts/verify_browser.sh && bash scripts/verify_e2e.sh

### P21-T08 · Truthful live work and model activity UI
- status: todo
- priority: medium
- lane: activity-ui
- parallel: no
- depends: P21-T01
- design: docs/design/p21-ui-control-center.md
- files: static/index.html, static/app.js, static/api.js, src/api/stream.rs, tests/recording_ui.cjs, tests/browser_e2e.cjs
- done-when: plan, model/provider label, status, elapsed time, bounded tool/permission activity, verification, usage and recovery are discoverable and accurately resumed after reload; show redacted provider-supplied reasoning **summary** only when explicitly available and validated, never raw hidden chain-of-thought or fabricated activity. Missing transcript remains explicitly unavailable.
- verify: bash scripts/verify_browser.sh && bash scripts/verify_e2e.sh

### P21-T09 · Complete owner-facing feature navigation and accessibility
- status: todo
- priority: medium
- lane: feature-ui
- parallel: no
- depends: P21-T05, P21-T07, P21-T08
- design: docs/design/p21-ui-control-center.md
- files: docs/design/p21-ui-control-center.md, static/index.html, static/app.js, static/style.css, tests/recording_ui.cjs, tests/ui_smoke.cjs
- done-when: coverage matrix reconciles every API operation to a discoverable UI control or documented expert/API-only rationale; navigation and labels avoid false claims, approvals/deletion require confirmations, and mobile/keyboard/screen-reader/dark-mode/XSS checks pass. Do not silently turn backend-only operations into unreviewed privileged actions.
- verify: python3 scripts/check_docs.py && bash scripts/verify_browser.sh

### P21-T10 · End-to-end provider/folder/UI release and production gate
- status: todo
- priority: high
- lane: release
- parallel: no
- depends: P21-T02, P21-T03, P21-T04, P21-T05, P21-T06, P21-T07, P21-T08, P21-T09
- design: docs/design/p21-ui-control-center.md
- files: scripts/verify_release.sh, tests/browser_e2e.cjs, tests/browser_e2e_failure.cjs, tests/, docs/PROGRESS.md
- done-when: disposable real-browser/provider/SQLite tests cover secret non-disclosure, provider switch mid-turn, key rotation/restart, /models failures, manual model fallback, folder sandbox escapes, lost ACK and resumed activity; strict gates pass and a verified recovery snapshot precedes a clean pushed and deployed commit with unchanged memory baseline, expected binary/schema and ready workers.
- verify: bash scripts/verify_release.sh && python3 scripts/check_docs.py
