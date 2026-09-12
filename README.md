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

## Browser suites

Two browser fixtures check the frontend against a mocked API (they do not run the
Rust service): `tests/recording_ui.cjs` covers recording, the generation feed and
resume behaviour; `tests/ui_smoke.cjs` covers the wider UI surface. Both need a real
browser, so they are kept out of `scripts/verify_release.sh`'s default path.

```sh
scripts/setup_browser_tests.sh   # once per machine: Playwright + Chromium + system libs
scripts/verify_browser.sh        # run both fixtures
```

`setup_browser_tests.sh` needs `npm` (Debian/Ubuntu: `apt-get install -y --no-install-recommends npm`)
and root for Chromium's system libraries. `verify_browser.sh` resolves the browser from
Playwright itself, so no path is hard-coded; override with `CHROMIUM_PATH` if needed.
`verify_release.sh` runs the browser suites automatically when `node_modules` is present
and skips them with a notice when it is not.


## Verify

```sh
bash scripts/verify_release.sh   # cargo test/clippy/build, SQL contracts, both HTTP suites
CHROMIUM_PATH=… node tests/ui_smoke.cjs && CHROMIUM_PATH=… node tests/recording_ui.cjs
```

HTTP suites use temporary DBs and a synthetic loopback provider — no paid
inference. The suites spawn real server processes, including SIGKILL/restart
recovery checks.

## Deploy

```sh
bash scripts/deploy.sh   # release build -> restart harness -> prove the live process is that build
```

The unit starts `target/release/harness`, while the gate above builds and tests the
debug profile, so `systemctl restart` on its own can relaunch a binary older than the
change being deployed. `deploy.sh` builds the release profile, restarts the unit,
compares the md5 of `/proc/<pid>/exe` with the binary it just built, and smoke-tests
that the API answers and that a non-object body is refused before reporting success.
Override the unit name with `HARNESS_UNIT`.

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
received; delivery is exactly-once because the cursor is the database sequence.
Full behavior: `docs/RECORDING_PROTOCOL.md`.

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
