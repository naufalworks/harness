# Harness repository audit — 2026-09-25

## Scope and boundaries

Audited the Rust Harness service and its checked-in SQL migrations, Python/Node test tooling, browser assets, Cargo/npm dependency graph, release scripts, and CI policies against deployed baseline `83e0a6d` (P20, schema 23). Work was performed in the isolated `audit/repo-cleanliness` worktree because the `main` checkout held pre-existing, uncommitted documentation, test-splitting, and test-contract edits. Those original working-tree files were not reset, discarded, or deployed. No production database was used as a test fixture.

## Implemented and verified

- Extracted embedded Rust test modules into dedicated test files for `main`, `storage`, `agent_loop`, `memory_agents`, and `storage/history`. The production module `main.rs` fell from 2,861 to approximately 300 lines, and `storage.rs` from 3,825 to approximately 540, without moving production behavior.
- Removed obsolete `dead_code` allowances on the P19 external-history auth boundary, which is live; marked test-only helpers as such; narrowed unused provenance re-exports.
- Added strict lint enforcement for `dbg!`, `todo!`, `unimplemented!`, and undocumented unsafe blocks, and supplied safety rationale for existing process, signal, lock, and browser FFI calls.
- Added `cargo-machete` to optional local supply-chain checks. The direct-dependency scan found **no unused direct dependencies**. Enforcing the same check in GitHub Actions requires workflow-file permission not available to the configured push token; the CI change is excluded from this release rather than claiming that it is enforced. No speculative Cargo dependency removal or version upgrade was made.
- Verification: `cargo test --locked -q` **366 passed, 0 failed**; strict all-target/all-feature Clippy passed with `-D warnings`; Rust format and `git diff --check` passed; Python unittest discovery **91 passed** with PATH configured; schema chain 001–023 and external-history contracts passed; documentation/route inventory passed. All 8 external-history integration tests and the mocked and Rust-backed Chromium E2E suites passed. Storage benchmark passes when rerun without competing compile/test tasks; its 1-second threshold failed under contention and should not be treated as an isolated performance regression.
- Cargo metadata resolved **209 packages**. `cargo-deny` reported `bans ok, licenses ok, sources ok` with metadata from the locked graph. npm audit previously reported zero vulnerabilities. `cargo tree -d` shows only transitive duplicate-version families; forcing them together without upstream compatibility evidence is not warranted.

## Unresolved gates and risks

- **RustSec vulnerability scan**: an upstream Git fetch failed (lock and connection reset), so an HTTPS snapshot of RustSec's public advisory repository was downloaded instead. `cargo-audit --db /tmp/advisory-db-main --no-fetch --deny warnings` loaded **1,269 advisories**, checked **209 locked dependencies**, and exited successfully with no vulnerability findings. It warned that the crates.io index was unavailable, so yanked-crate verification remains unconfirmed; do not claim that warning as a passed check.
- **Integration/deployment blocked by concurrent main-checkout edits**: do not replace or silently commit another actor's changes. Reconcile the main worktree, run the strict release gate on the exact resulting commit, then push/deploy and verify the binary, schema, backups, recall, and memory counts.
- Remaining maintainability opportunities for separate behavior-reviewed tasks: `src/api/routes.rs` (~2,148 lines) should be split by API domain; `static/app.js` (~104 KB) should be modularized with browser regression tests; `agent_loop::run` (~325 lines) and long storage/incident procedures need explicit state-machine or query-contract tests before extraction.
- Existing Python SQLite tests use numeric named placeholders with positional bindings, which emit Python 3.14 compatibility deprecation warnings; migrate only with parameter-binding contract coverage.
- Three roadmap tasks remain `todo` in the documentation ledger. Passing this code-maintenance audit is not proof those unrelated backlog capabilities are complete.

## Release policy

This audit must not be reported as fully released while the post-integration strict/deployment gates remain unverified. The deployed P20 release is not changed by the isolated audit worktree.
