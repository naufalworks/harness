# Harness

Single-user, loopback-first Rust coding agent with durable SQLite receipts and reviewed memory. Every accepted message is recorded before the provider runs; bounded agentic turns use recorded, permission-gated project tools, publish redacted durable events, verify final claims against tool evidence, and queue memory extraction separately.

## Run

```sh
cp .env.example .env   # set HARNESS_AUTH_TOKEN (32+ ASCII chars) and provider creds
chmod 600 .env
cargo run --release    # listens on 127.0.0.1:8080
```

Open `http://127.0.0.1:8080`, paste the token. The token stays in tab memory
only; Lock/reload clears it. Drafts are never persisted to the browser.

### Exposing it on a tailnet

The service binds loopback only. To reach it from another device, front it with
`tailscale serve` rather than rebinding:

```sh
tailscale serve --bg --https=8443 http://127.0.0.1:8080
```

Then add that origin to `.env` before restarting, or every API call from the
browser is refused with `Origin not allowed`:

```sh
HARNESS_ALLOWED_ORIGINS=https://<machine>.<tailnet>.ts.net:8443
```

The allow-list exists because the token is sent as a bearer header from the
page; accepting arbitrary origins would let any site the browser visits spend
it. Loopback origins on the bound port are always permitted, so local use needs
no configuration. Note that a Funnel route on :443 is a *different* listener —
pointing a public domain at this service would place it on the open internet
behind nothing but the token.

## Layout

| Path | Purpose |
|---|---|
| `src/` | Rust service: API/auth, agent loop, tools, recording, memory, provenance, archive |
| `migrations/` | Append-only SQLite schema, currently migrations 001–007 |
| `static/` | Single-page UI served at `/` |
| `tests/` | Rust-adjacent contracts, live HTTP/fault suites, mocked and real browser E2E |
| `scripts/` | Local/release verification, encrypted backup/restore, deployment/rollback, imports |
| `docs/` | ARCHITECTURE, RECORDING_PROTOCOL, ROADMAP |
| `reference/renewed-ui-original/` | Historical UI reference; its theme has been ported to `static/` |

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
- **Recall.** Hybrid FTS5 plus deterministic local feature-hashing vectors over approved active memories in the conversation scope plus global; no embedding API call or model download.
- **Recorded coding tools.** Read/search, anchored edit/write, bash, plan/think, skill, read-only task sub-agent, Rust AST rewrite, LSP, and CDP browser calls are durable steps. Side effects follow per-scope permission modes.
- **Resumable evidence.** Activity and generation feeds use database cursors; final answers can be verified against a bounded evidence manifest and causal incident graph.
- **Operational recovery.** One process owns each database. SIGINT/SIGTERM drain claimed work within a bounded timeout; abrupt restart interrupts claimed generation without replay.

## Browser suites

Two browser fixtures check the frontend against a mocked API (they do not run the
Rust service): `tests/recording_ui.cjs` covers recording, the generation feed and
resume behaviour; `tests/ui_smoke.cjs` covers the wider UI surface. Both need a real
browser. The permissive developer gate may skip them explicitly; the strict release gate requires them.

```sh
scripts/setup_browser_tests.sh   # once per machine: Playwright + Chromium + system libs
scripts/verify_browser.sh        # run both fixtures
```

`setup_browser_tests.sh` needs `npm` (Debian/Ubuntu: `apt-get install -y --no-install-recommends npm`)
and root for Chromium's system libraries. `verify_browser.sh` resolves the browser from
Playwright itself, so no path is hard-coded; override with `CHROMIUM_PATH` if needed.
`verify_local.sh` reports the browser suites as skipped when `node_modules` is absent.
`verify_release.sh` fails closed unless browser dependencies exist, runs both mocked-browser suites,
and also runs the real browser-to-Rust-to-SQLite/filesystem E2E lane.


## Verify

```sh
bash scripts/verify_local.sh     # permissive developer check; skipped suites are explicit
bash scripts/verify_release.sh   # strict, non-deploying gate including mocked and real E2E browser lanes
```

HTTP suites use temporary DBs and a synthetic loopback provider — no paid
inference. The suites spawn real server processes, including SIGKILL/restart
recovery checks.

## Deploy

```sh
bash scripts/deploy.sh   # release build -> restart harness -> prove the live process is that build
```

