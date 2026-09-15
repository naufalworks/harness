> Checkpoint update: chat generation is now owned by `recording::worker` and uses a durable receipt/outbox, not a request-owned provider call. `docs/RECORDING_PROTOCOL.md` is authoritative for chat capture, context, recovery and history. Remaining inherited memory/import architecture follows.

# Architecture decisions

## Scope

Single user, one local process, one SQLite database. Scope labels partition recall and proposals, but are not a tenant-security system. A master token authorizes the entire instance; the browser exchanges it once for a random, server-side session with a 15-minute default absolute lifetime. Both credentials stay only in JavaScript memory: they are never placed in URLs, DOM state, `localStorage`, or `sessionStorage`. No remote bind is allowed in this release.

## Authentication and request boundaries

Set `HARNESS_AUTH_TOKEN` to 32–256 non-whitespace ASCII characters. For restart-safe rotation, deploy a new value there and temporarily put the old value in `HARNESS_AUTH_TOKEN_PREVIOUS`; after clients have reconnected, remove the previous value and restart. Current and previous master tokens use constant-time comparison. Browser sessions cannot create more sessions, are capped at 128 live entries, are pruned on access, and expire absolutely. `HARNESS_SESSION_TTL_SECONDS` may tighten or extend the lifetime from 300 to 3600 seconds.

Failed bearer authentication waits a uniform 150 ms before returning 401. Authenticated traffic retains the global eight-request concurrency bound and also has one-minute per-route limits: 120 reads, 30 writes, and 8 imports. JSON bodies default to 64 KiB; `/memory/ingest` alone accepts up to 2 MiB. These are local abuse boundaries, not a multi-tenant quota system.

`HARNESS_PROXY_IDENTITY_HEADER` is disabled by default. When enabled, every authenticated API request must carry a printable, non-empty identity in that exact header, and rate accounting is separated by identity. Enable it only behind a trusted reverse proxy that removes every client-supplied copy and writes its own authenticated identity; forwarding an untrusted header defeats the boundary. Bearer authentication remains mandatory.

The loopback listener is HTTP, so it does not emit HSTS by default. A TLS-terminating deployment may set `HARNESS_HTTPS_HSTS=1` only when the public origin is HTTPS and all HTTP traffic is permanently redirected; this adds `Strict-Transport-Security: max-age=31536000; includeSubDomains`. Misusing HSTS on a partially migrated domain can make sibling services unreachable.

## HTTP surface

Every route the process serves. `scripts/check_docs.py` compares this list
against `router()` in `src/api/routes.rs` in both directions, so a route cannot
be added, renamed or removed without this section changing in the same commit.
Authenticated routes sit behind bearer authentication, which is checked before
existence: an unauthenticated request never learns whether a path exists.

### Authenticated

- `POST /chat`
- `POST /chat/submit`
- `GET /chat/requests/{id}`
- `POST /chat/requests/{id}/cancel`
- `POST /chat/requests/{id}/retry`
- `GET /chat/requests/{id}/context`
- `GET /sessions`
- `PATCH /sessions/{id}`
- `POST /sessions/{id}/fork`
- `GET /models`
- `GET|POST /config`
- `GET /memory/status`
- `GET /health`
- `GET /memory/candidates`
- `POST /memory/candidates/{id}/edit`
- `POST /memory/confirm`
- `POST /memory/ingest`
- `GET /sessions/{id}/messages`
- `GET /jobs`
- `POST /jobs/{id}/retry`
- `GET /processes`
- `POST /processes/{pid}/stop`
- `GET /git/state`
- `GET /scopes`
- `GET|POST /scopes/{scope}`
- `GET /permissions`
- `POST /permissions/{id}`
- `GET /chat/requests/{id}/steps`
- `GET /chat/requests/{id}/incident`
- `GET /sessions/{id}/plan`
- `GET /activity`
- `GET /activity/stream`
- `GET /generation`
- `GET /generation/stream`
- `GET /changes`
- `POST /changes/{id}/revert`
- `GET /chat/requests/{id}/provenance`
- `GET /chat/requests/{id}/retrieval`
- `POST /memory/retrieval/preview`
- `GET /memory/governance`
- `GET /memory/entries/{id}/timeline`
- `POST /memory/entries/{id}/governance`
- `POST /memory/branches`
- `POST /sources/{id}/archive`
- `POST /sources/{id}/privacy`
- `GET|DELETE /archives/{id}`

