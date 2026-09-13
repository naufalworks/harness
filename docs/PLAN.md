# PLAN — Harness as a durable coding agent

Status: living document. `docs/ROADMAP.md` is the active continuation sequence,
`docs/TASKS.md` is the executable backlog, and `docs/PROGRESS.md` is the journal.

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

## 2. What exists today (keep)

- Recording-first admission: message + receipt + outbox commit before any provider call.
- Serial generation worker owns the provider call; tabs never own work.
- Write-once `context_json` = auditable "what the model saw".
- Review-first memory: extraction → candidates → human approval → FTS5 recall.
- Redaction before storage and before provider.
- Loopback-only, bearer auth, strict CSP.

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
Browser UI  ──SSE/poll──► activity_events, turn_steps, permission_requests
     │ submit / approve
     ▼
axum API ──► DbStore (SQLite, WAL) ◄── background workers
                                          ├─ agent_loop worker  (P1)  main agent: model↔tools until final answer
                                          ├─ context manager    (P3)  builds bounded window: rules, skills index, repo map,
                                          │                           recalled memories, plan, compacted history, recent steps
                                          ├─ recall manager     (P4)  FTS5 + local embeddings, memory kinds, usage-aware rerank
                                          ├─ verifier/advisor   (P5)  checks claims vs evidence, surfaces past decisions
                                          ├─ extraction worker  (now) candidates from user statements and corrections
                                          └─ compactor          (P3)  summarizes finished sessions → episodic candidates, repo map refresh
