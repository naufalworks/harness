# AGENTS.md — how to continue this project (humans and AI agents)

This file is the entry point. Read it fully before touching anything.

## Goal in one paragraph

Turn Harness from a durable *chat + memory recorder* into a durable *coding agent*:
a thin main agent that reads, searches, edits and runs code through recorded,
permission-gated tools, surrounded by background agents that manage its context,
recall memories, verify its claims and learn the user's preferences over time. Every
step is recorded before it is shown, and the UI makes the agent's work visible
(plan, running tool, diffs, context budget) so the chat never feels like a black box.

## Read in this order, every session

1. `docs/PLAN.md` — goal, principles, target architecture, phases and acceptance criteria.
2. `docs/TASKS.md` — ordered task list with stable IDs. Pick the first task whose
   `status` is `todo` and whose `depends` are all `done`.
3. `docs/PROGRESS.md` — journal. The last entry tells you where the previous session
   stopped, why, and any open questions.
4. The design doc named in the task's `design:` line (`docs/design/*.md`).
5. Only then read source files listed in the task's `files:` line.

## Operating rules

- **One task at a time.** Set its status to `doing` in `docs/TASKS.md`, finish it,
  run its `verify:` command, set `done`, append a `docs/PROGRESS.md` entry. If you
  must stop mid-task, still write the PROGRESS entry (what is half-done, what is next).
- **Never mark `done` without running `verify:`.** If your environment cannot run it
  (for example no `cargo`), set `needs-verify` and say exactly which command the
  user must run.
- **Additive only.** Do not rewrite behavior documented in `docs/RECORDING_PROTOCOL.md`.
  New tables, new event kinds in new tables, new endpoints. Migrations are append-only
  (`003_...`, `004_...`); never edit an applied migration.
- **Recording-first.** A step (model call, tool call, permission request) is committed
  to SQLite *before* it runs and its result is committed *before* it is displayed or
  used by the next step. Restart must never re-run a side-effecting tool automatically.
- **Sanitize everything model- or user-facing** with `safety::redact`, including tool
  output and diffs.
- **Sandbox.** Tools may only touch paths under the scope's `root_path` (canonicalized;
  symlinks resolved; no `..` escape). No outbound network from tools in P1.
- **Prefer the style of the file you are editing.** Small focused functions, `anyhow`
  errors, no unwrap on external data.
- **Do not guess.** If a task is ambiguous, write the question under
  "Open questions" in `docs/PROGRESS.md` and choose the smallest reversible option.
- Keep `README.md` truthful: if a feature is not compiled and tested, do not describe it
  as working.

## Verify commands

| What | Command | Needs |
|---|---|---|
| Full release gate | `bash scripts/verify_release.sh` | cargo |
| Rust unit tests only | `cargo test --locked` | cargo |
| Migrations apply cleanly 001→latest | `python3 tests/test_migrations.py` | python3 (sqlite3 with FTS5) |
| SQL contract strings | `python3 -m unittest tests/test_sql_contracts.py` | python3 |
| Tool JSON schemas are valid | `python3 tests/test_tool_schemas.py` | python3 |
| Frontend syntax | `node --check static/app.js` | node |

## Repo map

| Path | Role |
|---|---|
| `src/main.rs` | axum router, auth middleware, HTTP handlers |
| `src/recording.rs` | durable admission, serial generation worker, outbox |
| `src/recording_sql.rs` | exact SQL strings (contract-tested) |
| `src/memory_agents.rs` | provider client (OpenAI-style), chat message builder, extraction |
| `src/storage.rs` | `DbStore`, memory candidates/approval/recall, jobs |
| `src/safety.rs` | redaction, fingerprints, validators |
| `src/ingest.rs` | transcript import parsers |
| `src/agentic_sql.rs` (P1) | exact SQL for steps/events/permissions/plan/recovery — contract-tested by `tests/test_agentic_sql.py` |
| `src/tools/` (P1) | `mod.rs` registry+trait, `paths.rs` sandbox, `textdiff.rs` diff, `fs_tools.rs` read/grep/glob (written, needs-verify); edit/bash/meta pending — see `docs/design/tools.md` |
| `src/agent_loop.rs` (P1) | multi-step turn loop — see `docs/design/agentic-turn.md` |
| `migrations/` | versioned schema; `003_agentic.sql` adds steps/permissions/changes |
| `tools/schemas/*.json` | model-facing tool definitions (OpenAI function format) |
| `prompts/*.md` | system prompts for main agent and helpers |
| `static/` | single-page UI (`index.html`, `app.js`, `style.css`) |
| `tests/` | Python contract/integration suites, node UI suites |
| `docs/` | PLAN, TASKS, PROGRESS, design docs, protocol |

## Glossary

- **scope** — a project label (`global` or e.g. `myrepo`). Partitions memory and, from P1,
  owns a `root_path` and `permission_mode`.
- **receipt** — `chat_receipts` row: durable record of one user turn and its state.
- **step** — `turn_steps` row: one model call, tool call, sub-agent run, compaction or
  verification inside a turn.
- **activity event** — `activity_events` row: append-only log powering the UI timeline
  and (P2) SSE stream.
- **candidate / memory** — proposed vs approved durable fact. Nothing is recalled until
  approved.
- **outbox** — durable intent to extract memories from a finished turn.
- **permission request** — a pending approval for a side-effecting tool call.
- **hash anchor** — `line:hash` reference returned by `read`, required by `edit` so
  stale edits fail loudly instead of corrupting files.
