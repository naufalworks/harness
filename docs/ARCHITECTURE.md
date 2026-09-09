> Checkpoint update: chat generation is now owned by `recording::worker` and uses a durable receipt/outbox, not a request-owned provider call. `docs/RECORDING_PROTOCOL.md` is authoritative for chat capture, context, recovery and history. Remaining inherited memory/import architecture follows.

# Architecture decisions

## Scope

Single user, one local process, one SQLite database. Scope labels partition recall and proposals, but are not a tenant-security system. The shared local token authorizes the entire instance. No remote bind is allowed in this release.

## Capture and privacy

The old service captured trimmed text and import summaries. This version captures sanitized user/assistant messages, full sanitized source text, warnings, and normalized extraction payloads. Sensitive-looking content is excluded before normal storage/provider use. This deliberately prioritizes reducing accidental secret retention over claiming a lossless raw archive.

Non-sensitive source lines are preserved; redacted JSONL records are reserialized as valid JSON. Malformed lines and unsupported source records remain in sanitized source text with warnings. JSON/tool formats are version-specific adapters, not a claim of complete fidelity across every agent version. Junie headings inside fenced code are not treated as role boundaries; ambiguous unfenced exact role headings still require format-aware review.

An encrypted, opt-in exact archive belongs in a later isolated subsystem. The current database is plaintext and backups contain private data. Use owner-only filesystem permissions and do not put it into ordinary source control.

## Database access and durability

`DbStore::run` isolates synchronous rusqlite work using Tokio spawn_blocking. A semaphore bounds concurrent blocking tasks; a mutex serializes connection access. No database mutex is held across a provider await. A queue-full or lock/storage failure is an error, never an implicit success.

The schema is transactional and versioned. WAL + FULL synchronous, foreign keys, constraints, and FTS triggers are enabled. Local filesystem durability still depends on the OS/storage respecting sync. Backup is a separately verified online SQLite backup, not copying the live .db alone.

This release does not enforce a cross-process lease. Run only one service process per DB. Startup recovers running jobs to pending and pending chat messages to failed; a second process could disrupt those states. A future process lock / leased multi-worker design must precede multi-instance deployment.

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
