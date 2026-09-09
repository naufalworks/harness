# Harness — changed & new files (2026-09-09, session 2)

Two bundles are attached. Both are ordinary **zip archives renamed** so they can be downloaded; rename them back (`.md`/`.txt` → `.zip`) and unzip.

| bundle | contents |
|---|---|
| `harness-full.zip.md` / `.txt` | the whole repo with all changes applied (unzip → `harness/`) |
| `harness-changes-only.zip.md` / `.txt` | only the 30 files below, same relative paths — unzip over your existing checkout |

Start reading at `harness/AGENTS.md` → `docs/PLAN.md` → `docs/TASKS.md` → `docs/PROGRESS.md`.

## Files

| status | path | purpose |
|---|---|---|
| new | `AGENTS.md` | entry point for any AI/human continuing the work; rules, file map, resume steps |
| modified | `README.md` | “Remaining work” now points to PLAN/TASKS/PROGRESS |
| new | `docs/PLAN.md` | goal, principles, phases P0–P6, non-goals |
| new | `docs/PROGRESS.md` | journal: what changed, verified, decisions, open questions, resume prompt |
| modified | `docs/ROADMAP.md` | header marks it superseded by PLAN/TASKS |
| new | `docs/TASKS.md` | ordered backlog with stable IDs, status, depends, done-when, verify |
| new | `docs/design/agentic-turn.md` | step loop, budgets, recovery, storage, API |
| new | `docs/design/tools.md` | tool contracts, path rules, permission modes, error codes |
| new | `docs/design/ui.md` | turn card, steps, permission card, plan strip, activity log |
| new | `migrations/003_agentic.sql` | scopes, turn_steps, activity_events, permission_requests, file_changes, plan_items |
| new | `prompts/main_agent.md` | main-agent system prompt with placeholders |
| new | `scripts/gen_tool_schemas.py` | generates tools/schemas/*.json |
| new | `src/agentic_sql.rs` | 23 SQL constants for the P1 loop (contract-tested) |
| modified | `src/main.rs` | adds `mod agentic_sql; mod tools;` |
| modified | `src/storage.rs` | accepts user_version 1..=3, applies 003 |
| new | `src/tools/fs_tools.rs` | read / grep / glob tools (+1 unit test) |
| new | `src/tools/mod.rs` | Tool trait, Registry, ToolCtx/ToolResult (redact + 32 KB cap), Artifact, PermissionMode, hashes |
| new | `src/tools/paths.rs` | sandboxed path resolution + secret deny-list (+4 unit tests) |
| new | `src/tools/textdiff.rs` | dependency-free unified diff (+3 unit tests) |
| new | `tests/test_agentic_sql.py` | 7 offline contract tests for agentic_sql.rs against the real schema |
| new | `tests/test_migrations.py` | 001→002→003 chain, constraints, recovery updates |
| new | `tests/test_tool_schemas.py` | schema files valid & match generator |
| new | `tools/schemas/bash.json` | model-facing tool schema (generated) |
| new | `tools/schemas/edit.json` | model-facing tool schema (generated) |
| new | `tools/schemas/glob.json` | model-facing tool schema (generated) |
| new | `tools/schemas/grep.json` | model-facing tool schema (generated) |
| new | `tools/schemas/read.json` | model-facing tool schema (generated) |
| new | `tools/schemas/think.json` | model-facing tool schema (generated) |
| new | `tools/schemas/todo_write.json` | model-facing tool schema (generated) |
| new | `tools/schemas/write.json` | model-facing tool schema (generated) |

## Verified in the AI sandbox

- `python3 -m unittest discover -s tests -p "test_*.py"` → 50 tests OK
- `node --check static/app.js` → OK

## NOT verified (no cargo here)

All Rust: `src/agentic_sql.rs`, `src/tools/*`, the two-line `src/storage.rs` change. On your machine run `bash scripts/verify_release.sh` then `cargo test --locked` and journal fixes in `docs/PROGRESS.md` (P0-T01).

## Status snapshot

done: P1-T01 (needs-verify), P1-T02 · needs-verify: P1-T05, P1-T06 · SQL groundwork done for T09/T10/T11 · next: P1-T07 edit/write, T08 bash, T09 think/todo, T03 provider adapter, T10 loop, T11–T14.
