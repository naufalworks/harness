# Harness · Recording receipts checkpoint

**A focused source implementation—not the completed two-branch merge.**

This session improves one thing: keeping a trustworthy record of an accepted chat even when memory processing fails or the browser disconnects. The small UI addition exposes recording status and an expandable context receipt. The preferred renewed UI is preserved separately, not yet fully merged.

**Start with `NEXT_SESSION_START_HERE.md`.** It contains user priorities, precise completion status and the continuation path. See `docs/ROADMAP.md` for everything carried forward.

## Implemented in source
- Durable sanitized-message admission before model calls; a background generation worker survives HTTP disconnects.
- Recording receipts and explicit complete/failed/interrupted states; same-ID replay does not regenerate an answer.
- Exact prepared provider-message array + recalled memory revision snapshots saved before dispatch, inspectable later.
- Completed answer storage independent of extraction queue capacity. Deferred outbox work resumes when capacity returns.
- Server-side session reopening and paginated message history; no browser-local transcript storage.
- Small responsive recording UI: quiet status, optional receipt, reconnect check and leave-for-later action.

**Not included yet:** encrypted exact originals/general artifact vault, streaming/partial output capture, trust-aware memory migration, balanced inline approval, archive search, full renewed UI merge or deployment. Sanitized SQLite storage is not a lossless encrypted archive. See the boundaries in `docs/RECORDING_PROTOCOL.md`.

## Validation status
41 SQL contracts and 28 named mocked-browser checks passed here. **Rust compilation, 21 native tests and both actual-server HTTP suites could not run** because this environment had no compiler or downloadable toolchain. Treat this as a source candidate until the native gate passes.

```sh
bash scripts/verify_release.sh
# Optional browser regression suites (require Playwright + Chromium):
CHROMIUM_PATH=/path/to/chromium node tests/ui_smoke.cjs
CHROMIUM_PATH=/path/to/chromium node tests/recording_ui.cjs
```

The HTTP suites use temporary DBs and a synthetic loopback provider; no paid inference. No private DB, provider credentials or binary is included.

## Try it only after the release gate passes
```sh
cp .env.example .env
# Configure the provider and a strong HARNESS_AUTH_TOKEN locally.
# Use a FRESH path in HARNESS_DB, not your only copy of an existing DB.
chmod 600 .env
umask 077
cargo run --release
```
Open `http://127.0.0.1:8080`. Enter the local token; it stays in tab memory only. Lock/reload clears the token and any unsaved draft. Receipt/session identifiers remain in sessionStorage for reconnect; server History discovers saved sessions in a new tab.

Configuration, supported transcript formats and inherited security controls are documented in `docs/previous-delivery/README.md`, but its old chat transaction/API/release-state descriptions are superseded by this README and `docs/RECORDING_PROTOCOL.md`. Keep using loopback, one trusted user, one process and a local filesystem.

## Database upgrade and rollback
- New DB: creates schema v1 then applies additive `002_recording.sql` to v2.
- Existing hardened schema v1: upgraded automatically on startup. Old messages remain; historical context receipts are not invented.
- Legacy original UI/backend DB: **still refused**. The inherited quarantine importer does not solve trust-aware migration. Do not use it for a final user migration yet.
- Before trying a copy: stop the service and make a verified SQLite backup (`scripts/backup.py`). Keep the original and any WAL companions safe. An older binary will reject schema v2; rollback means restoring a protected pre-upgrade backup to a separate path—not deleting new tables or overwriting the only copy. Later checkpoint chats are not in that old backup.

## API notes
Prefer `POST /chat/submit` → 202 receipt → authenticated `GET /chat/requests/{id}`. See the protocol document for full state/response behavior. `POST /chat` now waits up to approximately five seconds, then returns 202 if needed: clients must support polling. Same request ID replays state; no automatic repeated paid generation on restart.

## Package map
- `src/`, `migrations/`, `static/`: focused implementation.
- `tests/`, `docs/qa/`: test sources, logs and synthetic visual evidence.
- `NEXT_SESSION_START_HERE.md`, `docs/ROADMAP.md`: complete continuation instructions.
- `reference/renewed-ui-original/`: untouched preferred UI branch source.
- `reference/source-archives/`: original supplied ZIPs preserved byte-for-byte in the downloadable package.
- `docs/changes.patch`: focused changes against the hardened backend baseline (plus added files); retained original archives are the definitive reconstruction source.
