# PLAN — Harness as a durable coding agent

Status: living document. P0–P10 are the historical completed baseline; P11–P19 remain the tracked continuation and P20 long-term memory reliability is complete. `docs/ROADMAP.md` defines sequencing and the safe daily-use boundary, `docs/TASKS.md` is the authoritative executable backlog, and `docs/PROGRESS.md` is a newest-first journal. Journal evidence describes the run that produced it and is never a substitute for fresh verification.

## 1. Why

The owner wants an agent that:

1. **Codes without hallucinating** — every claim about the codebase is backed by a tool
   result from the current turn; edits are verified (diagnostics/tests) before they are
   reported as done.
2. **Remembers** preferences, decisions and past topics across sessions and projects,
   with the owner in control of what is remembered.
3. **Grows with the owner** — corrections become memories, repeated workflows become
   skills, decisions have a history.
4. **Feels good to use** — the UI shows the plan, what tool is running, what changed,
   what context the model had, and what it costs. Memory is ambient, not the product.

## 2. Current baseline

- Recording-first admission and server-owned generation/extraction workers; a lost tab does not cancel accepted work.
- Durable model/tool/permission/verification steps, resumable activity and generation events, write-once context receipts, and bounded causal incident graphs.
- Recorded tools for project read/search, anchored edits/writes, bash, planning, skills, read-only task sub-agents, Rust AST rewrites, LSP operations, and CDP browser work.
- Bounded context construction, tool-result/turn compaction, repository maps, and final-answer evidence verification.
- Review-first memory with FTS5 plus deterministic local feature-hashing vectors; no embedding API or model download.
- Loopback enforcement, current/previous token rotation, short-lived memory-only browser sessions, delayed auth failures, Origin/body/rate/session controls, optional trusted-proxy identity, and conditional HSTS.
- One-process-per-database ownership, graceful drain, no-replay crash recovery, readiness/build/schema identity, encrypted backup drills, schema-aware binary rollback, and disposable fault injection.
- Opt-in encrypted exact-original archival is distinct from sanitized recording and uses external current/previous keys.
- Full internal recovery snapshots are separate from reviewed exports, carry checksum/schema/count manifests, and are verified before restore.
- Memory health retains a non-zero baseline and refuses worker startup after an unexpected reset; agent-independent continuation links GPT/Claude/Codex-style sessions to shared Harness project memory without rewriting reviewed memory.

This list describes compiled capabilities, not a fresh pass or deployment. Current evidence comes from commands run for the present change.

## 3. Principles for everything new

| # | Principle | Consequence |
|---|---|---|
| 1 | Recording-first | Steps, tool calls and permissions are rows before they are actions. |
| 2 | Evidence-bound | An edit may reference only paths/anchors returned by tools in this turn. The loop enforces it, not just the prompt. |
| 3 | Verify-after-write | `edit`/`write` automatically run the scope's diagnostics command; the model sees the result. |
| 4 | Small main context | A Context Manager builds the window under a budget from curated parts; raw history is not the default. |
| 5 | Human gate on side effects | Permission modes: `ask` (default), `auto_edit`, `auto_all` per scope. Restart never auto-repeats a side effect. |
| 6 | Memory stays reviewed | Recall of approved memories only; new memory kinds still go through candidates. |
| 7 | Visible work | Every step produces an activity event the UI can render live. |
| 8 | Additive, testable | New tables/endpoints; each task has a verify command. |

## 4. Target architecture

```
Browser UI ──authenticated SSE/poll──► durable activity/generation events
     │ submit / approve
     ▼
Axum API ──► DbStore (SQLite WAL, one process) ◄── server-owned workers
                                                ├─ agent loop: model ↔ recorded tools
                                                ├─ context: rules, skills, repo map, memory, plan, history
                                                ├─ recall: FTS5 + deterministic local feature hashing
                                                ├─ verifier: final claims ↔ bounded evidence manifest
                                                ├─ extraction: reviewed memory candidates
                                                └─ compaction: bounded summaries and repo-map refresh
Tools: read, grep, glob, edit, write, bash, think, todo_write, skill, task,
       ast_edit, lsp, browser
```

### Current turn lifecycle

`captured → generating → [step*] → complete | failed | interrupted`

Inside `generating`, steps run serially: `model_call` → zero or more `tool_call`
(each possibly preceded by a `permission_request`) → `model_call` … → final answer.
Budgets: max steps per turn, max tool-output bytes, wall-clock limit. Exceeding a budget
ends the turn with a visible "budget exhausted" answer, never a silent stop.

## 5. Phases