Deployment is a separate promotion action, never part of either verification gate. `deploy.sh` rejects a dirty tree unless explicitly overridden, snapshots the currently served executable and identity, builds the release candidate, and verifies its commit, SHA-256, schema, worker readiness, API response, and JSON-object boundary. A failed candidate restores the previous executable only when the live schema is compatible. If the schema advanced or cannot be read, the service stops and database recovery requires explicit owner approval; the script never restores a database automatically. Override the unit with `HARNESS_UNIT`.

## Data

- DB at `HARNESS_DB` (default `data/harness_v2.db`), mode 0600, WAL.
  Plaintext sanitized content: keep it out of source control.
- One process per DB. Startup recovers interrupted generations and running extraction jobs; it never replays claimed provider/tool work.
- Graceful shutdown timeout: `HARNESS_SHUTDOWN_TIMEOUT_SECONDS` (default 30, range 1–300).
- Backups: `python3 scripts/backup.py create <db> <backup-dir> --key-file <owner-only-key>`
  creates a rotating AES-256-GCM archive from a verified online SQLite snapshot,
  including committed WAL state, and proves a clean restore before rotation. Generate
  the external key once with `python3 scripts/backup.py keygen <key-file>`; never store
  it beside the archives. During rotation, pass `--previous-key-file <old-key>` when creating, drilling, or restoring until old archives expire. The optional Python `cryptography` package is required.

## Legacy memory migration

Historical procedure retained for provenance and deliberate reruns into a new database. Its presence does not attest to current production state:

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
`POST /memory/confirm`. Pending tool approvals: `GET /permissions?scope=…`,
`POST /permissions/{id}` with `{"decision":"approve"|"deny"}` (idempotent; 409 if
already resolved the other way, 410 once the waiting turn gave up).
Turn record: `GET /chat/requests/{id}/steps` (previews ≤ 2 KB),
`GET /sessions/{id}/plan`, `GET /activity?session_id=…&after_seq=N` (≤ 200 events,
poll with the returned `next_after_seq`), `GET /changes?request_id=…` (each row
carries `revertable`, and a `revert_note` saying why when it is false).
Undo one recorded change: `POST /changes/{id}/revert`. It restores the content the
edit replaced only behind two proofs — the file must still hash to `after_hash`, and
the text rebuilt by reverse-applying the recorded diff must hash to `before_hash`.
Otherwise it answers 409 with the reason (already reverted, file changed since, no
project root) and writes nothing. Reverting a file the turn created deletes it again.
Live feed: `GET /activity/stream?session_id=…&after_seq=N` (`text/event-stream`,
`id: <seq>` per frame, `: heartbeat` every 15 s). Use `fetch` with the
`Authorization` header, not `EventSource`, and reconnect with the last `id` you
received. The database sequence plus the client cursor guard makes replay idempotent; the network transport itself is not claimed to be exactly-once.
Full behavior: `docs/RECORDING_PROTOCOL.md`.

## Remaining work

The active plan is `docs/PLAN.md`, sequencing and priority live in
`docs/ROADMAP.md`, executable work lives in `docs/TASKS.md`, and every state
change is journaled newest-first in `docs/PROGRESS.md`. Start at `AGENTS.md`.

P0–P10 are historical completed phases in the executable task ledger. The current baseline includes the recorded tool loop, permission gate, bounded context/compaction, hybrid reviewed memory, durable incremental generation, browser E2E, causal incident graphs, single-process ownership, readiness/build identity, encrypted backup drills, graceful shutdown, schema-aware rollback, and disposable fault injection. These are capability statements, not fresh release evidence; rerun the strict gate for a current claim.

P11–P18 are the active continuation covering performance, maintainability, remaining security/tool boundaries, daily workflow, memory governance, observability, the Memory Wind Tunnel, and optional platform evolution. Independent tasks may run in separate
worktrees only when their task metadata explicitly permits parallel execution.

Security posture: loopback bind enforced; current/previous bearer-token rotation; short-lived memory-only browser sessions; delayed authentication failures; Origin, body-size, per-route rate, session-cap, and session-recursion controls; opt-in trusted-proxy identity; conditional HSTS; conservative redaction before storage/provider; encrypted opt-in exact archive and backups with external keys. This remains a single-trusted-user service, not multi-tenant. Browser, command, and final-write policy hardening remains explicit backlog work.
