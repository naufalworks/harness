BEGIN IMMEDIATE;

-- P21-T02: provider metadata is safe to retain in SQLite; credentials are not.
-- Existing rows predate runtime provider selection and therefore belong to the
-- startup environment provider.
ALTER TABLE chat_receipts
    ADD COLUMN provider_id TEXT NOT NULL DEFAULT 'environment'
    CHECK(length(provider_id) BETWEEN 1 AND 64);
ALTER TABLE chat_receipts
    ADD COLUMN provider_version INTEGER NOT NULL DEFAULT 1
    CHECK(provider_version >= 1);

PRAGMA user_version=24;
COMMIT;
