# AGENTS.md — how to continue this project

This is the contributor entry point. Read it fully before changing the repository.

## Current product

Harness is a single-user, loopback-first Rust coding agent with durable SQLite receipts. The agent can inspect and modify a configured project through recorded tools, request permission for side effects, stream redacted output from durable events, recall reviewed memories, and expose causal evidence for its work. Generation and extraction are server-owned background workers; browser tabs do not own accepted work.

P0–P10 are historical, completed phases. The active continuation is P11–P18 in `docs/ROADMAP.md`; exact status and eligibility come only from `docs/TASKS.md`. A historical journal entry is evidence about that run, not current release evidence.

## Read in this order

1. `docs/PLAN.md` — current baseline, principles, historical phases, and continuation scope.
2. `docs/ROADMAP.md` — active waves, release boundary, priority policy, and coverage contract.
3. `docs/TASKS.md` — executable backlog. Pick the highest-priority eligible `todo`; a `release-blocker` wins within its priority tier, then use the earlier task ID.
4. `docs/PROGRESS.md` — newest-first journal; use the newest entries for handoff, not as a substitute for rerunning verification.
5. The design document named by the selected task.
6. Only then inspect the task's declared files and adjacent contracts needed to change them safely.

## Operating rules

- **One task per branch/worktree.** Set it to `doing`, journal the transition, run its exact `verify:` command, then set it to `done` and add a completion entry. Parallel work requires `parallel: yes`, completed dependencies, separate lanes/files, and separate worktrees. Merge one task at a time and rerun affected verification plus the strict release gate after integration.
- **Release blockers first.** Recovery, cancellation, tool boundaries, truthful verification, and fail-closed spend limits outrank performance, refactoring, research, and optional features until the safe daily-use boundary is met.
- **Never turn missing evidence into a pass.** `scripts/verify_local.sh` is permissive and labels skipped lanes. `scripts/verify_release.sh` is strict, non-deploying, and requires the declared browser runtimes and real browser-to-server E2E.
- **Deployment is separate.** Verification must not restart production. `scripts/deploy.sh` rejects dirty source by default, builds a release candidate, verifies live identity/readiness, and performs schema-aware binary rollback. It never restores a database automatically.
- **Progress is part of the task.** Journal every `doing`, `needs-verify`, `blocked`, `done`, or `dropped` transition with task ID, result, exact verification, blockers, and next eligible work.
- **Migrations are append-only.** Never edit an applied migration. Schema-writing tasks are serialized.
- **Recording-first.** Commit model calls, tool calls, permissions, and results before they are acted on or shown. Restart must never automatically replay a side-effecting tool or potentially billed provider call.
- **Sanitize model- and user-facing material** with `safety::redact`, including tool output and diffs. Exact-original archival is opt-in, encrypted, and separate from sanitized recording.
- **Respect current tool boundaries.** Filesystem tools resolve beneath the configured canonical project root. Side effects follow the scope permission mode. Browser and command policy hardening still has explicit remaining tasks; do not claim stronger isolation than tests prove.
- **Prefer focused, reversible changes.** Match local style, return `anyhow` errors for fallible Rust paths, and avoid panics on external data.
- **Do not guess.** Record unresolved product or recovery decisions in the newest progress entry and choose the smallest reversible implementation.

## Verification surfaces

| Purpose | Command | Contract |
|---|---|---|
| Developer gate | `bash scripts/verify_local.sh` | Permissive; reports RUN/PASS/FAIL/BLOCKED/SKIPPED truthfully |
| Release gate | `bash scripts/verify_release.sh` | Strict and non-deploying; browser and real E2E lanes mandatory |
| Real browser-to-server E2E | `scripts/verify_e2e.sh` | Browser → Axum → SQLite/filesystem → synthetic provider → browser |
| Rust tests | `cargo test --locked` | Native unit/integration behavior |
| Migration chain | `python3 tests/test_migrations.py` | Applies 001 through the latest migration with SQLite FTS5 |
| Fault injection | `python3 tests/fault_injection.py` | Disposable crash, WAL, disk, queue, and replay boundaries |
| Browser UI fixtures | `scripts/verify_browser.sh` | Both mocked-browser suites |
| Frontend syntax | `node --check static/app.js` | JavaScript parser check |

Do not copy historical test counts or deployment IDs into current claims. Report evidence from the command actually run.

## Repository map

| Path | Current role |
|---|---|
| `src/main.rs` | Process entry: `Harness` state, runtime identity, startup/shutdown |
| `src/api/` | `routes.rs` routing table and handlers, `auth.rs` authentication/session/hardening middleware, `stream.rs` SSE, `assets.rs`, `error.rs` |
| `src/agent_loop.rs` | Recorded bounded model↔tool loop: orchestration, budgets, permissions, tool calls, delegation |
| `src/agent_loop/` | `steps.rs` durable step/permission transitions, `compaction.rs` provider-window compaction, `verification.rs` verification evidence |
| `src/recording.rs`, `src/recording_sql.rs` | Durable admission, generation worker, event feed, extraction outbox |
| `src/memory_agents.rs` | Provider adapter plus extraction/compaction/verification calls |
| `src/storage.rs` | `DbStore` and schema initialization; row persistence only |
| `src/storage/` | `scope.rs` scope limits/plan validation/root canonicalisation, plus `config.rs`, `turns.rs`, `memories.rs`, `jobs.rs`, `provenance.rs`, `provider.rs` |
| `src/embeddings.rs` | Deterministic local feature-hashing vectors used with FTS5 recall |
| `src/tools/` | Read/grep/glob, edit/write, bash, think/todo, skill/task, AST, LSP, and CDP browser tools |
| `src/tools/lsp_tool/` | `protocol.rs` caps/argument preparation, `session.rs` framing and teardown, `format.rs` diagnostics/references, `rename.rs` workspace edits |
| `src/tools/browser_tool/` | `protocol.rs` caps/destination validation, `snapshot.rs` accessibility snapshots, `cdp.rs` socket and browser launch, `session.rs` page lifecycle |
| `src/archive/` | Opt-in encrypted exact-original archive and privacy actions |
| `migrations/` | Applied schema chain, currently 001–007 |
| `tools/schemas/` | Model-facing tool definitions |
| `static/` | Served single-page UI |
| `tests/` | Rust-adjacent Python contracts, HTTP/fault fixtures, Node/browser E2E |
| `scripts/` | Verification, encrypted backup/restore, deployment/rollback, import/migration helpers |

## Durable terms

- **receipt** — one accepted user turn and its durable state.
- **step** — recorded model call, tool call, sub-agent, compaction, or verification.
- **activity/generation event** — append-only UI/replay event addressed by a database cursor.
- **candidate / memory** — proposed versus reviewed active durable knowledge.
- **outbox** — durable intent to extract memories after a finished turn.
- **permission request** — durable approval decision for a side effect.
- **hash anchor** — line/hash precondition used to reject stale text edits.
