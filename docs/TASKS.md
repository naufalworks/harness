# TASKS — ordered backlog with stable IDs

How to use: pick the first task with `status: todo` whose `depends:` are all `done`.
Change status in place. Never renumber or delete a task; mark it `dropped` with a reason.

Status values: `todo` | `doing` | `done` | `needs-verify` | `blocked` | `dropped`

Each task has: `status`, `depends`, `design` (doc section), `files` (touched),
`done-when` (observable outcome), `verify` (command).

---

## P0 · Gate (owner machine)

### P0-T01 · Compile and run the existing suites
- status: todo
- depends: —
- design: docs/ROADMAP.md#p0
- files: (none; run only)
- done-when: `cargo test --locked`, `cargo clippy --locked --all-targets`, `cargo build --locked`, both Python HTTP suites and `node --check static/app.js` pass on the owner's machine; failures are fixed with minimal diffs and journaled in PROGRESS.md.
- verify: bash scripts/verify_release.sh

### P0-T02 · Verify migration chain and schemas offline
- status: done
- depends: —
- design: docs/design/agentic-turn.md#schema
- files: tests/test_migrations.py, tests/test_tool_schemas.py
- done-when: migrations 001→003 apply on an empty DB and on a v2 DB; all `tools/schemas/*.json` parse and have `name`, `description`, `parameters` with `additionalProperties:false`.
- verify: python3 tests/test_migrations.py && python3 tests/test_tool_schemas.py

## P1 · Tool loop with receipts per step

### P1-T01 · Migration 003_agentic.sql
- status: needs-verify
- depends: —
- design: docs/design/agentic-turn.md#schema
- files: migrations/003_agentic.sql, src/storage.rs (accept user_version 3, apply 003)
- done-when: `DbStore::init` upgrades v2→v3 additively; new tables `scopes`, `turn_steps`, `activity_events`, `permission_requests`, `file_changes`, `plan_items` exist; `recover()` marks `running` steps and `pending` permissions as `interrupted`/`expired`.
- verify: python3 tests/test_migrations.py && cargo test --locked storage
- note: SQL validated offline (python sqlite3) and `storage.rs` version check/apply edited, but not compiled (no cargo in the AI sandbox). The `recover()` additions for steps/permissions are part of P1-T10.