### Unauthenticated

- `GET /`
- `GET /api.js`
- `GET /app.js`
- `GET /style.css`
- `POST /auth/session`

## Capture and privacy

The old service captured trimmed text and import summaries. This version captures sanitized user/assistant messages, full sanitized source text, warnings, and normalized extraction payloads. Sensitive-looking content is excluded before normal storage/provider use. This deliberately prioritizes reducing accidental secret retention over claiming a lossless raw archive.

Non-sensitive source lines are preserved; redacted JSONL records are reserialized as valid JSON. Malformed lines and unsupported source records remain in sanitized source text with warnings. JSON/tool formats are version-specific adapters, not a claim of complete fidelity across every agent version. Junie headings inside fenced code are not treated as role boundaries; ambiguous unfenced exact role headings still require format-aware review.

The opt-in exact-original archive is isolated in `src/archive/`: callers must explicitly pass the original bytes, an owner-only archive directory, and a current external 256-bit key file. Versioned AES-256-GCM envelopes authenticate their metadata and ciphertext; SQLite stores only non-secret metadata and append-only privacy events. A previous external key may be supplied during rotation so old archives remain readable. No archive key or exact original enters the ordinary sanitized database. The subsystem is not enabled by default and no production key is generated automatically.

## Database access and durability

`DbStore::run` isolates synchronous rusqlite write work using Tokio spawn_blocking. A semaphore bounds concurrent blocking tasks; a mutex serializes the single write owner. `DbStore::read` provides bounded read projections through a separate reader pool for file-backed SQLite databases, while in-memory test databases safely fall back to the writer connection because SQLite memory databases are connection-local. No database mutex is held across a provider await. A queue-full or lock/storage failure is an error, never an implicit success.

The schema is transactional and versioned. WAL + FULL synchronous, foreign keys, constraints, and FTS triggers are enabled. Local filesystem durability still depends on the OS/storage respecting sync. Backup is a separately verified online SQLite snapshot, not copying the live `.db` alone.

Operational backups use `scripts/backup.py create`: the online snapshot is encrypted with a versioned AES-256-GCM envelope from the reviewed Python `cryptography` package, authenticated before restore, published through an owner-only temporary file plus atomic rename, restored into a clean temporary target for every creation, and rotated only after that drill succeeds. The required 256-bit key lives in a separate owner-only file and is never stored beside or inside the archive. `keygen` creates that file, `restore` refuses to overwrite a destination, and `drill` verifies an archive without touching the live database. Missing/wrong keys, corruption, short writes, quota errors, and failed restore validation leave no published target and never modify the source database. Plaintext `backup()` remains an internal short-lived migration snapshot helper only.

Example: `python3 scripts/backup.py keygen /secure/harness-backup.key`, then `python3 scripts/backup.py create data/harness_v2.db /secure/backups --key-file /secure/harness-backup.key --retain 7`. During rotation, new backups use `--key-file <current>` while `--previous-key-file <previous>` keeps the immediately preceding generation restorable; retire the previous key only after its archives expire or are re-encrypted. Production key placement and off-host copy policy remain owner decisions.

Privacy operations are deliberately separate. `forget` marks memory intent, `delete_source` marks removal of the sanitized source, `purge_index` marks derived-index removal, and `delete_archive` removes the ciphertext. Migration 007 records each operation with a constrained action and append-only trigger, while per-source timestamps make incomplete deletion workflows visible; no one operation silently implies another.

Startup acquires a kernel-backed exclusive process lock for the normalized database path before SQLite recovery. A second live process is rejected without mutating jobs or receipts; stale metadata is replaced only after kernel ownership is obtained. This remains a single-instance design rather than a leased multi-worker system.

SIGINT/SIGTERM stop HTTP admission and notify both durable workers. A claimed generation or extraction is allowed to finish its current durable boundary; idle workers stop immediately. Shutdown waits up to `HARNESS_SHUTDOWN_TIMEOUT_SECONDS` (30 by default, constrained to 1–300) and emits explicit start/complete/timeout events. Abrupt termination still uses startup recovery, which marks claimed generation interrupted and never replays its provider/tool side effects.

