# Validation record — 2026-09-08

## Release verdict

**Implementation candidate, NOT a compiled or production-validated release.**

The delivery environment had no `cargo`, `rustc`, or `rustup`. A bounded network probe could not resolve the Rust distribution host, so a compiler could not be installed. No actual provider key was used and no paid inference request was made.

## Executed successfully

- **16 SQLite / migration contract tests.** Tests execute the shipped migration and extracted mutation/recall SQL. A Python transaction driver mirrors the Rust approval orchestration. This verifies SQL constraints and transaction behavior, not Rust execution.
- **14 mocked-browser checks.** Connect, token non-persistence, conversation rendering, memory evidence, hostile markup rendered as text, recoverable failed approval, approval, import, job retry, model settings, mobile overflow, dark mode, lock, and no JavaScript exceptions.
- `node --check static/app.js`.
- Python syntax compilation of scripts and integration-test source.
- `git diff --check`.
- SQLite online backup and source-preserving migration using synthetic fixtures.
- Isolated migration dry run against a WAL-aware copy of the supplied legacy database: **697 pending candidates; 58 sensitive-looking/credential records skipped; zero active memories**. The new copy passed integrity checking. Neither it nor any original database is included in this package. The filter is heuristic; skipped counts are not a count of verified live secrets.

Test logs and browser result metadata are included in `docs/qa/`. Screenshots use invented fixture data, not the user's conversation or database values.

## Included, NOT executed here

- **13 native Rust tests** for authentication/Origin behavior, storage approval/idempotency, redaction, lexical normalization, Unicode chunking, consecutive messages, source JSON preservation, and tool-result evidence isolation.
- `tests/integration_smoke.py`, which starts the compiled service plus a local synthetic provider and checks real HTTP authentication, approvals, scoped recall, multi-turn message forwarding, import deduplication, path rejection and redaction.
- GitHub Actions workflow with cargo test, clippy, build, SQL tests, integration smoke and JavaScript syntax check.

## Must pass before use

```sh
cargo fmt --all
cargo test --locked
cargo clippy --locked --all-targets
cargo build --locked
python3 -m unittest discover -s tests -p 'test_*.py' -v
python3 tests/integration_smoke.py
```

If dependency resolution requires a lockfile update, regenerate and review it, then rerun the complete gate. The lockfile root dependency entry was updated against already-present packages but has not been validated by Cargo in this environment.

## Not claimed

No benchmark of retrieval quality, provider compatibility, attack resistance, production throughput, cancellation recovery, or complete secret detection was performed. No Rust binary was produced. No original database was modified, no credential was rotated, and no service was deployed.

The source implements explicit controls; a passing SQL or mocked-UI test cannot substitute for validating those controls in the compiled service. Keep native/integration tasks open until they actually pass.
