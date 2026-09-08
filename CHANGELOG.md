# Changes from the supplied prototype

## 0.2.0 implementation candidate

### Intentional breaking changes

- API calls require a separate local Bearer token.
- Listen address must be loopback; default changes from 0.0.0.0 to 127.0.0.1.
- Default DB is `data/harness_v2.db`. Legacy DBs are rejected rather than automatically mutated.
- `POST /memory/ingest` accepts transcript content, not a server-side path. Use the browser or local CLI.
- Imports and chat extraction create pending candidates; neither directly updates active memory.
- Confirmation uses the structured endpoint and scope. Text prompts starting with confirm/reject are ordinary chat messages.
- Model roles are `main` and `extraction`; local FTS5 replaces the recall LLM and queued extraction replaces the synchronous gatekeeper.
- Chat returns a queued job status and source-linked recalled records. The UI checks the persistent candidate inbox separately.
- The service remains a custom text-only API. Streaming and tool calls are not claimed.

### Preservation and recovery

- Original source/archive/database were not overwritten.
- Backup includes committed WAL state and checks integrity.
- Legacy migration creates unapproved, separately scoped candidates in a new DB and excludes sensitive-looking records.
- Legacy artifacts, settings and graph edges remain in the original backup, not the active v2 store.
- Sanitized sources and all normalized chunks are retained; exact encrypted raw archival is intentionally deferred.

### Validation

See `docs/VALIDATION.md`. SQL/migration checks and mocked-browser tests passed. Native Rust build, parser/API tests and compiled-service integration checks remain mandatory release gates.