Tools (P1): read, grep, glob, edit, write, bash, think, todo_write   (P5+: task/subagent, skill, web_fetch; P6: ast_edit, lsp, browser)
```

### Turn lifecycle (P1 onward)

`captured → generating → [step*] → complete | failed | interrupted`

Inside `generating`, steps run serially: `model_call` → zero or more `tool_call`
(each possibly preceded by a `permission_request`) → `model_call` … → final answer.
Budgets: max steps per turn, max tool-output bytes, wall-clock limit. Exceeding a budget
ends the turn with a visible "budget exhausted" answer, never a silent stop.

## 5. Phases

### P0 — Gate: make the current checkpoint real
Historical gate, completed: the baseline compiled and its required suites ran before later phases proceeded. Current release evidence comes from the strict non-deploying release gate, not this historical statement.

### P1 — Tool loop with receipts per step
Provider adapter accepts tool definitions and tool calls; `turn_steps`,
`activity_events`, `permission_requests`, `file_changes`, `scopes`, `plan_items`
tables; tools read/grep/glob/edit/write/bash/think/todo_write; sandboxed path
resolution; permission gate; minimal UI rendering of steps and permission prompts
(polling). Acceptance: with the mock provider, a turn that reads a file, edits it via a
hash anchor, runs `bash`, and answers, is fully recorded; killing the process mid-tool
leaves the step `interrupted` and nothing is re-run on restart; an edit with a stale
anchor is rejected; a path outside `root_path` is rejected.

### P2 — Streaming and activity rail
Authenticated SSE over `activity_events` with a resumable cursor; three-pane layout;
live plan/todo, running-tool indicator, diff cards with accept/reject, token/cost chips,
context-budget meter. Acceptance: reconnecting mid-turn replays missed events exactly
once; UI shows every step within 500 ms of its commit; no XSS with adversarial tool output.

### P3 — Context Manager, compaction, repo map
Deterministic window builder with per-category budgets; tool-result compaction
(old results replaced by hash references); context compaction at ~70% with a receipt;
tree-sitter/ctags-style repo map per scope, refreshed by the compactor. Acceptance:
context receipt lists included and excluded parts with byte counts; a 40-step turn stays
under the token budget; re-reading an unchanged file returns a reference, not the body.

### P4 — Memory kinds, hybrid recall, ambient memory UI
Migration `004`: memory kinds `preference | fact | project | rule | skill | decision |
episodic | procedural`; local embeddings (ONNX, no API call) + FTS5 hybrid; rerank by
scope, recency, prior usefulness; corrections detected as high-signal candidates; inline
suggestion tray (Save / Edit / Dismiss) replaces the Memory tab as the primary surface.
Acceptance: a decision made in session A is recalled in session B by meaning, not
keyword; rejecting a suggestion never deletes chat history (existing invariant holds).

### P5 — Verifier/advisor, skills, sub-agents
Parallel cheaper-model verifier flags unverified claims and skipped diagnostics;
`skills/<name>/SKILL.md` progressive disclosure; `task` tool spawns a read-only explore
sub-agent with its own context returning a bounded summary. Acceptance: verifier badge
appears on turns whose answers cite files never read; a skill body is only in context
after being loaded.

### P6 — Structural tooling
`ast_edit` (ast-grep), `lsp` (diagnostics, references, rename), browser tool via CDP.

### P7 — Durable generation continuation
Generation output uses a persisted, authenticated, resumable event feed rather than
browser-owned work. The atomic provider boundary and idempotent restart recovery are
landed on `main`. Landed since then: per-event request attribution with proven
multi-turn cursor/SSE replay (`P7-T02c`), truthful migration-gate reporting (`P7-T04`),
and a chat UI that renders the durable feed with explicit complete, failed and
interrupted states (`P7-T03`). The phase's umbrella tasks (`P7-T01`, `P7-T02`) are
closed on that evidence. Subsequent work completed both remaining items. Publication before DONE landed as
`P7-T05`, now designed in `docs/design/incremental-publication.md`: the publication unit
is a completed line, because `safety::redact` erases a matched line whole and a released
prefix could never be retracted, and whole-answer buffering stays the active boundary
and is covered against `tests/test_incremental_publication.py`. The browser runtime and both mocked-browser suites landed as `P7-T06`; the strict release gate now additionally requires the real browser-to-service E2E lane.

### P8 — Causal observability
Current traces show sequence and outcome, but not the cross-component dependency chain that explains why a tool call happened, which evidence supported it, what state it changed, or where a failure first became possible. P8 adds bounded, typed provenance edges across memory/evidence, model claims, tool calls, permissions, observations, mutations, and recovery. It deliberately excludes hidden chain-of-thought and starts with one coding-turn incident graph before any broad observability platform work.

Acceptance: a reviewer can start from a denial, stale-anchor failure, or crash recovery and navigate to the earliest known causal break, all affected durable rows, and the recovery action. Missing provenance is reported as unknown, not guessed.

### P9 — Review hardening

Closed bounded incident-graph edge/adjacency integrity and Python SQLite lifecycle
warnings. P1–P9 form the verified baseline for continuation work.

### P10–P13 — Safety, performance, maintainability, security

Close remaining incident projection references; enforce one process per database;
add health/build identity, restore drills and rollback; replace stream polling with
commit notifications; improve SQLite read/query/retention behavior; decompose large
modules; introduce typed contracts; strengthen CI/release gates; and harden auth,
privacy, browser, command and path boundaries.

### P14–P16 — Daily workflow, memory, and observability

Add cancellation/retry, session and permission workflows, provider/tool improvements,
modular accessible UI, inspectable retrieval and memory governance, and measurable
causal incident comparison/search/export without hidden-reasoning capture.

### P17–P18 — Memory Wind Tunnel and optional platform evolution

Build immutable run capsules, strict/live/hybrid memory treatments and evidence-bound
comparison reports. Multi-worker, plugin and remote execution remain optional and may
start only after the earlier safety contracts are complete.

## 6. Non-goals (for now)
Multi-user, remote bind, enabling exact-original archive by default, OpenAI-
compatible proxy API, autonomous background coding without a human in the loop.

## 7. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Code written by an AI without compiling | P0 gate; `needs-verify` status; owner runs `cargo test` between tasks. |
| Tool output leaks secrets into DB/provider | `safety::redact` on all tool output; bash output caps; `.env`-like files denied by default. |
| Path escape / destructive bash | canonicalized root check; deny-list of destructive patterns in `ask` mode; permission gate default. |
| Provider incompatibility with tool calling | adapter validates `tool_calls` shape; text-only fallback keeps today's behavior when `tools` unsupported. |
| Context bloat, cost | budgets in the loop from P1; compaction in P3. |
| Scope creep | ROADMAP defines accepted coverage; TASKS is the executable backlog. New ideas must be mapped to both or explicitly dropped with a reason. |
