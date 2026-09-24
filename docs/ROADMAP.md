# Harness continuation roadmap

Status: active from 2026-09-13. `docs/PLAN.md` defines principles, this file defines sequencing and coverage, `docs/TASKS.md` is the executable backlog, and `docs/PROGRESS.md` is the append-only journal.

## Priority and parallelism

Priority order is `critical` → `high` → `medium` → `low` → `research`. Tasks marked `release-blocker` take precedence within an eligible priority tier; ties then use the earlier stable task ID. Execution waves describe product sequencing, but a later-wave release-contract or documentation correction may run early when it blocks truthful evidence.

A task may run in parallel only when its `parallel: yes` metadata is present, every dependency is done, it uses a different lane from other active work, and its declared files do not overlap. Each parallel task uses its own branch/worktree. Commits are merged one at a time, rebased before merge, and the task's exact verification plus the release gate are rerun after integration. One agent still owns only one task at a time.

## Execution waves

| Wave | Goal | Phases | Parallel lanes |
|---|---|---|---|
| A | Close correctness and operational safety gaps | P10 | incident, runtime, backup, release |
| B | Remove avoidable latency and storage pressure | P11 | streams, database, assets |
| C | Make change safer and faster | P12 | architecture, API, CI, docs |
| D | Harden the exposed deployment | P13 | auth, privacy, tools, audit |
| E | Improve daily product use | P14 | workflow, frontend, providers, languages |
| F | Make memory inspectable and controllable | P15 | retrieval, governance, history |
| G | Turn traces into measurable diagnosis | P16 | incident UX, metrics, deployment provenance |
| H | Build the Memory Wind Tunnel | P17 | capsules, replay, experiments, reports |
| I | Optional scale and ecosystem work | P18 | workers, plugins, portability |
| J | Record external development history | P19 | contract, capture, delivery, history UX |
| K | Make Harness reliable as the long-term brain | P20 | backup, restore, health, continuation, multi-agent |
| L | Expose safe configuration and feature coverage in the UI | P21 | providers, model picker, server folder chooser, activity, navigation |

Critical tasks in Wave A start first. Independent CI/docs/backup work may proceed beside runtime work. Schema-writing tasks never run in parallel with another schema-writing task.

## Safe daily-use release boundary

The first daily-use release is bounded to recovery, cancellation, tool boundaries, truthful verification, and a minimal fail-closed spend limit. Promotion requires: a strict non-deploying release gate with real browser-to-service evidence; a documented schema/rollback policy; crash and write-fault evidence; durable cancellation without replay; final-write path/command/browser policy; and a hard per-turn/day cost ceiling. Performance, research, ecosystem, optional voice, broad language expansion, and dashboard work do not block this boundary. Representative coding fixtures will report deterministic task acceptance, unsupported completion claims, stale-edit rejection, unauthorized mutations, restart/retry duplication, recall usefulness, latency, and cost; thresholds must be chosen from measured baseline and owner needs rather than invented in advance.

## P10 — Correctness and operational safety

Close the bounded-graph reference gap; use causal rather than insertion-order projection; enforce one process per DB; expose readiness/build/schema identity; automate encrypted backup rotation and clean restore drills; add graceful shutdown, deployment rollback, and fault/crash-point tests. This includes WAL integrity, disk-full/read-only/corruption behavior, request reconciliation, queue/backpressure visibility, background-process lifecycle, and stale-deployment detection.

## P11 — Runtime and storage performance

Replace 200 ms per-connection SQLite polling with commit notifications plus cursor replay; share fan-out where useful; separate serialized writes from bounded reads; reuse prepared statements; audit indexes and query plans; benchmark production-sized histories; manage WAL/vacuum/retention; compact old generation chunks; consolidate hidden-tab polling; compress and fingerprint static assets; split lazy frontend features; deduplicate unchanged output; review optional tool features, TLS choice, and duplicate dependencies.

## P12 — Maintainability, API, CI, and release engineering

Decompose `main.rs`, `agent_loop.rs`, `storage.rs`, `browser_tool.rs`, and `lsp_tool.rs`; introduce typed API/database DTOs and state enums; centralize bounds; remove or connect dead code; fix risky numeric conversions and nested patch options; audit production panics; standardize API errors; publish an OpenAPI contract and generated client; keep docs truthful. Add format/lint gates, warning budgets, dependency/license audits, pinned actions, Python/shell/JS lint, coverage, property/fuzz tests, fallback-tool tests, architecture checks, performance budgets, reproducible signed artifacts, SBOMs, public smoke tests, and rollback tests.

## P13 — Security, privacy, and auditability

Add per-route size/rate limits; short-lived browser sessions and token rotation; reverse-proxy identity options and HSTS; encrypted exact-original archive and encrypted backups with external keys; retention/deletion controls and secret-manager integration; browser network/upload/download policy; stronger path TOCTOU defense; structured command policy; sanitized audit export and optional hash-chain integrity. Preserve loopback-first and single-user assumptions until an explicit security phase changes them.

## P14 — Workflow, tools, providers, and UX

