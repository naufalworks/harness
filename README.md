# myharness

An OpenAI-compatible AI harness in Rust (axum) that proxies to the LongCat API and adds a persistent **SQLite memory layer**: every conversation is stored, relevant long-term memories are recalled into the prompt, and session history from external coding agents can be ingested and distilled into new memories.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| GET | `/models` | List available models (OpenAI-compatible) |
| POST | `/chat` | Chat completion + memory pipeline (see below) |
| GET | `/memory/status` | Memory/graph/artifact stats |
| POST | `/memory/confirm` | Resolve a Gatekeeper confirmation: `{"confirmation_id": "...", "confirm": true/false}` |
| POST | `/memory/ingest` | Ingest agent session files into memories (see below) |

## Chat memory pipeline

`POST /chat` with `{"prompt": "...", "session_id": "..."}` runs:

1. **Raw store** — the untouched prompt and agent response are logged to the artifacts hub.
2. **Recall** — a keyword prefilter (`search_memories`: OR'd `key LIKE`/`value LIKE` over up to 8 prompt words, top-confidence fallback) narrows 750+ stored memories to ~40 candidates; an LLM filter agent picks the 1–3 strictly relevant ones. Injected as `[Retrieved User Context & Preferences]`; response field `recalled_context_applied` reports whether this fired.
3. **Gatekeeper** — an LLM agent checks the exchange for durable facts (preferences, credentials, project decisions). If found, a confirmation is created and the response asks you to `confirm <id>` / `reject <id>`; confirmed items are upserted into `memories` (keyed, so re-confirming updates in place).
4. **Graph linker** (background) — extracts entity relations from the exchange into `graph_edges`.

Response includes `recalled_context_applied` and `background_status` (`graph_syncing_in_background` while the linker runs).

## Session ingestion

`POST /memory/ingest` reads chat history files written by external agents and runs each user→assistant exchange through the memory-extraction agent:

```json
{"path": "/absolute/path", "format": "omp"}
```

- `path` — a single file or a directory (walked recursively)
- `format` — `"omp" | "claude" | "codex" | "junie"`, or omit to auto-detect per file

Supported sources (on this machine):

| Format | Files |
|---|---|
| omp | `~/.omp/agent/sessions/**/<session>.jsonl` |
| Claude Code | `~/.claude/projects/**/<session>.jsonl` |
| Codex | `~/.codex/sessions/**/*.jsonl` |
| Junie | `~/.junie/sessions/*/transcript.md` (`## User` / `## Assistant` markdown sections) |

Response: `{"files_ingested", "exchanges", "memories_extracted", "errors"}`. Files with no extractable exchanges (e.g. a Junie session that never got an assistant reply) contribute nothing; unrecognized files are reported in `errors` and skipped.

## Storage

SQLite at `harness_memory.db` (override with `HARNESS_DB`), WAL mode. Tables: `memories` (key/value/category/confidence), `pending_confirmations` (Gatekeeper queue), `artifacts` (raw untruncated prompt/response/ingest log), `graph_edges` (entity relations).

## Configuration

| Env | Default | Purpose |
|---|---|---|
| `HARNESS_ADDR` | `0.0.0.0:8080` | Listen address |
| `HARNESS_BASE_URL` | `https://api.longcat.chat/openai` | Upstream OpenAI-compatible API |
| `HARNESS_API_KEY` | — (required) | Upstream API key |
| `HARNESS_MODEL` | `LongCat-2.0` | Default model |
| `HARNESS_DB` | `harness_memory.db` | SQLite path |

`.env` is auto-loaded via dotenvy.

## Run

```sh
cargo run --release
```
