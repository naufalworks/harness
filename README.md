# Harness 0.2 — implementation candidate

A single-user, local Rust chat proxy with reviewable memory. This version separates conversation capture, provider generation, queued extraction, and explicit memory approval.

**Release status:** Rust compilation and live HTTP integration have NOT been run in the delivery environment. It had no Rust toolchain and could not download one. Offline SQLite tests and mocked-browser checks are documented in `docs/VALIDATION.md`. Run the release gates below before relying on this build. This is source code, not an installed/deployed service.

## What changed

- Loopback-only binding; Bearer authentication for every data/API route; browser Origin checks; conservative concurrency and payload limits.
- Best-effort secret redaction before normal storage/provider calls; credential-category memories are rejected. This is not a complete secret detector or encrypted vault.
- Fresh, versioned SQLite schema with foreign keys, WAL and `synchronous=FULL`.
- Blocking SQLite work isolated with `spawn_blocking`, a bounded semaphore, and short transactions.
- Completed conversation turns are replayed within an explicit byte budget.
- SQLite FTS5 recall returns approved, scoped, evidence-linked records. No recall LLM call.
- Every extraction result enters a candidate inbox; imported history never directly updates active memory.
- Atomic approval/rejection, expected-revision conflict checks, expiration, and immutable revision entries.
- Durable extraction jobs, idempotent source/chunk identities, capped retry with manual retry after failure.
- User/assistant messages are not reduced to pairs. All normalized text is chunked at UTF-8 boundaries, without a first-40-message cutoff.
- Sanitized source text is retained alongside normalized job payloads. Unsupported source records are reported, not represented as successfully normalized.
- Source upload replaces arbitrary server-side path ingestion.
- Browser inbox shows old/proposed values and user evidence; structured confirmations replace prose parsing.
- Backup and legacy migration scripts never overwrite their destination.

## Validation / release gates

Install stable Rust and Python 3 on your own machine, then:

```sh
cargo fmt --all
cargo test --locked
cargo clippy --locked --all-targets
cargo build --locked
python3 -m unittest discover -s tests -p 'test_*.py' -v
python3 tests/integration_smoke.py
```

The integration test uses a local mock provider and temporary database; it makes no paid external requests. GitHub Actions runs these compile/test checks (except formatting, which should be applied locally).

The existing lockfile package graph was retained; `ring` and `tower` were already present transitively and were added to the root dependency list. Cargo has not regenerated/verified this lockfile here. If Cargo reports that the lockfile needs an update, run `cargo generate-lockfile`, inspect and commit the changes, then rerun all gates. Do not bypass a failed gate and treat the package as validated.

Optional browser checks:

```sh
npm install --no-save playwright
CHROMIUM_PATH=/path/to/chromium node tests/ui_smoke.cjs
```

## Start a clean local instance

```sh
cp .env.example .env
# Edit .env locally; do not paste secrets into issue trackers or chats.
# Generate a local token with:
python3 -c 'import secrets; print(secrets.token_urlsafe(32))'
# Put it in HARNESS_AUTH_TOKEN, set HARNESS_API_KEY, then:
chmod 600 .env
umask 077
cargo run --release
```

Open `http://127.0.0.1:8080`. Enter the local Bearer token into the UI. It is held only in tab memory, not localStorage. Conversation identity and scope are kept in sessionStorage. Reloading requires re-entering the token.

This release requires a single process and a single trusted user. Project scopes are retrieval partitions, not multi-tenant authorization boundaries. Do not share the token, put the database on a network filesystem, or start multiple application instances on the same DB. Remote/team deployments require a separate authentication and process coordination design.

## Configuration

| Environment variable | Default / requirement |
|---|---|
| `HARNESS_ADDR` | `127.0.0.1:8080`; must be loopback |
| `HARNESS_AUTH_TOKEN` | Required: 32–256 non-whitespace ASCII characters, randomly generated |
| `HARNESS_API_KEY` | Required upstream provider credential |
| `HARNESS_BASE_URL` | `https://api.longcat.chat/openai`; HTTPS, or HTTP only on loopback |
| `HARNESS_MODEL` | `LongCat-2.0`; verify availability with your provider |
| `HARNESS_DB` | `data/harness_v2.db`; fresh v2 database, not the original legacy DB |

No provider model availability has been verified. Configure model names your provider actually supports. Main and extraction roles can be overridden in the Models tab.

## Data flow