P0–P10 below are historical completed scope records. Their acceptance prose explains what each phase set out to prove; it is not pending work and does not claim a fresh run. Current status is always read from `docs/TASKS.md` and current evidence must be rerun.

### P0 — Gate: make the current checkpoint real [historical — complete]
Historical gate, completed: the baseline compiled and its required suites ran before later phases proceeded. Current release evidence comes from the strict non-deploying release gate, not this historical statement.

### P1 — Tool loop with receipts per step [historical — complete]
Provider adapter accepts tool definitions and tool calls; `turn_steps`,
`activity_events`, `permission_requests`, `file_changes`, `scopes`, `plan_items`
tables; tools read/grep/glob/edit/write/bash/think/todo_write; sandboxed path
resolution; permission gate; minimal UI rendering of steps and permission prompts
(polling). Acceptance: with the mock provider, a turn that reads a file, edits it via a
hash anchor, runs `bash`, and answers, is fully recorded; killing the process mid-tool
leaves the step `interrupted` and nothing is re-run on restart; an edit with a stale
anchor is rejected; a path outside `root_path` is rejected.

### P2 — Streaming and activity rail [historical — complete]
Authenticated SSE over `activity_events` with a resumable cursor; three-pane layout;
live plan/todo, running-tool indicator, diff cards with accept/reject, token/cost chips,
context-budget meter. Acceptance: reconnecting mid-turn replays missed events exactly
once; UI shows every step within 500 ms of its commit; no XSS with adversarial tool output.

### P3 — Context Manager, compaction, repo map [historical — complete]
Deterministic window builder with per-category budgets; tool-result compaction
(old results replaced by hash references); context compaction at ~70% with a receipt;
tree-sitter/ctags-style repo map per scope, refreshed by the compactor. Acceptance:
context receipt lists included and excluded parts with byte counts; a 40-step turn stays
under the token budget; re-reading an unchanged file returns a reference, not the body.

### P4 — Memory kinds, hybrid recall, ambient memory UI [historical — complete]
Migration `004`: memory kinds `preference | fact | project | rule | skill | decision |
episodic | procedural`; deterministic local feature-hashing vectors (no API call or model download) + FTS5 hybrid; rerank by
scope, recency, prior usefulness; corrections detected as high-signal candidates; inline
suggestion tray (Save / Edit / Dismiss) replaces the Memory tab as the primary surface.
Acceptance: a decision made in session A is recalled in session B by meaning, not
keyword; rejecting a suggestion never deletes chat history (existing invariant holds).

### P5 — Verifier/advisor, skills, sub-agents [historical — complete]
A recorded verifier call flags unsupported claims and skipped diagnostics;
`skills/<name>/SKILL.md` progressive disclosure; `task` tool spawns a read-only explore
sub-agent with its own context returning a bounded summary. Acceptance: verifier badge
appears on turns whose answers cite files never read; a skill body is only in context
after being loaded.

### P6 — Structural tooling [historical — complete]
`ast_edit` (ast-grep), `lsp` (diagnostics, references, rename), browser tool via CDP.

### P7 — Durable generation continuation [historical — complete]
Generation output uses a persisted, authenticated, resumable event feed rather than
browser-owned work. The atomic provider boundary and idempotent restart recovery are
landed on `main`. Landed since then: per-event request attribution with proven
multi-turn cursor/SSE replay (`P7-T02c`), truthful migration-gate reporting (`P7-T04`),
and a chat UI that renders the durable feed with explicit complete, failed and
interrupted states (`P7-T03`). The phase's umbrella tasks (`P7-T01`, `P7-T02`) are
closed on that evidence. Subsequent work completed both remaining items. Publication before DONE landed as
`P7-T05`, now designed in `docs/design/incremental-publication.md`: the publication unit
is a completed line, because `safety::redact` erases a matched line whole and a released
prefix could never be retracted. `tests/test_incremental_publication.py` covers the contract: completed lines publish incrementally while an unfinished tail remains withheld. The browser runtime and both mocked-browser suites landed as `P7-T06`; the strict release gate now additionally requires the real browser-to-service E2E lane.

### P8 — Causal observability [historical — complete]
P8 added bounded, typed provenance edges and a closed incident read model across memory/evidence, model calls, tools, permissions, observations, mutations, and recovery. It excludes hidden chain-of-thought and reports missing provenance as unknown.

Historical acceptance evidence covered denial, stale-anchor failure, and crash recovery navigation to the earliest known causal break, affected durable rows, and recovery action.

### P9 — Review hardening [historical — complete]

