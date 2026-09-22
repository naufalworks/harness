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


## P19-T02 authorization and privacy boundary

`AuthState::authorize_external` is the single authorization/privacy preparation boundary.
It does **not** validate the entire v1 envelope, verify its digest, commit anything, or
acknowledge a receipt. P19-T03 must perform those checks and use this boundary before any
record, diagnostic detail, export, extraction input, or search index is written. This task
adds no ingestion route, external-history schema, or development-mcp capture behavior.

Owner-controlled `HARNESS_HISTORY_PRODUCERS` is a bounded JSON array of objects with exactly
`producer_id`, `token`, and `projects`. Tokens are independent, 32–256 printable ASCII bytes;
operators must generate high-entropy values and protect environment/configuration access.
`projects` is a nonempty list of exact project/scope IDs, with no wildcard or path expansion.
Absent configuration or `[]` enables no producers. Invalid/duplicate grants, duplicate
credentials, unknown fields, and collisions with current/previous owner tokens fail startup
with a static error that does not echo configuration. Changing grants requires restart;
there is no remote grant-management API. Startup wiring in `src/main.rs` is the only change
outside the task's declared runtime areas.

A producer token never becomes owner/browser authority and is refused by existing history,
memory-confirmation, session-minting and archive routes. Owner/browser tokens are not
producer credentials. Submitted producer identity must match the authenticated grant;
project IDs must match its exact allowlist. Correlation keys are tuples of trusted producer,
authorized project and validated external ID, never bare session/task/invocation/artifact
IDs or delimiter-concatenated strings. Future storage must not use nested payload IDs to
resolve another scope's resources. Scoped correlation does not replace the contract's
producer-wide `(producer_id,event_id)` deduplication key: ingestion must also reject replay
of an event ID with a changed project/digest.

The body is capped at 65,536 bytes before parsing. Duplicate keys (including escaped aliases),
invalid JSON/UTF-8, floats, out-of-range integers, invalid object keys, oversized strings or
containers, and excessive nesting are rejected. All decoded object keys and string values
are checked recursively, including arguments, titles, paths, results, errors, output and
artifact metadata. Recognized secret patterns, credential-bearing URLs, sensitive keys,
and known producer/owner/browser credentials produce `redaction_missing`. A failed privacy
state check produces `privacy_check_failed`; errors contain no submitted text. Rejection
returns no evidence object, so callers must not persist/log/index/export the rejected body.
Private fields and immutable accessors keep an accepted scope/value pair bound together.

This is deliberately conservative pattern recognition, **not complete DLP**. Arbitrary,
encoded, split or otherwise unrecognized secrets may escape detection; producers still
must sanitize at source. Unknown secrets cannot be guaranteed absent. Clean redaction
markers are accepted; secrets are rejected rather than silently rewritten, preserving the
producer digest. Artifact references do not authorize fetching a URL/path or retaining bytes.
Any future fetched bytes need their own privacy boundary before use.

### Retention, deletion and exact content rules

External source policy includes its sanitized events, artifact metadata/content, exports,
indexes, extraction candidates and derived memories; do not retain detached copies as a
way around source expiry/deletion. The existing separate privacy intents remain distinct:

- **Forget:** exclude derived candidates/memories from approval/recall; source history is
  unchanged unless separately deleted.
- **Purge index:** remove derived searchable material (including artifact text) without
  claiming the source or archive has been deleted.
- **Delete source / retention expiry:** remove the scoped source and its artifact copies,
  purge its indexes/exports, and invalidate derived memory/extraction material before it can
  be recalled or re-indexed. Shared derivations must be recomputed from still-authorized
  sources or excluded. Keep only non-content-bearing audit/tombstone evidence needed to
  enforce deletion and prevent replay from resurrecting removed content.
- **Exact archive:** never implied by ingestion or a reference. Require separate owner opt-in,
  encrypted storage with external keys, and explicit deletion of associated archive objects
  using the existing audited archive-deletion path. Unconfigured means disabled; partial or
  invalid archive configuration fails startup, never silently enables plaintext fallback.

These are policy requirements for future external storage/retention consumers, not a claim
that external-event deletion jobs exist in P19-T02. No new retention period is invented here.
Existing encrypted archive tests remain the regression proof for encryption/deletion; the
new test additionally checks disabled versus partial configuration without mutating process
configuration. Simulated sanitized sinks test privacy output, not database ingestion.


## P19-T03 durable ingestion

`POST /external-history/events` accepts one v1 envelope with a producer Bearer
credential. P19-T02 authorization/privacy checks precede full envelope and SHA-256
validation. Optional invocation/task IDs may be null for unrelated event types, as
permitted by the v1 fixtures; required tool/task identities may not be null.

The body is capped at 65,536 bytes with a ten-second read deadline and the existing
shared API concurrency semaphore. Owner/browser credentials are not producer authority.
Unreadable, oversized or timed-out bodies return 413; invalid envelopes/privacy/digests
400; unauthorized producers 401; scope refusal 403; conflicting identity reuse 409;
storage/concurrency unavailability 503. Errors contain static codes, not submitted text.

Migration 021 stores evidence and its receipt together in an immutable row. The existing
serialized writer uses an IMMEDIATE transaction and commits before acknowledgement.
201 means new committed history; 200 means an identical retry. Both return receipt_id,
ingested_at, state=committed and replay. Retries preserve the original receipt and time,
including after process restart or a lost HTTP response. Producer-wide event IDs with
changed digests are conflicts, never updates. Lower sequences are accepted without
claiming global ordering. Append-only triggers reject updates/deletes.

There is no extraction intent, provider call, tool dispatch or chat submission, and no
separate outbox consistency window. Restart needs no replay worker: stored rows are final.
An unacknowledged producer retries its envelope, not the original development operation.
Existing databases upgrade additively to schema 21; older binaries cannot open schema 21.

Limitations: external read UI, exporters, retention/deletion jobs and volume quotas are
not introduced. Future audited deletion must reconcile the source policy with immutable
receipt/tombstone semantics. Pattern-based privacy is not complete DLP. P19-T04, MCP
capture and client integrations remain out of scope.


## P19-T06 evidence-linked reads

Owner/browser credentials can read `/external-history/sessions`, `/external-history/activity`
and `/external-history/artifact`; producer credentials cannot. Harness is single-owner:
project filters are exact selection constraints, not new multi-user authorization grants.
Activity/artifact reads require the full project/producer/session tuple. Event identity is
producer-scoped; receipt identity links each displayed event to its immutable evidence.

Pages are capped at 100 records. `after` is an exclusive durable arrival rowid, not producer
sequence. Identical retries add no row; late sequence values remain discoverable by resume.
The UI groups instances and sorts loaded records by producer sequence within each instance.
It does not claim global ordering, all pages loaded, or an unseen terminal outcome. Session
pagination uses first arrival; refresh discovery to update existing session counts.

Reads do not fetch external paths/URLs or alias internal artifact IDs. Artifact views expose
only scoped ingested evidence; referenced bytes are explicitly unavailable (no byte ingest
contract exists). Conversation absence means unavailable, not empty. Optional observed
messages show source client, role, session and supplied task/invocation linkage. Payloads are
rendered as text, never HTML. Lock clears visible evidence and invalidates in-flight responses.
Harness commit is known; producer receipt of acknowledgement and local undelivered backlog
remain unknown. No provider, extraction, tool execution, archive activation or retention
change is introduced. Arrival cursors rely on the append-only rowid table; future deletion,
rebuild or VACUUM work must preserve cursor identity or version the resume contract.
