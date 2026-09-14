# HANDOFF — P14-T01 durable cancellation and safe-boundary retry

Authoritative remote handoff. No secrets, tokens, or credentials are recorded here.

## CHECKPOINT TERBARU — DISIMPAN UNTUK DILANJUTKAN

Waktu simpan menurut host repo: `2026-09-13T15:46:39Z`. Blok ini menggantikan status sesi sebelumnya di bawah.

### Status dan batas pekerjaan

- **P14-T01 tetap `needs-verify`, belum selesai dan belum disetujui untuk rilis.**
- User meminta hanya menyimpan progress di repo dan akan melanjutkan nanti. Development/review kode dihentikan sementara; pembaruan ini hanya dokumentasi.
- Repo: `/root/development/harness-p14-cancellation`; branch: `p14-durable-cancellation`.
- HEAD: `9947c0b7798c9d7d42b791b1b29df080043f137f`.
- Worktree berisi perubahan yang belum di-commit: 16 modified + 3 untracked, tanpa staging. Untracked: `docs/HANDOFF-P14-T01.md`, `migrations/009_run_cancellation.sql`, `src/processes.rs`.
- **Tidak ada commit, push, deployment, atau restart produksi.** Checkpoint tersimpan di worktree remote, bukan commit/GitHub. Jangan reset, clean, atau menghapus worktree ini sebelum perubahan diamankan.

### Perubahan implementasi terakhir

- **P14-REVIEW-01:** `src/agent_loop.rs` kini me-race child-provider wait dengan durable cancellation, menutup model step yang dibatalkan sebagai interrupted, dan memeriksa cancellation kembali setelah respons.
- Regresi: `agent_loop::tests::cancelling_a_held_sub_agent_provider_call_interrupts_without_a_further_call`.
- **P14-REVIEW-02:** `src/processes.rs` menambahkan seal pembatalan sebelum drain; registrasi process group yang terlambat disinyalkan saat registrasi.
- Regresi: `processes::tests::a_group_spawned_after_cancel_is_killed_when_it_registers`.
- Implementer melaporkan kedua regresi fail-before/pass-after. Perubahan source sesi kelanjutan hanya kedua file di atas; delta P14 sebelumnya tetap dipertahankan.
- Seal proses bersifat in-memory dan bounded (`MAX_SEALED=4096`). Hubungannya dengan durable intent, eviction, serta lifetime/PID-reuse masih harus ditinjau independen; ini bukan persetujuan keamanan implementasi.

### Bukti pengujian terakhir

Berikut hasil dari tool report implementer pada sesi kelanjutan, **bukan pengujian ulang pada pembaruan dokumentasi ini dan bukan audit independen**. Angka 201 pada catatan lama adalah hasil sebelum dua regresi baru; hasil terakhir yang dilaporkan adalah 203.

| Pengujian | Hasil yang dilaporkan implementer | MCP task ID | Log remote |
| --- | --- | --- | --- |
| `cargo check --locked --all-targets` | exit 0 | `26a2c7d4106c` | `/tmp/p14-continuation-check1.log` |
| `cargo test --locked` | exit 0; 203 passed, 0 failed | `cccf3ed2e378` | `/tmp/p14-continuation-cargotest.log` |
| `cargo test --locked processes::` | exit 0; 4 passed | `1ce5cc6a3b9f` | `/tmp/p14-continuation-proc.log` |
| Regresi held child-provider | exit 0; 1 passed | `aebfdb2d1097` | `/tmp/p14-continuation-sub2.log` |
| Fail-before kedua regresi | keduanya FAILED; exit 101 | `eeff850f1d01` | `/tmp/p14-continuation-failbefore.log` |
| `bash scripts/verify_e2e.sh` | exit 0 | `9e0c9b9a9723` | `/tmp/p14-continuation-e2e.log` |
| `bash scripts/verify_release.sh` | exit 0; strict non-deploy gate PASS | `992b740ddaa6` | `/tmp/p14-continuation-release.log` |

### Mengapa masih needs-verify

- Review independen terhadap perubahan kode terbaru **belum dijalankan**. Reviewer tidak boleh dianggap telah menyetujui kode.
- Persiapan snapshot lokal melalui MCP mengalami timeout, lalu pemeriksaan kontinuitas baris gagal. Ada penyesuaian perhitungan newline pada helper lokal, tetapi belum ada snapshot lengkap yang berhasil diverifikasi hash maupun review yang selesai.
- Jangan gunakan snapshot parsial atau metadata `end_line/next_offset` saja sebagai bukti file lengkap. Ambil chunk kecil, hitung baris termasuk baris kosong, dan cocokkan SHA-256 dengan file remote.
- Riwayat penolakan packet verifier dari sesi terdahulu tetap historis; itu bukan review atau verdict kode terbaru.