Add durable cancellation and safe-boundary retry; session naming/search/archive/fork; stop/regenerate controls; permission bundles/countdowns/notifications; dry-run plans; active process viewer; Git-aware changes, focused commit proposals, and checkpoints; bounded parallel read-only sub-agents; provider capability detection/fallback/circuit breakers/Retry-After/cost limits; AST/LSP language expansion and session reuse; browser screenshots/artifacts and transfer controls. Modularize and virtualize the UI; add keyboard/mobile/accessibility work, reconnect states, better diffs, context/cost/project dashboards, and optional voice input.

## P15 — Memory and history

Add optional semantic embeddings while retaining deterministic local recall; create recall evaluation fixtures; persist and explain inclusion/exclusion receipts; preview retrieval changes before approval; add branches, decision timelines, temporary expiry, conflict grouping, deduplication, usefulness feedback, sanitized conversation/artifact FTS, separate forget/source deletion, portable import/export, and explicitly pinned global profile entries.

## P16 — Causal observability

Add graph expansion/search/timeline/export and run comparison; live incident formation; missing-edge, earliest-break, graph-size, and reviewer-time metrics; confidence labels that distinguish recorded dependency from temporal proximity; graph retention; anomaly flags; and deployment/build/restart/smoke-test provenance. Reports must continue to say `unknown` when evidence is absent and must not capture private chain-of-thought.

## P17 — Memory Wind Tunnel

Define immutable content-addressed run capsules and deterministic assertions; freeze project/model/tool/memory/context state; validate and fork isolated treatments; implement strict, live, and hybrid replay; add no-memory, remove-one, stale, conflict, pollution, and poisoned-memory treatments; align traces by semantic step identity; report first divergence, deterministic outcomes, costs, repeated-trial uncertainty, and sanitized evidence. Remote disposable runners remain opt-in with image pinning, TTL, spend cap, kill switch, and proven cleanup.

## P18 — Optional platform evolution

Only after the earlier phases: leased multi-worker execution, multi-instance semantics, plugin/provider SDKs, portable continuation packets across machines, benchmark packs, signed extension manifests, and isolated remote runners. Multi-user tenancy remains a separate product/security decision rather than an accidental consequence of scaling.

## P19 — development-mcp history integration

Accept development activity observed by `development-mcp` as durable Harness history: a versioned
external-event contract, producer scope and redaction policy, idempotent ingestion with receipts,
isolated per-session capture in the producer, acknowledgement-driven delivery, evidence-linked
session history, review-first memory from external evidence, and recovery qualification. External
ingestion never invokes the agent loop and never replays development actions. Conversation text is
recorded only when a supported client supplies it; MCP hosts retain full chat history.

## P20 — Long-term memory reliability

Keep reviewed exports distinct from full internal recovery snapshots. Backups are
transactionally consistent and manifest-verified; restores validate checksum and schema
before publication. A retained memory baseline detects unexpected reset, while
agent-independent continuation envelopes and stable agent-session links let GPT, Claude,
Codex, and future clients resume shared Harness project context without inventing
conversation text that the client did not provide. P20 is complete; its task evidence is
recorded in `docs/TASKS.md` and `docs/PROGRESS.md`.

## P21 — UI control center and configuration coverage (planned)

Audit the existing HTTP surface against current browser controls and close owner
workflow gaps without equating backend availability with usable UI. Build securely
stored custom-provider profiles (including YAML/JSON input of an
`openai-completions` / proxy-discovery profile), safe connection testing, and
model selection from the chosen provider's `/models`, with manual ID fallback.
Add a server-side allowlisted folder chooser plus typed absolute-path support,
provider-aware role settings, and truthful live activity/progress displays that
do not expose raw hidden reasoning. Finish with feature-coverage navigation,
accessibility and browser-to-live-server tests. Detailed boundaries and task
ordering: `docs/design/p21-ui-control-center.md` and P21 in `docs/TASKS.md`.

## Coverage contract

The executable P10–P18 tasks cover every accepted audit opportunity from the 2026-09-13 review
(P19 was added later for external development-history integration and is outside that review):

- reliability/correctness: graph closure and projection, process ownership, crash/disk/DB recovery, queues, background jobs, graceful deployment;
- performance: event-driven streams, DB concurrency/query plans/retention, frontend polling/assets/build footprint;
- maintainability: module decomposition, typed contracts, warning and panic cleanup, centralized limits, documentation;
- security/privacy: auth/rate/body boundaries, encryption/key management, deletion, browser/bash/path policy, audit integrity;
- CI/release: format/lint/audit/coverage/fuzz/property/cross-target gates, reproducible artifacts, smoke and rollback;
- agent/tools: cancellation, retry, policies, dry-runs, process/Git checkpoints, parallel reads, provider/language/browser improvements;
- memory: semantic retrieval, evaluation/explanations, rehearsal, branches/timeline/expiry/conflicts/history/export/pinning;
- observability: compare/search/timeline/export, causal metrics, anomalies, deployment provenance;
- UX: sessions, keyboard/mobile/a11y, virtualization, connection state, approvals, diffs and dashboards;
- research: capsules, strict/live/hybrid treatments, statistics, reports and optional remote isolation.

An item may be split into smaller tasks during design, but it may not be silently removed. Mark it `dropped` in `docs/TASKS.md` with the owner's reason if it is intentionally rejected.
