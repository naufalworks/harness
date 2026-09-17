# Runtime observability contract

Status: P16-T04 first slice

## Purpose

`harness.runtime/v1` provides bounded operational timing for one recorded turn. It helps identify where elapsed time was spent without becoming a second source of truth for request content, durable state, spend, or external effects.

## Trust and privacy boundary

The runtime report accepts only fixed stage durations, fixed counters, a fixed terminal outcome, and a fixed unavailable-field list. It has no fields for prompts, responses, session/request IDs, models, paths, tool arguments, tool output, errors, authorization data, credentials, or hidden reasoning.

The report is emitted as one JSON line to stderr after the terminal receipt is durably saved. Journald retention and access remain deployment concerns. Durable receipts and steps remain authoritative for what happened; this report says only how long bounded stages took in this process.

## Clock and units

Elapsed values use `std::time::Instant` and are reported as saturating integer milliseconds. UTC timestamps are not used to calculate durations. Stage values can overlap and therefore must not be summed to reconstruct `total_ms`.

## Schema

```json
{
  "schema": "harness.runtime/v1",
  "event": "turn_runtime_finished",
  "outcome": "complete",
  "durations": {
    "total_ms": 0,
    "context_ms": 0,
    "provider_ms": 0,
    "tool_ms": 0,
    "permission_ms": 0,
    "verification_ms": 0,
    "publication_ms": 0
  },
  "counts": {
    "provider_calls": 0,
    "tool_calls": 0
  },
  "unavailable": [
    "sqlite_queue_ms",
    "sqlite_read_ms",
    "sqlite_write_ms",
    "sqlite_commit_ms"
  ]
}
```

## Boundaries

- `total_ms`: entry to the generation function through successful terminal receipt persistence.
- `context_ms`: generation entry through persistence of the immutable first-call context receipt. It includes recall, scope lookup, repository/skill discovery, context building, retrieval receipt persistence, and context persistence.
- `provider_ms`: elapsed waits around measured provider futures. Each measured wait increments `provider_calls`. It includes cancellation polling performed while the provider future is pending.
- `tool_ms`: blocking registry invocation only. Permission waiting and durable pre/post effect records are excluded. Each invocation increments `tool_calls`.
- `permission_ms`: elapsed wait after a durable permission request exists until approval, denial, cancellation, expiry, or disappearance.
- `verification_ms`: the complete advisory verification path, including its durable step transitions and provider wait. Because provider time is also recorded, this overlaps `provider_ms`.
- `publication_ms`: time spent durably appending streamed generation chunks plus terminal receipt persistence.

A zero means the measured stage did not execute or completed below one millisecond. It does not mean an unavailable measurement. Unavailable measurements are named explicitly.

## First-slice limitations

SQLite queue/read/write/commit decomposition is deliberately unavailable. Adding it safely requires named storage-operation classes and instrumentation around the existing semaphore, reader pool, writer mutex, transactions, and commits. This first slice must not guess those values from whole-stage timings.

Compaction and delegated sub-agent provider/tool totals are not yet guaranteed complete. The initial deterministic fixture covers the normal main-agent provider/tool/verification path. Any report that requires complete nested-role accounting must wait for a follow-up task.

## Repeated fixture summary

The pure distribution helper sorts elapsed samples and reports:

- `sample_count`;
- lower median;
- nearest-rank p95;
- maximum.

Thresholds are not part of this slice. Baseline measurements must be collected before the owner approves regression budgets.

## Failure behavior

Operational emission is best-effort. Serialization or stderr failure must not turn a successfully persisted user request into a failure. Durable correctness writes retain their existing fail-closed behavior.

Cancellation, provider failure, context failure, and interrupted-turn terminal emission remain follow-up coverage. This slice emits the successful terminal report only and does not reinterpret existing failure states.

## Verification

The focused tests prove the report schema, repeated-sample distribution, explicit unavailable fields, and absence of sensitive field names. Existing agent-loop and strict release gates prove that instrumentation did not weaken recording, cancellation, fencing, spend, recovery, or no-replay behavior.