### Titik mulai sesi berikutnya

1. Baca checkpoint ini, `docs/TASKS.md` P14-T01, lalu periksa HEAD/status/diff remote untuk mendeteksi perubahan sejak checkpoint. Pertahankan seluruh modified/untracked files.
2. Siapkan artefak lengkap yang bisa diakses verifier (hash/kelengkapan terverifikasi), termasuk semua delta cancellation/retry, migration 009, `src/processes.rs`, UI, dan tes; jangan hanya meninjau dua fix.
3. Audit independen provider/sub-agent cancellation, permission waits, restart/finalization, late process registration/seal/lifetime, dan safe-boundary retry tanpa replay side effect. Gunakan schema verifier yang berlaku saat itu; deklarasikan path artefak aktual dan issue/claim IDs untuk follow-up.
4. Jika ditemukan defect atau drift, perbaiki dan jalankan regresi terkait, `bash scripts/verify_e2e.sh`, serta `bash scripts/verify_release.sh` setelah memeriksa script. Verifikasi tetap non-deploy.
5. Perbarui status berdasarkan bukti. Konfirmasikan scope commit/push sebelum publikasi ke target sebelumnya `origin p14-durable-cancellation`; checkpoint ini tidak mengizinkan deployment produksi.

Kredensial MCP tidak disimpan di dokumentasi. Gunakan akses yang valid pada sesi berikutnya; token yang pernah dibagikan dalam chat sebaiknya dirotasi.

---

## ARSIP SESI SEBELUMNYA — BUKAN STATUS TERBARU

Seluruh catatan di bawah dipertahankan sebagai riwayat. Status, bukti terakhir, dan langkah lanjut yang berlaku adalah CHECKPOINT TERBARU di atas; pernyataan complete/pending, jumlah tes, waktu, dan task board lama tidak menggantikan checkpoint tersebut.

## STATUS PADA SESI SEBELUMNYA

- **Implementation: complete. Automated gates: all passed.**
- **P14-T01 status: `needs-verify` — NOT done** (`docs/TASKS.md`, P14-T01 entry).
- **Independent review: PENDING — NOT RUN.** The verification sub-agent was rejected twice by the
  orchestration layer: the first packet was missing `<verification_packet>`, and the corrected body
  was rejected as *not valid JSON*; the tool then returned
  `verification_packet_validation_failed_twice` and forbade further verifier attempts for that
  request. This is an orchestration packet-validation failure, not a code failure.
- **Push withheld.** No commit, no push. Branch `p14-durable-cancellation` has no upstream;
  HEAD `9947c0b7798c9d7d42b791b1b29df080043f137f`.
- **Live task board** (`CID-41770868U1789304-798A2B-5423-AB64EE`): task1 (context/handoff) completed;
  task2 (implementation) completed; task3 (verification) pending — awaiting independent review;
  task4 (finalize/push) pending. (task3 was `in_progress`; it is being held, not running.)
- **No production deployment.**

## Current remaining work

1. **Independent review (pending; may find additional defects).** Independently audit ALL
   cancellation/retry changes. Invoke a verification sub-agent with **exactly one
   `verification_packet` whose body is valid JSON**. Treat the automated gates below as
   necessary-but-not-sufficient.
2. **If review finds a defect or any drift:** fix it, then re-run the affected suites and the strict
   gate `scripts/verify_release.sh`.
3. **Only then** commit and push to `origin p14-durable-cancellation` (no upstream yet). No
   production deployment.

## Repo / worktree

- Repo root: `/root/development/harness-p14-cancellation`; branch `p14-durable-cancellation`.
- HEAD `9947c0b7798c9d7d42b791b1b29df080043f137f` ("Harden tool destination and write policies").
- Upstream: none configured. Push target (read-only observation):
  `origin git@github.com:naufalworks/harness.git` (fetch+push).
- Working tree uncommitted; `git diff --check` clean.

## Task scope (P14-T01 only — not T02/T03)

From `docs/TASKS.md`:
> Add durable cancellation and safe-boundary retry. **done-when:** users can cancel a generation, a
> permission wait, sub-agents and process groups, then retry only from a recorded non-mutating
> boundary without replaying side effects. **verify:** `scripts/verify_e2e.sh`. **files:**
> `migrations/*`, `recording.rs`, `agent_loop.rs`, `main.rs`, `static/app.js` plus adjacent
> tests/tools.

