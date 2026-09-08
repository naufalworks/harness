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

Candidates have a 30-day expiry and an expected memory revision. One immediate transaction checks scope/status/expiry/revision, writes the current value and immutable revision, and resolves approval. Rejection never writes active memory. Conflicts are retained as conflict records instead of silently rebasing to a new value. Conflict editing/reproposal UI is backlog work.

No extraction call can directly activate or overwrite a memory. Legacy imports start in a separate review scope with explicitly unknown provenance. The old confidence value is not treated as approval or a calibrated probability.

## Recall

FTS5 token matching includes short words and punctuation normalization, unlike the old substring prefilter. Search matches approved active heads in the requested scope and global scope. A project-specific active key suppresses its global counterpart. BM25 ranks candidates; deterministic byte/count budgets limit injected evidence.

The output contains record identity, revision, scope and source evidence. It is supplied as untrusted reference data under fixed system policy; approval does not turn source instructions into system authority. No tools are executed by this release. This reduces a risky path, but prompt injection resistance is not proven solely by delimiting data.

No semantic synonym expansion, episode/archive search, global profile pinning, reranking model, graph traversal or persisted retrieval traces is provided yet. Evaluate those changes using task-success and stale-memory baselines before adding infrastructure.

## Durable extraction

A source and all chunk jobs are inserted in one transaction. Idempotency is per original-content hash, explicit format selection, scope and parser version. Accepted jobs survive crashes; running jobs reset on a single-process restart. Candidate insertion and job completion are atomic, with source-scoped uniqueness for retries. Provider calls themselves are at-least-once across crashes and may be charged more than once.

The single worker processes up to 1,000 outstanding jobs with a 45-second provider limit and up to three attempts. A failed job can be retried in the UI. Invalid model output is a failed job, not a successful extraction of zero memories. Disk/storage errors during job-state recovery are logged and can require restart/operator intervention.

## Compatibility

This is a text-only custom service. The upstream wire format is OpenAI-style, but the public API is not a standards-compatible OpenAI proxy. Tool calls are explicitly rejected. A future compatibility adapter must preserve complete messages, tool-call IDs/results, cancellation, streaming completion markers, and provider-specific errors without fabricating success.
