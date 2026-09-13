-- 008: durable provider dispatch and usage ledger for fail-closed spend limits.
BEGIN IMMEDIATE;

CREATE TABLE provider_calls (
 call_id TEXT PRIMARY KEY,
 request_id TEXT,
 kind TEXT NOT NULL CHECK(length(kind) BETWEEN 1 AND 32),
 model TEXT NOT NULL CHECK(length(model) BETWEEN 1 AND 128),
 state TEXT NOT NULL CHECK(state IN ('reserved','complete','failed','refused')),
 prompt_tokens INTEGER CHECK(prompt_tokens IS NULL OR prompt_tokens >= 0),
 completion_tokens INTEGER CHECK(completion_tokens IS NULL OR completion_tokens >= 0),
 cost_microusd INTEGER CHECK(cost_microusd IS NULL OR cost_microusd >= 0),
 usage_status TEXT NOT NULL CHECK(usage_status IN ('pending','reported','unavailable')),
 reason TEXT,
 created_at TEXT NOT NULL,
 finished_at TEXT
);
CREATE INDEX provider_calls_request ON provider_calls(request_id, created_at);
CREATE INDEX provider_calls_day ON provider_calls(created_at, state);

PRAGMA user_version=8;
COMMIT;