Constraints honored: recording-first; no automatic side-effect or provider replay; append-only
migrations; narrow reversible changes; truthful tests. The development migration
`009_run_cancellation.sql` is untracked and was not edited for cleanup.

## Delivered (final implementation)

- **Durable intent/lineage.** Schema v9 `run_controls(request_id PK, cancel_requested_at,
  cancelled_at, safe_boundary_seq, retry_of, retried_by)` in `migrations/009_run_cancellation.sql`
  (untracked). `DbStore::{request_cancellation, cancellation_requested, finalize_cancellation,
  retry_recording}` with `RetryAdmission::{Saved,Unsafe,NotTerminal,Busy,NotFound}`.
- **Endpoints.** `POST /chat/requests/{id}/cancel` (202 generating, 200 already-cancelled, 409
  otherwise) and `POST /chat/requests/{id}/retry` (202 admitted, 404/409 refusals).
- **Mid-flight provider cancellation.** `agent_loop` races every provider wait (streaming and
  tool-calling) against the durable cancel intent (`Ctx::race_cancellation`, 200 ms
  `PROVIDER_CANCEL_POLL`); the abandoned future is dropped, aborting the in-flight request. A
  cancelled model-call step is closed as `interrupted` (`finish_cancelled_step`), so no `running`
  step survives a cancel.
- **Sub-agent cancellation.** `run_task` checks the cancel intent between provider calls and between
  sub-agent tool calls, records `subagent::Stop::Cancelled`, and finishes the delegation and its
  parent step as `interrupted`.
- **Permission-wait cancellation.** `await_permission` checks the cancel intent on every poll.
- **Process groups.** `src/processes.rs` live registry; `run_capped_for` registers the foreground
  `bash`, edit-diagnostics and LSP command group per request; the cancel endpoint commits durable
  intent and then terminates the live group.
- **Safe-boundary retry, refusing unsafe retry.** Refuses if any side-effecting/uncertain tool step
  or an applied file change exists; admits otherwise; idempotent replay; 409 reasons surfaced.
- **UI.** Composer **Stop** (available while the turn runs), per-turn **Retry from safe boundary** on
  terminal `failed`/`interrupted` turns, 409 refusal reasons surfaced; CSP-safe DOM construction.
- **Tests.** Five Rust retry regressions plus `processes`/`subagent` unit tests; migration v9
  constraints; `tests/recording_ui.cjs` mocked Stop/Retry; `tests/browser_e2e_failure.cjs`
  real-server `cancellationThenSafeRetry` and `unsafeRetryIsRejected`.

## Verification — authoritative results (true exit codes)

Clarification on `verify_local`: a **standalone** `scripts/verify_local.sh` run first FAILED at
`mocked-browser` (browser dependencies were not yet provisioned). There is **no standalone
full-local success**. The full local-contract pass is evidenced only through the strict release
gate, which runs `env HARNESS_REQUIRE_BROWSER=1 scripts/verify_local.sh` and then the real e2e gate.

- `scripts/setup_browser_tests.sh` -> **exit 0** (Playwright Chromium provisioned).
- `scripts/verify_release.sh` (strict, non-deploying) -> **exit 0**:
  - `local-contract` `[PASS]`: rust-tests **201 passed**, clippy, build, python-contracts,
    integration-smoke, recording-integration, javascript-syntax, mocked-browser.
  - `real-browser-to-server` `[PASS]`: E2E, DENIAL, CRASH-RECOVERY, CANCELLATION, UNSAFE-RETRY.
- `scripts/verify_e2e.sh` -> **exit 0** after a fix (the first run exited 1 and found a leaked
  `running` model-call step, which was fixed).
- `cargo check --locked --all-targets` -> 0; `python3 tests/test_migrations.py` -> 0
  (`user_version=9`); `git diff --check` clean; `node --check` clean.

## Evidence ledger — commands, true exit codes, MCP task IDs, logs

MCP shared task board: `CID-41770868U1789304-798A2B-5423-AB64EE`. Remote MCP tool: `run_command`
(shell executor); each task id is the remote background run; logs live on the remote host under `/tmp`.