Fault-injection verification is destructive only inside disposable temporary directories. It kills helper processes at pre/post-commit boundaries, constrains SQLite pages to force `SQLITE_FULL`, opens databases with `mode=ro`, damages WAL frame checksums and database pages, reconciles committed-but-unacknowledged admission by request signature, saturates admission/extraction queues, and restarts around a detached command. Tests accept either explicit corruption rejection or a clean state that ignores an invalid WAL frame; they never treat silent partial state as success.

Detached bash commands intentionally outlive the Harness server after their tool result is durably complete. Startup does not replay completed steps or relaunch their processes. Their PID and project-local log are observational handles rather than a promise that Harness supervises them across restart.

Deployment snapshots the currently served executable and identity before building. Candidate readiness must match commit, SHA-256, schema, database, and worker health. A failed candidate automatically restores the previous executable only when the live `PRAGMA user_version` is no newer than the previous release's schema. If schema advanced, the service is stopped and database recovery requires explicit owner approval after reviewing backup age and writes accepted since that backup; deployment never restores a database automatically.

## Conversation flow

Only successfully completed prior turns are replayed. The user request is first recorded as pending; completion stores assistant output and queues memory extraction in one transaction. Provider/recall failures mark the user message failed. Database failures are reported without a successful-save claim. An abrupt cancellation may leave pending capture until restart; request reconciliation beyond restart recovery remains a release follow-up.

A single chat mutex rejects concurrent chat requests with a conflict response. This is an intentional single-user simplification, not a throughput optimization. Duplicate request identifiers fail rather than repeating an upstream call; persisted response replay for idempotency keys is not implemented.

## Memory approval

Only user quotes qualify as extraction evidence. Types, sizes, categories, sensitivity and quote membership are validated before candidate insertion. A model can still misinterpret a correct quote, so every candidate requires review.

Candidates have a 30-day expiry and an expected memory revision. One immediate transaction checks scope/status/expiry/revision, writes the current value and immutable revision, and resolves approval. Rejection never writes active memory. Pending candidates may be edited after the replacement value is revalidated; evidence, category and expected revision stay fixed. Conflicts are retained instead of silently rebasing.

No extraction call can directly activate or overwrite a memory. Chat candidates are associated with their request and shown inline; the Inbox filters to imports/backlog. Legacy imports retain explicitly unknown provenance. The old confidence value is not treated as approval or a calibrated probability.

## Recall

Recall lazily persists 256-dimensional normalized vectors from the bundled deterministic `harness-local-hash-v1` word/character/bigram feature hasher. It requires no network, model download, native runtime or provider call. Each vector is keyed by model and memory content hash, so stale values refresh before ranking.

Retrieval unions FTS5 top 20 with cosine top 20, then deterministically reranks by lexical position, cosine score, project scope, recency and prior usefulness. A project-specific active key suppresses its global counterpart. At most 20 snapshots and 6,000 serialized bytes are returned; sensitive values are skipped. Record identity, revision, scope and evidence remain untrusted reference data under fixed system policy.

The local feature hasher improves spelling/morphology overlap but is not a semantic language model and makes no synonym-quality claim. Episode/archive search, global profile pinning, graph traversal and full persisted retrieval traces remain future work.

## Durable extraction

A source and all chunk jobs are inserted in one transaction. Idempotency is per original-content hash, explicit format selection, scope and parser version. Accepted jobs survive crashes; running jobs reset on a single-process restart. Candidate insertion and job completion are atomic, with source-scoped uniqueness for retries. Provider calls themselves are at-least-once across crashes and may be charged more than once.

The single worker processes up to 1,000 outstanding jobs with a 45-second provider limit and up to three attempts. Live extraction sends exact user events as evidence plus separately labelled, untrusted current-plan context. Plan/assistant/tool text cannot be cited. Explicit corrections are deterministically marked high priority, and decisions/procedures remain review-only. A failed job can be retried in the UI. Invalid model output is a failed job, not a successful extraction of zero memories.

## Compatibility

This is a custom service, not a standards-compatible OpenAI proxy. The main-agent adapter supports OpenAI-style function tool calls and records complete assistant/tool replay. Extraction and compaction roles are deliberately text-only and reject tool calls. Provider-specific errors and unsupported-tool fallback never fabricate success.
