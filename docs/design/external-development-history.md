# External development history contract — v1

Status: contract for P19-T01. Enforcement lands in P19-T02 (scope/privacy) and P19-T03 (ingestion).

Harness accepts development activity observed by an external producer (`development-mcp`) as
durable history. This document defines the envelope, the deterministic meaning of every
submission outcome, and the vocabulary for partial capture. It introduces no provider
generation and no agent-loop entry point.

## 1. Envelope

Schema string: `harness.external-history/v1`. A producer MUST send the exact string; an
unrecognised value is rejected as `unsupported_schema_version` rather than coerced.

| Field | Required | Meaning |
|---|---|---|
| `schema_version` | yes | Contract identifier |
| `event_id` | yes | Producer-scoped stable identity used for deduplication |
| `content_digest` | yes | SHA-256 over the canonical payload; separates replay from conflict |
| `producer_id` | yes | Configured producer identity, never read from a tool argument |
| `producer_instance_id` | yes | Process incarnation; changes on restart |
| `producer_sequence` | yes | Monotonic per instance; gaps are detectable, not fatal |
| `project_id` | yes | Authorization scope, validated against the producer grant |
| `logical_session_id` | yes | Development session grouping |
| `invocation_id` | conditional | Required for `tool.*` events |
| `task_id` | conditional | Required for `task.*` events |
| `occurred_at` | yes | RFC 3339 UTC at the producer |
| `event_type` | yes | Section 2 |
| `outcome` | yes | Section 3 |
| `capture` | yes | Section 4 |
| `payload` | yes | Sanitized, bounded, type-specific body |

`ingested_at` is assigned by Harness. A producer-supplied value is rejected, never trusted.

## 2. Event types

`session.opened`, `session.closed`, `tool.admitted`, `tool.started`, `tool.completed`,
`tool.failed`, `request.rejected`, `task.started`, `task.output`, `task.completed`,
`task.interrupted`, `artifact.recorded`, `capture.gap`, `message.observed`.

An unlisted value is rejected as `unknown_event_type`. `message.observed` is valid only when a
supported client actually supplied the text; it is never synthesised.

## 3. Outcome: transport is not execution

`outcome` carries three independent fields, because collapsing them is the primary way a
history becomes untruthful:

- `transport` — `ok` | `error`. The handler returned or raised.
- `execution` — `succeeded` | `failed` | `unknown` | `not_applicable`. What the underlying
  operation actually did.
- `exit_code` — integer, or null for non-process work.

A handler that returns normally while the command it ran exited non-zero MUST serialize as
`transport: ok` with `execution: failed`. A crash between side effect and completion record MUST
serialize as `execution: unknown`; it is never upgraded to `succeeded` and never replayed.
A handler error can follow a successful side effect; it does not contradict independently
observed execution success. Nonzero exit status with execution success is rejected as
`malformed_envelope`. Transport here means handler outcome, not proof of client delivery.

## 4. Capture coverage

`capture.conversation` is `client_supplied`, `unavailable`, or `not_supported`. MCP hosts retain
conversation history, so `unavailable` is the expected default and MUST NOT render as an empty
conversation. `capture.payload` is `full`, `summarized`, or `omitted`; `capture.truncated` is a
boolean. A `capture.gap` event records a known missing sequence range.

## 5. Submission outcomes

| Condition | Result |
|---|---|
| New `event_id` | Accepted, committed, acknowledged |
| Same `event_id`, same `content_digest` | Idempotent replay: acknowledged, no second record |
| Same `event_id`, different `content_digest` | Rejected `duplicate_event_id_conflict` |
| Sequence below a recorded value | Accepted as late arrival; ordering uses sequence, not receipt time |
| Unknown schema | Rejected `unsupported_schema_version` |
| Missing required field | Rejected `missing_required_field` |
| Unlisted event type | Rejected `unknown_event_type` |
| Non-RFC-3339 timestamp | Rejected `invalid_timestamp` |
| Contradictory or out-of-vocabulary enum | Rejected `malformed_envelope` |
| `project_id` outside the producer grant | Rejected `scope_not_permitted` |
| Unsanitized secret-bearing field | Rejected `redaction_missing` |

Acknowledgement is emitted only after durable commit. An unacknowledged event MUST be retried by
the producer; retry redelivers the record and never re-executes the development action.

## 6. Boundaries

External ingestion does not submit chat turns, invoke tools, or call a provider. It is not an
encrypted exact-original archive; that surface stays opt-in and separate.

## Offline conformance profile

P19-T01 checks structural conformance only. It does not enforce authentication,
redaction, durable commit, or actual receipt deduplication. Those belong to later tasks.
`invalid_digest` rejects changed content or malformed SHA-256 values.
The digest covers the entire envelope excluding `content_digest`, not only payload.
Canonical bytes are compact UTF-8 JSON with lexicographically sorted ASCII object keys,
no ASCII escaping of Unicode text, no floats, and no whitespace outside strings.
Fixture-only `__expect` is removed before validation and is not a wire field.
Deduplication identity is `(producer_id, event_id)`, across producer restarts.

Identifiers are 1–128 ASCII characters matching `[A-Za-z0-9][A-Za-z0-9_.:-]*`.
Object keys match `[a-zA-Z_][a-zA-Z0-9_]{0,63}`. Integers are bounded to
9007199254740991 in magnitude; booleans are not integers. Sequences are positive.
Containers have at most 256 entries, nesting at most 12 levels, strings at most
16384 UTF-8 bytes, and serialized envelopes at most 65536 bytes. Unknown envelope
fields are rejected, including producer-supplied `ingested_at`; Harness adds its own.
UTC timestamps use `Z`, valid calendar dates, and at most six fractional digits.
Exit codes are null or signed 32-bit integers. Non-process results use null.
Client-supplied message payloads carry role, text and source_client; unavailable
conversation does not mean empty dialogue. Capture-gap ranges are positive and ordered.
Artifact references carry artifact_id, media_type, byte_count, digest and truncated;
bytes are not implicitly archived by referencing them. Arguments/results/error details
are sanitized evidence, never trusted instructions. Sequence order applies within a
producer instance only; late arrival is accepted without claiming global clock order.