```text
Authenticated chat
  → sanitize → persist pending user message
  → load recent completed turns + scoped FTS5 recall
  → text-only provider completion
  → transaction: complete turn + enqueue extraction
  → return answer

Worker
  → claim persisted job → extract grounded proposals
  → validate category/length/secret policy + exact user quote
  → transaction: candidates + job completed

Human review
  → scope + expiration + expected revision checked
  → transaction: active memory + revision + proposal resolution
```

Only user messages are eligible evidence. An exact quote check verifies that the quote exists, not that the model's interpretation is correct. Human review remains necessary. Stored context is reference data, not permission to run commands.

Completed turns are bounded to the latest 20 messages and at most 24,000 content bytes (oldest complete pairs removed). Current prompts have a 16,000-byte limit. FTS queries use at most 24 unique alphanumeric tokens, including short technical terms. Recall is limited to six approved records / 6,000 serialized bytes. These are byte budgets, not model-specific token guarantees; adapt after measurement.

## Import transcripts

The browser accepts one UTF-8 transcript up to 1 MiB. For local directories:

```sh
# Export HARNESS_AUTH_TOKEN in your shell first; scripts do not source .env.
python3 scripts/import_sessions.py /path/to/sessions \
  --scope project-example --format claude --send-to-provider
```

Sources: `claude`, `codex`, `omp`, `junie`, or automatic detection. The CLI skips symlinks, limits depth/files, uploads one bounded file at a time, and never exposes server filesystem reads.

The acknowledgement flag means sanitized user messages may be sent to your configured provider. Filter/redact sources before import. Automated redaction is heuristic and may both miss unknown secret formats and redact legitimate technical lines.

Content hash + scope + parser version prevent repeating an unchanged source import. Changing the explicit format selection can produce a different identity. Modified/appended files are new source snapshots; incremental tail offsets and cross-snapshot event deduplication remain backlog work.

All chunks are saved as queued jobs in one transaction; the source is not considered accepted if that transaction fails. There is a 1,000 outstanding-job cap. Each worker call is bounded to 45 seconds; failed extraction/validation retries up to three attempts, then requires explicit retry from the jobs UI.

## Existing database: do not overwrite it

1. Keep the original database and any WAL/SHM companions together.
2. Make a verified backup:

```sh
python3 scripts/backup.py /path/to/harness_memory.db /safe/path/legacy-backup.db
```

3. Convert non-sensitive active legacy records into **pending candidates in a new DB**:

```sh
python3 scripts/migrate_legacy.py /safe/path/legacy-backup.db \
  /safe/path/harness_v2.db --scope legacy-review
```

4. Point `HARNESS_DB` to the new path. In the UI, start a conversation with scope `legacy-review`, then inspect the Memory inbox.

Legacy values have unknown provenance. Confidence `0.8`/`1.0` is not converted into approval. Credential/sensitive-looking, invalid and inactive records are excluded and counted. Legacy artifacts, settings and graph edges are NOT migrated into the new active service; they remain in the original backup. No private database is included in this source package.

## API (all require Bearer auth)

| Endpoint | Purpose |
|---|---|
| `POST /chat` | `{prompt, scope?, session_id?, request_id?, model?}` |
| `GET /sessions/{id}/messages` | Last 100 captured messages, including pending/failed states |
| `GET /memory/status` | Counts and job health |
| `GET /memory/candidates?scope=global` | Up to 100 pending candidates with evidence and prior value |
| `POST /memory/confirm` | `{confirmation_id, scope, confirm}`; no text-command interception |
| `POST /memory/ingest` | `{name, content, format?, scope?}`; no `path` field |
| `GET /jobs` | Last 100 jobs |
| `POST /jobs/{id}/retry` | Retry a failed job |
| `GET /models` | Provider model listing |
| `GET/POST /config` | Model role overrides: `main`, `extraction` |

This remains a custom text-only chat API, NOT a drop-in OpenAI-compatible server. Streaming and tool calls are intentionally not implemented. Unexpected tool-call responses fail explicitly rather than becoming empty successful answers.

## Limits and next steps

See `docs/PLAN.md`, `docs/VALIDATION.md` and `docs/ARCHITECTURE.md`. Not yet provided: a guaranteed lossless/encrypted raw archive, secret vault, complete deletion/retention workflow, scoped migration editing UI, candidate conflict rebasing UI, retrieval-trace persistence, episode/archive search, semantic embeddings, graph recall, full OpenAI streaming/tool semantics, multi-user permissions, automatic credential rotation, or a live deployment.
