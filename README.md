# Harness

Single-user, local Rust chat service with a durable memory layer. Every accepted
message is recorded before the model runs; memories are extracted, reviewed, and
recalled into future conversations via local FTS5 — one LLM call per chat.

## Run

```sh
cp .env.example .env   # set HARNESS_AUTH_TOKEN (32+ ASCII chars) and provider creds
chmod 600 .env
cargo run --release    # listens on 127.0.0.1:8080
```

Open `http://127.0.0.1:8080`, paste the token. The token stays in tab memory
only; Lock/reload clears it. Drafts are never persisted to the browser.

## Layout

| Path | Purpose |
|---|---|
| `src/` | Rust service: recording, storage, ingest, safety, memory agents |
| `migrations/` | Versioned SQLite schema (v1 core, v2 recording) |
| `static/` | Single-page UI served at `/` |
| `tests/` | SQL contracts, live HTTP suites, mocked-browser UI suites |
| `scripts/` | Online backup, session import CLI, legacy migration tools |
| `docs/` | ARCHITECTURE, RECORDING_PROTOCOL, ROADMAP |
| `reference/renewed-ui-original/` | Preferred UI source, pending merge |

## How it behaves

- **Recording before generation.** Sanitized user message + receipt commit
  before the provider call; a background worker owns generation, so a lost tab
  never cancels admitted work. Same request ID + content replays the receipt,
  never a second paid generation.
- **Answers survive extraction failures.** Memory extraction runs from a
  durable outbox; a full/failed queue never rolls back an answer.
- **Review-first memory.** Chat extractions and imports create pending
  candidates; nothing enters recall until approved. Rejection never deletes
  chat history — recording and memory are separate pipelines.
- **Recall.** Local FTS5 (BM25, no extra model call) over approved active
  memories in the conversation scope plus global.

## Verify

```sh
bash scripts/verify_release.sh   # cargo test/clippy/build, SQL contracts, both HTTP suites
CHROMIUM_PATH=… node tests/ui_smoke.cjs && CHROMIUM_PATH=… node tests/recording_ui.cjs
```

HTTP suites use temporary DBs and a synthetic loopback provider — no paid
inference. The suites spawn real server processes, including SIGKILL/restart
recovery checks.

## Data

- DB at `HARNESS_DB` (default `data/harness_v2.db`), mode 0600, WAL.
  Plaintext sanitized content: keep it out of source control.
- One process per DB. Startup recovers interrupted generations; never run two.
- Backups: `scripts/backup.py` performs a verified online SQLite backup
  (includes committed WAL). Never copy a live `.db` alone.

## Legacy memory migration

One-time, already applied to the live DB. For provenance/re-runs:

```sh
sqlite3 harness_memory.db ".backup 'backup.db'"   # or scripts/backup.py
python3 scripts/migrate_legacy_trusted.py harness_memory.db data/harness_v2.db
```

Only rows with a verified legacy confirmation became active memories; the rest
imported as pending candidates (bulk-approved after review); sensitive rows are
kept but never enter recall.

## API sketch

Prefer `POST /chat/submit` → 202 receipt → poll `GET /chat/requests/{id}`.
Context receipt: `GET /chat/requests/{id}/context`. History: `GET /sessions`,
`GET /sessions/{id}/messages`. Memory inbox: `GET /memory/candidates?scope=…`,
`POST /memory/confirm`. Full behavior: `docs/RECORDING_PROTOCOL.md`.

## Remaining work

The active plan is `docs/PLAN.md` (goals, principles, phases) with the ordered
backlog in `docs/TASKS.md` and the journal in `docs/PROGRESS.md`. Start at
`AGENTS.md` if you are an AI or a new contributor. `docs/ROADMAP.md` is the
older list and is kept for history; where they differ, PLAN.md wins.

Current direction: turn the single text-only provider call into a recorded,
sandboxed tool loop (read/grep/glob/edit/write/bash) with per-step receipts and
an approval gate, then a live activity UI, then context management and richer
memory. Nothing in P1+ is compiled yet; run `bash scripts/verify_release.sh`
first (P0-T01).

Security posture: loopback bind enforced, Bearer auth, Origin checks, no
server-side file paths, conservative secret filtering before storage/provider.
Single trusted user — not a multi-tenant service.