Closed bounded incident-graph edge/adjacency integrity and Python SQLite lifecycle
warnings. P1–P9 form the verified baseline for continuation work.

### P10 — Correctness and operational safety [historical — complete]

Closed bounded incident projection references; enforced one process per database; added readiness/build/schema identity, encrypted backup/restore drills, graceful shutdown, schema-aware executable rollback, and disposable crash/disk/WAL/queue/background-process fault injection.

### P11–P13 — Performance, maintainability, and remaining security [active continuation]

Replace polling and reduce storage pressure; decompose large modules; add typed/API and CI contracts; prevent documentation/deployment drift; and complete browser, command, final-write path, retention, and audit boundary work. P13 authentication hardening and encrypted archive/backup keys are already complete; remaining task status lives in `docs/TASKS.md`.

### P14–P16 — Daily workflow, memory, and observability [active continuation]

Add cancellation/retry, session and permission workflows, provider/tool improvements,
modular accessible UI, inspectable retrieval and memory governance, and measurable
causal incident comparison/search/export without hidden-reasoning capture.

### P17–P18 — Memory Wind Tunnel and optional platform evolution [active continuation]

Build immutable run capsules, strict/live/hybrid memory treatments and evidence-bound
comparison reports. Multi-worker, plugin and remote execution remain optional and may
start only after the earlier safety contracts are complete.

### P19 — External development history [active continuation]

Durably ingest sanitized development-mcp activity with producer/project isolation and
expose evidence-linked external session history. Remaining extraction/readiness work is
tracked in `docs/TASKS.md`; unsupported client transcript access must remain explicit
rather than fabricated.

### P20 — Long-term memory reliability [complete]

Separate reviewed export from full recovery backup; add checksum/schema/count-verified
restore, memory-reset health protection, agent-independent continuation envelopes,
stable agent-session linking, and production recovery verification. The full transcript
of a client that does not supply conversation text is still unavailable; tool history
does not imply transcript capture.

### P21 — UI control center and configuration coverage [complete]

Make setup and daily operation usable without editing a production environment file:
owner-managed OpenAI-compatible provider connections, safe UI-only secret replacement,
per-provider `/models` discovery with selectable main/extraction/verification models,
an authenticated server-side project folder browser alongside typed paths, and a
durable activity view that shows observable work rather than hidden model reasoning.
Inventory every existing owner-facing backend feature against usable UI coverage;
explicitly label expert/API-only and unavailable functions instead of claiming every
feature already has a screen. The exact security, non-goals, acceptance criteria and
implementation order live in `docs/design/p21-ui-control-center.md` and
`docs/TASKS.md`. P21 is implemented and production-verified; expert/API-only
operations remain explicitly labelled instead of being represented as browser controls.

## P22 · Full UI/UX redesign and session experience

Redesign the owner-facing product around the approved modern workspace, re-authentication and
Control Center direction while preserving all P21 security/recovery guarantees. The work includes
a coherent design system, new responsive application shell, Chat / Work redesign, richer recorded
runtime activity, a Claude-Code-style **Decision Trace** built only from explicit/recorded metadata,
session-expiry/re-auth restoration without persisting the master token or resending ambiguous work,
and redesigned Projects/Folders, Providers/Secrets, Model Roles, Memory, History/Privacy and
Imports/Jobs surfaces. P22 remains on a separate branch until explicit owner acceptance so the P21
production baseline can be retained or restored cleanly. Detailed requirements and the executable
T01-T16 ledger live in `docs/design/p22-ui-redesign.md` and `docs/TASKS.md`.

## 6. Non-goals (for now)
Multi-user, remote bind, enabling exact-original archive by default, OpenAI-
compatible proxy API, autonomous background coding without a human in the loop.

## 7. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Code or documentation claimed without current evidence | Exact task verification plus strict integrated release gate; use `needs-verify` or `blocked` rather than converting missing coverage into a pass. |
| Tool output leaks secrets into DB/provider | `safety::redact` on all tool output; bash output caps; `.env`-like files denied by default. |
| Path escape / destructive command/browser action | Canonical project-root checks and permission modes exist; P13-T03 owns stronger command, browser, symlink and final-write enforcement before the daily-use boundary. |
| Provider incompatibility with tool calling | adapter validates `tool_calls` shape; text-only fallback keeps today's behavior when `tools` unsupported. |
| Context bloat, cost | budgets in the loop from P1; compaction in P3. |
| Scope creep | ROADMAP defines accepted coverage; TASKS is the executable backlog. New ideas must be mapped to both or explicitly dropped with a reason. |
