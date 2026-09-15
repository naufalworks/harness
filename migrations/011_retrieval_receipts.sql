-- 011: durable retrieval explanations (P15-T02).
-- Additive only. One header row per turn plus one row per considered memory, so a receipt can
-- say what was retrieved, what was rejected, with which scores and for which reason. These rows
-- are evidence about retrieval only; they make no claim about how the model used the context.
BEGIN IMMEDIATE;

CREATE TABLE retrieval_receipts (
 request_id TEXT PRIMARY KEY REFERENCES chat_receipts(request_id) ON DELETE CASCADE,
 scope TEXT NOT NULL,
 strategy TEXT NOT NULL CHECK(strategy IN ('hybrid','lexical_only')),
 embedding_model TEXT NOT NULL,
 prompt_fingerprint TEXT NOT NULL,
 budget_bytes INTEGER NOT NULL CHECK(budget_bytes >= 0),
 included_bytes INTEGER NOT NULL CHECK(included_bytes >= 0),
 considered INTEGER NOT NULL CHECK(considered >= 0),
 included INTEGER NOT NULL CHECK(included >= 0),
 created_at TEXT NOT NULL
);

-- `decision` is what happened; `reason` is why. `category_budget` is decided by the context
-- builder after ranking, the other reasons are decided by recall itself.
CREATE TABLE retrieval_candidates (
 request_id TEXT NOT NULL REFERENCES retrieval_receipts(request_id) ON DELETE CASCADE,
 memory_id TEXT NOT NULL,
 scope TEXT NOT NULL,
 key TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 0),
 rank INTEGER NOT NULL CHECK(rank >= 0),
 decision TEXT NOT NULL CHECK(decision IN ('included','excluded')),
 reason TEXT NOT NULL CHECK(reason IN ('ranked_and_fit','rank_cutoff','payload_ceiling','category_budget')),
 lexical_score REAL NOT NULL DEFAULT 0,
 semantic_score REAL NOT NULL DEFAULT 0,
 scope_score REAL NOT NULL DEFAULT 0,
 recency_score REAL NOT NULL DEFAULT 0,
 usefulness_score REAL NOT NULL DEFAULT 0,
 total_score REAL NOT NULL,
 bytes INTEGER NOT NULL CHECK(bytes >= 0),
 PRIMARY KEY(request_id, memory_id)
);
CREATE INDEX retrieval_candidates_decision ON retrieval_candidates(request_id, decision, rank);

PRAGMA user_version=11;
COMMIT;