### P1-T02 · Tool schemas and system prompts
- status: done
- depends: —
- design: docs/design/tools.md
- files: tools/schemas/*.json, prompts/main_agent.md
- done-when: eight schemas (read, grep, glob, edit, write, bash, think, todo_write) in OpenAI function format; system prompt states evidence-bound and verify-after-write rules in ≤ 60 lines.
- verify: python3 tests/test_tool_schemas.py

### P1-T03 · Provider adapter: tools in, tool_calls out (non-streaming)
- status: todo
- depends: P0-T01
- design: docs/design/agentic-turn.md#provider-adapter
- files: src/memory_agents.rs (rename later to provider.rs is optional), src/agent_loop.rs (types)
- done-when: `complete_with_tools(model, messages, tools) -> ModelTurn { text: Option<String>, tool_calls: Vec<ToolCall>, usage }`; `ToolCall { id, name, arguments_json }`; malformed JSON arguments produce a `tool_call` step with `status=failed, error_code=invalid_arguments` (fed back to the model as an error result), never a panic; the old text-only `chat_prepared` path is unchanged for `extraction`.
- verify: cargo test --locked provider

### P1-T04 · Scopes: root_path, permission_mode, diagnostics_cmd
- status: todo
- depends: P1-T01
- design: docs/design/agentic-turn.md#scopes
- files: src/main.rs (GET/POST /scopes/{scope}), src/storage.rs
- done-when: a scope row can be created/updated with a canonical absolute `root_path` that exists and is a directory, `permission_mode in (ask, auto_edit, auto_all)`, optional `diagnostics_cmd` (≤ 512 chars); tools refuse to run for scopes without `root_path`.
- verify: cargo test --locked scopes

### P1-T05 · Tool registry and sandboxed path resolution
- status: needs-verify
- depends: P1-T04
- design: docs/design/tools.md#registry, docs/design/tools.md#path-rules
- files: src/tools/mod.rs, src/tools/paths.rs
- done-when: `Tool` trait `{ name, schema, side_effecting, run(ctx, args) -> ToolResult }`; `resolve(root, user_path)` canonicalizes and rejects anything outside root, symlink escapes, and denied names (`.env*`, `*.pem`, `id_rsa*`); `ToolResult { content, truncated, bytes, artifacts }` with content passed through `safety::redact` and capped at 32 KB (head 24 KB + tail 8 KB).
- verify: cargo test --locked tools::paths
- note: written 2026-09-09 as src/tools/mod.rs + src/tools/paths.rs (+ src/tools/textdiff.rs, a dependency-free unified diff for T07). Not compiled (no cargo in the AI sandbox). SHA-256 via the existing `ring` crate, so no new deps yet. `is_dangerous_command` lives in mod.rs so the gate compiles before T08.

### P1-T06 · read / grep / glob
- status: needs-verify
- depends: P1-T05
- design: docs/design/tools.md#read, #grep, #glob
- files: src/tools/read.rs, src/tools/grep.rs, src/tools/glob.rs
- done-when: `read` returns numbered lines `N:hash│text` with per-line 4-hex hash (see design), supports `offset/limit`, caps 400 lines per call, reports `total_lines` and file `content_hash`; `grep` shells to `rg` when available else falls back to a Rust regex walk, max 100 matches, 1 context line; `glob` returns ≤ 500 paths sorted by mtime desc, respects `.gitignore` when `rg`/`ignore` crate available.
- verify: cargo test --locked tools::read tools::grep tools::glob
- note: written 2026-09-09 as ONE file src/tools/fs_tools.rs (Read, Grep, Glob) instead of three; grep shells to `rg --json` when present, else a literal (non-regex) walk that says so in its output; glob uses a hand-rolled `**`/`*`/`?` matcher and skips .git/target/node_modules. Not compiled.

### P1-T07 · edit / write with hash anchors and file_changes
- status: todo
- depends: P1-T06
- design: docs/design/tools.md#edit, #write, docs/design/agentic-turn.md#file_changes
- files: src/tools/edit.rs, src/tools/write.rs
- done-when: `edit` accepts `{path, anchors:[{line, hash}], old_string?, new_string}`; every anchor must match the current file or the tool fails with `stale_anchor` and the current lines; `old_string` (if given) must match exactly once; writes are atomic (temp + rename); a `file_changes` row with unified diff, before/after hashes is committed before the file is touched and flipped to `applied=1` after; `write` refuses existing files unless `overwrite:true`; both run `diagnostics_cmd` (if set, 60 s cap) and append its trimmed output to the result.
- verify: cargo test --locked tools::edit tools::write

### P1-T08 · bash
- status: todo
- depends: P1-T05
- design: docs/design/tools.md#bash
- files: src/tools/bash.rs
- done-when: runs `sh -c` with cwd = root_path, env stripped to a whitelist (PATH, HOME, LANG, TERM), timeout default 120 s (max 600), output capped per `ToolResult`; `background:true` starts detached with stdout/stderr to `<root>/.harness/logs/<step>.log` and returns the pid + log path; a deny-list of destructive patterns (`rm -rf /`, `git push --force`, `mkfs`, `dd if=`, `> /dev/sd`) always requires permission even in `auto_all`.
- verify: cargo test --locked tools::bash

### P1-T09 · think / todo_write and plan_items
- status: todo
- depends: P1-T01
- design: docs/design/tools.md#think, #todo_write
- files: src/tools/think.rs, src/tools/todo.rs, src/storage.rs
- done-when: `think` stores its text as the step output and returns "noted"; `todo_write` replaces the session's plan (≤ 30 items, each ≤ 200 chars, one `in_progress` max) in `plan_items` transactionally and returns the normalized list.
- verify: cargo test --locked tools::todo
- note: SQL for plan_items already exists in src/agentic_sql.rs (PLAN_CLEAR/PLAN_INSERT/PLAN_LIST) and is contract-tested by tests/test_agentic_sql.py; only the Rust tool + storage method remain.

### P1-T10 · Agent loop in the generation worker
- status: todo
- depends: P1-T03, P1-T05, P1-T09
- design: docs/design/agentic-turn.md#loop
- files: src/agent_loop.rs, src/recording.rs (call into loop), src/recording_sql.rs (new statements), src/storage.rs (recover)
- done-when: `generate()` runs the loop from the design doc: commit `model_call` step → provider → commit output → for each tool call commit `tool_call` step (and `permission_request` if required) → run → commit result → append to messages → repeat until final text or budget (`max_steps=40`, `max_tool_bytes=400_000`, `max_wall=15 min`); every transition writes an `activity_events` row; `context_json` is written once with the initial window and each `model_call` step stores its own full message array; restart marks `running` steps `interrupted`, the receipt `interrupted`, and never re-executes a tool.
- verify: cargo test --locked agent_loop && python3 tests/recording_integration.py
- note: all SQL for steps/events/recovery already exists in src/agentic_sql.rs and is contract-tested (tests/test_agentic_sql.py, 7 tests). Storage methods + the loop itself remain.

### P1-T11 · Permission gate
- status: todo
- depends: P1-T10
- design: docs/design/agentic-turn.md#permissions
- files: src/agent_loop.rs, src/main.rs (GET /permissions?scope=, POST /permissions/{id})
- done-when: in `ask` mode every side-effecting tool creates a `pending` permission row and the loop waits (poll 500 ms, expire after 30 min → tool result "denied: timed out"); `auto_edit` auto-approves edit/write only; `auto_all` auto-approves everything except the bash deny-list; approve/deny is idempotent and recorded as an activity event; denial is returned to the model as a tool error so it can adapt.
- verify: cargo test --locked permissions && python3 tests/recording_integration.py
- note: permission SQL exists in src/agentic_sql.rs; `Registry::requires_permission` (mode × side_effecting × bash deny-list) exists in src/tools/mod.rs.

### P1-T12 · API: steps, plan, activity
- status: todo
- depends: P1-T10
- design: docs/design/agentic-turn.md#api
- files: src/main.rs, src/storage.rs
- done-when: `GET /chat/requests/{id}/steps` (ordered steps with bounded output), `GET /sessions/{id}/plan`, `GET /activity?after_seq=N&session_id=` (≤ 200 events) all authenticated and covered by the mock-provider HTTP suite.
- verify: python3 tests/recording_integration.py

### P1-T13 · Minimal UI for steps and permissions (polling)
- status: todo
- depends: P1-T12
- design: docs/design/ui.md#p1-minimal
- files: static/app.js, static/index.html, static/style.css
- done-when: each user turn shows a collapsible step list (`● read src/x.rs 1-120`, `● edit +3 −1`, `● bash cargo test — exit 0`); a pending permission renders an Approve/Deny card at the bottom of the chat; the plan renders as a checklist above the composer; scope settings form (root path, mode, diagnostics command) in the Models tab; all text via `textContent`, never innerHTML.
- verify: node --check static/app.js && CHROMIUM_PATH=… node tests/recording_ui.cjs

### P1-T14 · Mock provider with tool calls + recovery tests
- status: todo
- depends: P1-T10, P1-T11
- design: docs/design/agentic-turn.md#testing
- files: tests/recording_integration.py, tests/mock_provider.py
- done-when: the synthetic loopback provider can be scripted to emit tool_calls; suite covers: happy path read→edit→bash→answer, stale anchor rejection, path escape rejection, permission deny, SIGKILL during a tool with `interrupted` state and no re-execution, budget exhaustion message.
- verify: python3 tests/recording_integration.py

## P2 · Streaming and activity rail

### P2-T01 · SSE endpoint over activity_events
- status: todo
- depends: P1-T12
- design: docs/design/agentic-turn.md#sse
- files: src/main.rs
- done-when: `GET /activity/stream?after_seq=N` (auth header via fetch, not EventSource) sends `id: <seq>` events, heartbeats every 15 s, resumes from `after_seq` with exactly-once delivery from the DB.
- verify: python3 tests/recording_integration.py

### P2-T02 · Three-pane layout and activity rail
- status: todo
- depends: P2-T01, P1-T13
- design: docs/design/ui.md#p2-layout
- files: static/*
- done-when: sessions/scopes left, chat center, activity right (plan, running step with elapsed time, context meter placeholder, permission prompt); mobile collapses panes; light/dark themes ported from `reference/renewed-ui-original`.
- verify: node --check static/app.js && node tests/ui_smoke.cjs

### P2-T03 · Diff cards with accept/reject and undo
- status: todo
- depends: P2-T02
- design: docs/design/ui.md#diff-cards
- files: static/*, src/main.rs (POST /changes/{id}/revert)
- done-when: each `file_changes` row renders as a diff card; revert restores `before` content if the file's current hash equals `after_hash`, else explains why not.
- verify: cargo test --locked changes && node tests/ui_smoke.cjs

## P3 · Context Manager, compaction, repo map

### P3-T01 · Context Manager with per-category budgets
- status: todo
- depends: P1-T10
- design: docs/design/context.md (to write when starting)
- done-when: window = system rules + tool defs + skills index + repo map + recalled memories + plan + compacted history + recent steps + user message, each with a byte budget; receipt lists included/excluded parts and sizes.
- verify: cargo test --locked context

### P3-T02 · Tool-result compaction and read cache
- status: todo
- depends: P3-T01
- done-when: tool results older than 3 model calls are replaced in the window by `[tool <name> step N, <bytes> bytes, hash <h>; call read again if needed]`; `read` of an unchanged file (same content_hash as an earlier step in the turn) returns a reference.
- verify: cargo test --locked context::compaction

### P3-T03 · Turn compaction at 70% budget with receipt
- status: todo
- depends: P3-T02
- done-when: a `compaction` step summarizes older steps (cheaper model), keeps plan + last 2 tool results verbatim, and the summary is stored as an episodic candidate source.
- verify: cargo test --locked context::turn_compaction

### P3-T04 · Repo map per scope
- status: todo
- depends: P1-T04
- done-when: `.harness/repo_map.txt` (≤ 8 KB) with files and top-level symbols (tree-sitter or ctags if present, else heading/regex fallback), refreshed by a job when files change.
- verify: cargo test --locked repo_map

## P4 · Memory kinds, hybrid recall, ambient UI

### P4-T01 · Migration 004_memory_kinds.sql
- status: todo
- depends: P1-T01
- done-when: `memories.category` and `candidates.category` accept `decision | episodic | procedural` via table rebuild inside one transaction; FTS rebuilt; existing rows preserved.
- verify: python3 tests/test_migrations.py && cargo test --locked storage

### P4-T02 · Local embeddings + hybrid recall
- status: todo
- depends: P4-T01
- done-when: embeddings table; ONNX small model (no network at runtime); recall = union(FTS5 top 20, cosine top 20) reranked by scope, recency, prior usefulness; same 6000-byte budget.
- verify: cargo test --locked recall

### P4-T03 · Correction detection and decision candidates
- status: todo
- depends: P4-T01
- done-when: extraction prompt also proposes `decision` (from plan + user confirmations) and marks user corrections ("no, use X") as high-priority candidates.
- verify: cargo test --locked extraction

### P4-T04 · Inline suggestion tray
- status: todo
- depends: P2-T02
- done-when: candidates from the current turn appear under it with Save / Edit / Dismiss; Memory tab becomes "Inbox" for imports only.
- verify: node tests/ui_smoke.cjs

## P5 · Verifier, skills, sub-agents

### P5-T01 · Verifier step
- status: todo
- depends: P3-T01
- done-when: after the final answer, a `verification` step (cheaper model) lists file/symbol claims and checks each against tool evidence in the turn; unverified claims produce a badge and an activity event.

### P5-T02 · Skills with progressive disclosure
- status: todo
- depends: P3-T01
- done-when: `skills/*/SKILL.md` discovered at startup; name+description in the window; `skill` tool loads a body (≤ 16 KB) as a tool result.

### P5-T03 · task tool (read-only explore sub-agent)
- status: todo
- depends: P3-T01
- done-when: sub-agent has its own step list under the same request (parent_step_id), read/grep/glob only, returns ≤ 1 KB summary + file refs; budgets inherited and shared.

## P6 · Structural tooling

### P6-T01 · ast_edit via ast-grep
- status: todo
### P6-T02 · lsp tool (diagnostics, references, rename)
- status: todo
### P6-T03 · browser tool via CDP
- status: todo

---

## Ideas parking lot (not scheduled)
- Memory branches per project; memory rehearsal (retrieval diff before approval) — from ROADMAP P6.
- Portable continuation packet export.
- Cost dashboard per scope/day.
- Voice input.