| Command (remote cwd: repo root) | MCP task id | Log | True exit |
| --- | --- | --- | --- |
| `bash scripts/setup_browser_tests.sh` | `9d882be49907` | `/tmp/p14_setup.log` | **0** |
| `bash scripts/verify_local.sh` (standalone, first) | `6f0d5713b909` | `/tmp/p14_vl3.log` | **1** (mocked-browser; deps not yet provisioned) |
| `bash scripts/verify_browser.sh` (rerun) | inline | `/tmp/p14_vb2.log` | **0** |
| `bash scripts/verify_e2e.sh` (first) | `851ceaba8d9f` | `/tmp/p14_e2e.log` | **1** (found leaked `running` model-call step) |
| `bash scripts/verify_e2e.sh` (after fix) | `8e1ce0ae5adb` | `/tmp/p14_e2e2.log` | **0** |
| `bash scripts/verify_release.sh` (strict, non-deploying) | `470cb50e9cd0` | `/tmp/p14_release.log` | **0** |
| `cargo check --locked --all-targets` | inline | `/tmp/p14_chk4.log` | **0** |
| `python3 tests/test_migrations.py` | inline | stdout | **0** (`user_version=9`) |
| `git diff --check` | inline | stdout | clean (`DIFFCHECK_OK`) |
| `node --check static/app.js` + all `tests/*.cjs` | inline | stdout | **0** |

Strict release gate final line (`/tmp/p14_release.log`):
`[PASS] strict release gate: native, contracts, HTTP, mocked-browser, and real browser-to-server
suites all passed.` -> `EXIT:0`.

Real e2e scenario lines (`/tmp/p14_e2e2.log`): `E2E passed`, `DENIAL E2E passed`,
`CRASH-RECOVERY E2E passed`, `CANCELLATION E2E passed: Stop during a slow provider wait, then exactly
one safe-boundary retry with no replayed side effect`, `UNSAFE-RETRY E2E passed: a completed side
effect makes retry a hard refusal, with no retry turn created`.

## Next-session action

Independently audit **all** cancellation/retry changes (backend provider race, sub-agent and
permission-wait cancellation, process-group termination, retry admission/refusal, and the
`static/app.js` + `static/index.html` UI) using a verification sub-agent invoked with **exactly one
`verification_packet` whose body is valid JSON**. Independent review is pending and may find
additional defects. If it or any rerun finds a defect or drift, fix it and re-run the affected suites
plus `scripts/verify_release.sh`. Only then commit and push to `origin p14-durable-cancellation`
(currently no upstream). No production deployment.

---

## Historical checkpoint — handoff start (2026-09-13T13:35:05Z) [SUPERSEDED]

> HISTORICAL SNAPSHOT. Describes the *starting* state and the work planned at handoff start. It was
> subsequently completed; do not read it as current instructions or current status.

- Task scope read from `docs/TASKS.md` at the time; P14-T01 status was `todo` -> set to `doing`.
- Existing changes preserved at handoff start: modified `docs/PROGRESS.md`, `docs/TASKS.md`,
  `src/agent_loop.rs`, `src/main.rs`, `src/recording.rs`, `src/storage.rs`,
  `tests/test_migrations.py`; untracked `migrations/009_run_cancellation.sql`.
- Already implemented at start: schema v9 `run_controls`; `DbStore::request_cancellation`,
  `cancellation_requested`, `finalize_cancellation`, `retry_recording`; the two endpoints;
  cooperative cancel checks in the loop top / after model calls / before tool calls /
  `await_permission`; `src/main.rs` health test corrected `schema_version 8 -> 9`; **193** Rust tests
  passed at that point.
- Planned/pending at start (all since done): sub-agent cancel checks and forced `interrupted`
  finish; live process-group termination on cancel; confirm the turn-finalize race never records an
  empty `complete`; retry-safety evidence coverage; Stop/Cancel + server Retry UI (absent at start);
  focused regression coverage; run the gates.
- `verify_e2e.sh` was believed blocked at that time because browser dependencies were absent; that
  was later resolved by provisioning the test runtime (see current verification above).
- No commit or push was performed at any point.

## Historical checkpoint — mid-session (2026-09-13T13:50:07Z) [SUPERSEDED]

> HISTORICAL SNAPSHOT of the implementation increment. Superseded by the CURRENT STATUS and
> authoritative verification above.

- Completed: mid-flight provider cancellation with `Ctx::race_cancellation` and interrupted
  model-call step closure; sub-agent cancellation (`subagent::Stop::Cancelled`); process-group
  registry (`src/processes.rs`, `run_capped_for`); Stop + safe-boundary Retry UI; five Rust retry
  regressions; `tests/test_recording_contracts.py` applies migration 009; mocked and real e2e
  coverage added.
- Verification recorded then: `scripts/setup_browser_tests.sh` exit 0; strict gate later exit 0; the
  standalone `verify_local.sh` run first failed at mocked-browser before deps were provisioned.
- Final commit/push remained parent-owned and unperformed.
