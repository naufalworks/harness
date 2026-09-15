# PROGRESS — journal

## 2026-09-15T15:45:00Z · P14-T03 — done; process, Git and checkpoint controls

P14-T03 finished on branch `p14-t03` in `/root/development/harness-p14-t03`. The Git tool now
provides bounded read-only status/diff/log evidence and focused commit proposals. Checkpoints store
tracked patches plus metadata, explicitly exclude untracked files, and refuse restore when HEAD,
tracked worktree state, or checkpoint metadata is stale/inconsistent. Restore, commit, and push are
approval-gated mutations even under automatic permission modes.

Detached Bash processes are registered with scope, request, summary, log, and start time. The
in-memory registry purges dead entries, bounds its size, removes a record before signalling, and
never replays old PIDs after restart. Authenticated API routes and the Imports UI expose read-only
Git state plus safe process inspection and stop controls.

Final evidence: full Rust tests passed 260 tests; strict clippy and formatting passed; migrations
001→013 passed; API schema matched 54 operations; mocked browser suites and browser-to-service E2E
passed approval/denial, crash recovery, cancellation/retry, unsafe-retry refusal, and no-JavaScript-
exception coverage. The next roadmap task remains deferred until this branch is committed cleanly.

## 2026-09-15T07:59:00Z · P14-T02 — done; session and approval workflow

The autonomous continuation completed the earliest eligible medium-priority task after P15-T03.
Migration 013 adds session title, archive timestamp and fork lineage without changing existing
message/receipt history. The API and UI now support bounded search, archive/restore, rename and
history-preserving forks. Permission projections sweep expired rows before listing, expose a
request-scoped bundle identity/count, and the UI shows the remaining deadline with wording that
never claims approval was recorded before the operator acts. Existing generating, complete, failed,
interrupted, stop and safe-retry states remain evidence-bound.

Final evidence: `cargo test --locked` passed 256 tests; strict clippy passed; migration and API
schema contracts passed (001→013 and 51 operations); mocked browser verification passed; and real
browser-to-service E2E passed approval, denial, crash recovery, cancellation/retry and unsafe-retry
refusal coverage.

## 2026-09-15T07:40:50Z · P15-T03 — done; governance history stays reviewable

The autonomous continuation found two release-gate mismatches before calling the task complete. The
SQL contract test still built only migrations 001→004 while extracting the branch-aware approval
statement from Rust, so every approval failed on the missing `memories.branch` column. I upgraded
that mirror to the complete 001→012 chain and made its transaction driver pass the checked-out
branch and decision-group parameters. This preserves the contract test's purpose — execute the
SQL the service actually ships — instead of weakening the new branch constraint or deleting the
old approval cases.

The API schema gate also found the four P15-T03 operations absent from the human HTTP inventory.
I added `GET /memory/governance`, `GET /memory/entries/{id}/timeline`,
`POST /memory/entries/{id}/governance`, and `POST /memory/branches` to `docs/ARCHITECTURE.md`.
The OpenAPI document and router already agreed; the inventory now agrees too.

The task's done-when is satisfied by the implementation already in the worktree: branches,
considered/chosen/superseded decisions, expiry-aware recall and sweeps, conflict/deduplication
groups, usefulness feedback, and pinned profile entries all retain review/revision history. I did
not add a speculative governance panel to `static/*`: there is no existing governance workflow
to extend, and adding a UI without a product contract would be lower priority than finishing the
durable/API contract. The existing browser suites still pass, including the retrieval and memory
review coverage.

Evidence from the final gates: migration chain 001→012 with `user_version=12`; SQL contracts 16
passed; API schema 49 operations plus 16 error codes, 22 receipt fields and 5 request states
matched; memory tests 29 passed; full Rust suite 256 passed; clippy and formatting clean;
`verify_local.sh` passed with only environment-only coverage/signature/cross-target/public-smoke
checks skipped; and both mocked-browser suites passed.

P15-T03 is now marked `done`. The next ledger candidate remains P15-T04, but it is blocked on
P13-T02, so no follow-on implementation was started in this turn.

## 2026-09-15T06:45:00Z · P15-T02 — done; explaining retrieval without claiming causation

Selected by the ledger rule, not by preference: after P15-T01 this was the only `high` with all
dependencies `done`.

The done-when has two halves, and the second one is where the honesty risk lives. Retaining
included/excluded candidates with scores, reasons and revisions is bookkeeping. "Preview
deterministic retrieval changes without causal overclaiming" is a claim discipline: the tempting
feature is "approving this memory would change the answer", and I cannot support that sentence.
Nothing here runs a counterfactual generation, so every string the user sees says *sent to the
model* or *retrieval would change*, never *the answer would change*. The preview response carries
that note from the server, and the UI test asserts the note is present.

Design decision that mattered most: one ranking implementation. `rank_in_tx` now serves live
recall, the persisted receipt and the rehearsal. A separate preview ranker would have been a copy,
and the copy is what would have drifted — the same trap P15-T01 avoided by measuring the real
`DbStore::recall` instead of a Python reimplementation. The preview opens an `IMMEDIATE`
transaction, ranks, optionally applies the pending approval, ranks again, and always rolls back;
`persist: false` additionally skips the embedding upsert and the `recall_count` increment, so
rehearsing is observation-free rather than merely non-destructive. A test asserts the candidate is
still pending and recall still returns nothing afterwards, because "it rolls back" is the kind of
claim that stays true only while someone checks it.

Receipts are write-once: `save_retrieval_receipt` inserts with `ON CONFLICT DO NOTHING` and
returns whether it wrote, and the test asserts `true` then `false` on retry. `revision` is stored
per candidate so a later memory edit cannot retroactively change what the receipt says was sent.
The prompt is fingerprinted, never stored.

Three defects my own gates caught, all worth naming:

- `cargo check` flagged `recall` / `recall_with_strategy` as never used once `recording.rs` moved
  to `recall_explained`. Left alone it would have failed `clippy -D warnings`. They are still the
  public API the eval and the memory tests drive, so they carry `#[allow(dead_code)]` with the
  reason written down, not a deletion.
- clippy `type_complexity` on the nine-column receipt header row. Factored into a named
  `ReceiptHeader` alias whose comment lists the column order.
- **The one I would have shipped blind:** migration 011 moved `user_version` to 11, but readiness
  still hard-coded `schema_version==10`. The full suite failed three tests — one schema assertion
  and two `/health` tests returning 503 rather than 200. A gate that only ran the new tests would
  have passed and the deployed server would have reported itself not ready. Fixed in
  `storage.rs` and the two assertions.

One UI assertion changed rather than being worked around: `ui_smoke.cjs` asserted the proposal
controls were exactly `['Save','Edit','Dismiss']`. Adding "Preview retrieval" between Edit and the
destructive Dismiss breaks it by design, so I updated the expected list rather than loosening the
assertion to a `includes` check — the exact list is what catches a silently reordered or duplicated
control.

Evidence from the commands actually run: `cargo test --locked context` 11 passed · full suite
**254 passed** (was 252) · `cargo clippy --locked --all-targets -- -D warnings` clean ·
`cargo fmt --all` clean · `python3 tests/test_migrations.py` 001→011, `user_version=11` ·
`scripts/verify_browser.sh` both suites passed, with three new checks
(`retrieval_receipt_panel`, `retrieval_receipt_text_inert`, `retrieval_preview_rehearsal`).

Declared-files caveat, flagged rather than papered over: the task lists `migrations/*`,
`src/context.rs`, `src/storage.rs`, `static/*`. The work also had to touch
`src/storage/memories.rs` (recall and the ranking live there), `src/recording.rs` (the only place
that knows a turn's request id at recall time), `src/api/routes.rs` (the two new endpoints),
`src/recording_tests.rs`, `src/main.rs` (schema-version assertion) and the tests/docs listed in
`CHANGED_FILES.md`. Noted in the ledger too.

Next by the selection rule: P15-T03 (`medium`, depends P15-T02, `parallel: no`). P15-T04 also
unblocks only once P13-T02 is done. P13-T04 remains blocked on P16-T03.

## 2026-09-15T05:50:00Z · P15-T01 — done; the measurement overruled the plan

I planned to calibrate thresholds by observing the numbers and then writing honest ones. That
part went as intended. What I did not expect was that the eval would immediately invalidate the
obvious fix for the one bad case it found.

Shape of the work, and why it is split this way. Ranking is Rust, so the measurement is a Rust
test (`storage::tests::recall_eval_fixtures_meet_labeled_budgets`) that drives the real
`DbStore::recall` over `tests/recall_eval/fixtures.json` and writes `metrics.json`. The Python
side (`run.py --check`) validates those metrics and nothing else. Reimplementing the reranker in
Python would have measured the copy rather than the shipped code, and would have drifted silently
the first time the Rust changed. This is exactly the order the declared verify command implies:
`cargo test` produces the evidence, `run.py --check` refuses to accept bad evidence.

Because a gate that cannot fail is decoration, I tampered with it four ways: deleted
`metrics.json`, set `fixture_sha256` to zeros, injected a stale leak, and changed the recorded
model. All four exit 1 with a specific reason. The `fixture_sha256` check is the important one —
it stops someone editing the fixtures to be easier while an old passing `metrics.json` sits on
disk.

The finding. Case `no-match-returns-nothing` uses the prompt "xylophone quarterly submarine",
which shares no token with any memory, so FTS contributes nothing — yet recall returned two
memories. They cleared the vector arm's `> 0.01` cosine floor on hash noise alone. My first
instinct was to raise that floor, so I measured the scores before touching the constant:

    noise  "xylophone quarterly submarine" vs "database\nSQLite"            0.069
    noise  "xylophone quarterly submarine" vs "language\nRust"              0.066
    signal "rustacean tooling preferences" vs "language\nRust systems ..."  0.042

The noise outscores the signal. There is no threshold that removes the false positives and keeps
the morphology bridge, which is the entire reason the vector arm exists; a floor above 0.069 would
also break `character_features_recall_related_spelling`. Raising the constant would have looked
like a fix, passed my own eval, and quietly deleted the feature. So I did not tune it.

What I did instead is the task's own "optional" clause, now motivated by evidence rather than by
symmetry: `embeddings::Strategy` with `HARNESS_MEMORY_SEMANTIC_RECALL=0` selecting lexical-only.
The default is unchanged, so no deployment behaviour moves. Two deliberate choices there. The env
string is parsed by a pure function (`parse_strategy`) and the strategy is an *argument* to
`recall_with_strategy`, because mutating process environment inside a test races every other test
in the same binary. And the eval measures both arms, so the choice is informed:

    hybrid       macro_precision 0.786   relevant_coverage 1.000
    lexical_only macro_precision 0.857   relevant_coverage 0.875

That is the real trade: the vector arm buys recall coverage and pays in precision. Neither is
strictly better, which is why this is an operator switch and not a silent default change. Stale
use is 0.000 everywhere — archived rows never reached the context in any case, so the ranking's
`status = 'active'` filter is doing its job. Note that the schema only allows `active` and
`archived`, so "stale" had to be modelled as archived rows that still match the prompt strongly,
plus aged active rows; there is no separate stale state to assert against.

Thresholds are set from these measurements, not aspirationally: `min_macro_precision` 0.60 sits
below the measured 0.786 with room for the known noise case, `max_stale_use_rate` is 0.0 because
the measured value is 0.0 and any leak is a real defect, and `max_context_bytes` 6000 mirrors the
existing hard ceiling in recall rather than inventing a second number. Latency is 250ms as a
regression tripwire, not a benchmark — measured max was 1ms, and asserting anything near that
would fail on a noisy machine for no reason.

One self-inflicted defect worth recording. My first two `embeddings` tests were named
`the_vector_arm_...` and `hasher_noise_...`, neither containing "recall" — so they compiled, passed
under `cargo test`, and were silently skipped by this task's own verify command. The only visible
symptom was `filtered out` moving from 246 to 248 while `passed` stayed at 3. Renamed both. This
is the third time this trap has cost me something; the check that catches it is confirming that
`passed` *increases*, never that the run is green.

Also added `recall_lexical_only_strategy_suppresses_vector_noise`, which asserts the baseline
(hybrid admits the noise) and then that lexical-only returns nothing, and that a genuine lexical
match still comes back. Without the first assertion the test would pass even if the noise
disappeared for some unrelated reason, and would stop proving the switch does anything.

`src/storage/memories.rs` is touched, which is not literally in the declared `files` list
(`src/embeddings.rs, src/storage.rs, tests/recall_eval/*`). It is the submodule of `src/storage.rs`
that holds `recall`, and the switch cannot be honoured anywhere else. Flagging it rather than
pretending the list covered it.

Verification: `cargo test --locked recall` 6 passed (from 2), `cargo test --locked` 252 passed
(from 248), `run.py --check` PASS, clippy `-D warnings` clean, `cargo fmt --all` clean.

## 2026-09-15T05:40:00Z · P15-T01 — doing; and what the task actually still needs

Selected by the ROADMAP rule rather than by my own judgement. P15-T01 is the only eligible
`high` task: P15-T02 is also `high` but depends on this one, and everything else eligible is
`medium`. In the previous session I had suggested P16-T01 next because it unblocks P16-T03 and
P13-T04, which was wrong — P16-T01 is `medium`, and the rule is priority first, `release-blocker`
second, task ID only as a tiebreak. Unblocking value is not a term in it. Dependency P11-T03 is
done, so this is eligible now.

Recon before planning, because the task title oversells what is missing:

- `src/embeddings.rs` already exists (109 lines): a signed feature-hashing model, `MODEL =
  "harness-local-hash-v1"`, `DIMENSIONS = 256`, deterministic, normalized, with `encode`/`decode`/
  `cosine` and no network or model download.
- `DbStore::recall` in `src/storage/memories.rs:117` already implements the hybrid: a lexical FTS5
  top-20 unioned with a vector top-20, reranked on lexical rank, cosine, scope, recency and a
  usefulness ratio, then truncated to a 6,000-byte ceiling. Vectors are cached in
  `memory_embeddings` and re-embedded on a content-hash miss.
- So the "optional local semantic embeddings coexist with deterministic hashing" clause is
  substantially already met, and is covered by two passing tests.

Baseline evidence: `cargo test --locked recall` -> `2 passed; 0 failed; 246 filtered out`
(`embeddings::tests::character_features_recall_related_spelling` and
`storage::tests::hybrid_recall_uses_offline_vectors_shadowing_usefulness_and_budget`).

Two genuine gaps remain:

1. `tests/recall_eval/` does not exist, so the declared verify command
   `python3 tests/recall_eval/run.py --check` cannot run. This is the standing WARN that
   `check_docs.py` has been reporting. The clause requires labeled fixtures measuring precision,
   stale use, latency and context cost.
2. Nothing is actually *optional*. Grep finds no `HARNESS_*` toggle for embeddings or recall; the
   feature-hash vectors are always computed. The clause's "before enabling a model" only means
   something if enabling is a decision the operator can make, and if the measurement gate exists
   to inform it.

Planned order: build the eval harness first (it is the blocking half and the missing verify
command), then add the opt-in switch so the measured numbers are what gates enabling. Note that
`docs/PLAN.md:25` and `docs/ARCHITECTURE.md:123` both promise no embedding API or model download
and explicitly make no synonym-quality claim; the switch must not quietly break either promise,
so it will select among local strategies rather than introduce a remote model.

## 2026-09-15T05:30:00Z · P14-T04b — done; capability detection moved before dispatch

The last open clause was capability detection. Step 1 had already made the tools-unsupported
fallback reactive and universal, but every turn still paid one rejected call to rediscover the
same fact. The fix caches the discovery on the shared health state from step 2:
`tools_supported(model)` / `note_tools_unsupported(model)`.

It is centralised in `complete_with_tools` rather than at the two call sites in `agent_loop.rs`.
Both the parent loop and delegated sub-agents route through that one method, so a single
implementation cannot drift between roles — the exact defect step 1 existed to fix. Detection is
per model rather than global, and optimistic: a capability is assumed present until the provider
actually rejects it, so a healthy provider is never downgraded on a guess. Because `MemoryAgents`
is cloned per request, the cache had to live behind the shared `Arc`; a per-clone cache would
rediscover the rejection on every turn and the clause would be unmet in practice. A test asserts
the cross-clone visibility for that reason.

The reactive fallback from step 1 is deliberately left in place. Pre-dispatch detection cannot
know about a model it has never called, so the first call still needs somewhere to land.

All six done-when clauses are now covered: capability detection (this step), role fallback
(step 1), circuit breakers and Retry-After/jitter (step 2), foreground/background fairness and
role-specific budgets (step 3). The fail-closed limits from P14-T04a are preserved throughout:
every new refusal path is enforced inside the existing reservation transaction or ahead of
`reserve_spend`, and no new code path reaches the provider without a reservation.

Verified at the end state, not just per step: `cargo test --locked provider` → `34 passed; 0
failed` (23 at the start of this task), full `cargo test --locked` → `248 passed; 0 failed`,
`python3 tests/recording_integration.py` → `PASS`, clippy `-D warnings` clean.

## 2026-09-15T05:25:00Z · P14-T04b step 3 — fairness and role budgets inside the reservation

Recon settled where this belonged. The reservation decision already lives in one atomic
transaction (`src/storage/provider.rs:16`), which reads per-turn and per-UTC-day totals and writes
a durable `refused` row plus a `provider_spend_refused` event. Putting fairness anywhere else
would mean deciding on counts that could change before the insert, so the new checks go inside
that same transaction and inherit its atomicity and its audit trail for free.

The role vocabulary already existed and did not need inventing: `kind` is persisted on every
`provider_calls` row, and callers pass `model_call` (the turn itself, including sub-agents),
`compaction`, `verification`, and `extraction`. So foreground/background is a classification over
data already recorded, not new plumbing.

Two new refusal reasons, both fail-closed and both durable:

- `foreground_reserve` — background work is refused once the day's usage plus the reserve would
  reach the shared daily ceiling. The point is that a busy extraction worker must not spend the
  last request of the day and leave the user's own turn refused.
- `background_request_limit` — an optional daily ceiling for background roles alone, which binds
  even when the shared budget is wide open.

Decisions worth defending:

- **Unknown roles are treated as background.** `is_background_kind` is `kind != "model_call"`
  rather than a whitelist of the three known background kinds. If someone adds a role later and
  forgets this function, the failure mode is that their new work politely yields; a whitelist
  would have made the failure mode "new work outranks the user's request", which is worse. A test
  pins that an unrecognised kind yields.
- **Fairness is on by default**, reserving a tenth of the daily budget, because a fairness
  mechanism that ships disabled protects nobody. Note `env_u64` rejects zero, so the smallest
  configurable reserve is 1 rather than "off" — called out because it is a real limitation of
  reusing that parser.
- **The new checks are appended after the existing ones**, so P14-T04a's precedence and its
  reason strings are untouched; the pre-existing refusal test still passes unchanged.
- **A background ceiling never blocks foreground work.** Asserted explicitly, since the obvious
  implementation mistake is to apply the background counter to every call.

Verified: `cargo clippy --locked --all-targets --all-features -- -D warnings` clean;
`cargo test --locked provider` → `33 passed; 0 failed` (31 before, `filtered out` unchanged at 214,
so both new tests are inside the gate); full `cargo test --locked` → `247 passed; 0 failed`, which
I ran because the two new `SpendLimits` fields change defaults for every caller, not just this
path; `python3 tests/recording_integration.py` → `PASS` (its `budgets` case exercises the
reservation path).

Remaining on this task: pre-dispatch capability detection. Step 1 made the tools-unsupported
fallback reactive and universal; the open clause is avoiding the wasted first call, which can
cache on the shared health state added in step 2.

## 2026-09-15T05:20:00Z · P14-T04b step 2 — circuit breaker, Retry-After, and jitter

Recon first, because the shape of the fix depended on facts I did not have. Three of them
mattered:

1. There is no retry, backoff, jitter, or breaker code anywhere in `src/`, and `memory_agents.rs`
   contained no `Arc`, `RwLock`, or `Mutex` at all. Genuinely greenfield shared state.
2. `MemoryAgents` is *cloned* (`main.rs:213`, `main.rs:233`, `recording.rs:832`). A breaker stored
   as a plain field would therefore be per-clone and would never trip. The state lives behind an
   `Arc<Mutex<_>>` so every clone observes one decision; a test asserts exactly that, since it is
   the property most likely to be broken later by someone adding a field.
3. There are exactly **two** `reserve_spend` call sites (`complete_turn`, `stream_turn`), so every
   metered provider call funnels through two places rather than the seven public methods. The
   breaker is gated there and nowhere else.

The gate runs *before* `reserve_spend`, deliberately. A refused call must not consume a request
from the per-turn or per-day budget; otherwise a sick provider would quietly burn the caller's
ceiling while doing no work. That keeps the P14-T04a fail-closed limits intact: nothing routes
around `reserve_spend`/`finish_spend`, and no new provider call is introduced.

Design decisions worth defending:

- **A 400 never trips the breaker.** Only 408/429/5xx and transport faults count. One malformed
  prompt taking the provider offline for every other caller would be a worse failure than the one
  being prevented.
- **Jitter spans [50%, 100%] of the base, not [0%, 100%].** Textbook full jitter can select a
  near-zero wait, which defeats the purpose of having opened the breaker.
- **`Retry-After` is honoured only in delta-seconds form, and clamped to the 60s cap.** The
  HTTP-date form is ignored rather than half-parsed: a misparsed date could stall the provider far
  longer than any backoff we would choose. A provider asking for an hour gets 60s.
- **The classifier reads the status back out of the error message** that `response_json` formats,
  reusing that one contract instead of adding a parallel error type. That coupling is real, so a
  test pins the message format and will fail loudly if it changes.
- **A poisoned mutex recovers** instead of panicking, so a breaker cannot permanently wedge the
  provider.

One structural finding recorded for whoever does step 3: both `response_json` and
`consume_stream_response` reduce a response to a status plus body, so headers are gone by the time
a failure is classified. `Retry-After` is therefore captured at the two funnels while the headers
are still in hand, not downstream.

Clippy earned its keep: it rejected a `health()` accessor I had added that nothing used. Removed
rather than silenced with an allow — speculative API is not worth a lint exemption.

Verified: `cargo test --locked provider` → `31 passed; 0 failed` (24 before; the seven new tests
all sit in `provider_tests`, whose module path the gate's filter already matches, and `filtered
out` stayed at 214 confirming none were skipped). `python3 tests/recording_integration.py` →
`PASS`. `cargo clippy --locked --all-targets --all-features -- -D warnings` clean.

Remaining for this task: foreground/background fairness and role-specific budgets, plus
capability detection *before* dispatch, which can now cache on the same shared health state
instead of needing new plumbing.

## 2026-09-15T05:10:00Z · P14-T04b step 1 — one discovered capability, applied to every role

Recon for the role-fallback clause found the asymmetry worth fixing first, and it was not a
missing feature so much as an inconsistent one. The parent loop already degrades when a provider
rejects `tools`: it marks the step `tools_unsupported`, drops the definitions and answers from
text. The delegation path, given the *identical* error from the *identical* provider, had no such
handling — it recorded `provider_failed` and stopped the sub-agent. So the same capability of the
same provider produced two different behaviors depending on which role made the call.

The fix reuses the existing `memory_agents::is_tools_unsupported` rather than adding a second
notion of the same capability, and mirrors the parent's semantics in the delegated loop. It is
bounded by construction: the retry has empty definitions, so the guard is false on a repeat and
the sub-agent stops instead of spinning.

Two things worth recording because neither was obvious:

1. The new test was initially named without the word `provider`, and the task's own verify command
   is `cargo test --locked provider`. The run reported `23 passed; 215 filtered out` — the test
   compiled and was silently skipped by the gate meant to prove it. Renamed so the declared verify
   actually exercises it; the run then reported `24 passed; 214 filtered out`. A done-when proved
   by a filter that excludes its own test is not proved at all.
2. The test was confirmed load-bearing by temporarily disabling the fallback, at which point it
   failed. The failure mode was worse than expected: the turn still recorded `state: complete`,
   but answered with the sub-agent's text, because the failed delegation shifted the scripted
   replies by one. The old behavior was not a visible provider error but a plausible wrong answer.
   The file was restored to the exact pre-experiment hash before committing.

Spend safety is untouched: no new provider call is introduced. The degraded retry is an ordinary
`complete_with_tools` call that still brackets itself in `reserve_spend`/`finish_spend`, exactly
as the parent's existing retry does, so the P14-T04a fail-closed ceilings still see every call.

Verified: `cargo test --locked provider` → `24 passed; 0 failed`, `python3
tests/recording_integration.py` → `PASS` (it reports `NOT RUN` until `cargo build --locked` has
produced a binary, which is easy to mistake for a pass), and `cargo clippy --locked --all-targets
--all-features -- -D warnings` clean.

Remaining clauses: capability detection before dispatch, circuit breakers, `Retry-After`/jitter,
foreground/background fairness, and role-specific budgets. Pre-dispatch detection needs a cache on
`MemoryAgents`, which today has no interior-mutable state; that is the next commit.

## 2026-09-15T04:50:00Z · P14-T04b — doing; and a correction to the suggested next step

Task: P14-T04b, improve provider scheduling and resilience; status: `todo` → `doing`; priority:
high; lane: providers; worktree `../harness-p14-providers` on branch
`p14-t04b-provider-resilience` off 2805d71.

**The previous session's suggested next step was wrong, and this entry corrects it rather than
inheriting it.** That handoff proposed P13-T04 next. P13-T04 cannot be started: it depends on
P16-T03, which is still `todo`. Selecting by the documented rule instead — highest-priority
eligible `todo`, `release-blocker` first within the tier, then earliest stable ID — the eligible
set whose dependencies are all `done` is P14-T04b and P15-T01 at `high`, then P14-T02, P14-T03,
P14-T05 and P16-T01 at `medium`. Neither `high` task carries `release-blocker`, so the earlier ID
wins and P14-T04b is the correct pick. P15-T01 is equally eligible and is the natural next one.

Recon before writing code, because the six done-when clauses could plausibly have been partly
built already. They are not: at 2805d71 there is no `circuit`, `breaker`, `Retry-After`,
`retry_after`, `jitter`, `capabilit*` or role-`fallback` symbol anywhere under `src/`. The three
`fallback` matches are unrelated — `grep_fallback` in `fs_tools.rs`, `fallback_symbols` in
`repo_map.rs`, and an unused-status fallback in `api/error.rs`. `src/storage/provider.rs` is 143
lines and exports no `pub` item at all. So all six clauses are greenfield.

The constraint that shapes the work: P14-T04a is a `release-blocker` that is already `done`, and
its done-when says unknown cost must never be treated as zero. Its mechanism is `SpendLimits`
plus `reserve_spend`/`finish_spend` bracketing every provider call in `src/memory_agents.rs`
(`complete`, `stream_turn`, `complete_with_tools`, `compact`, `verify`, `extract`). Retries,
fallback and breaker probes are all *additional provider calls*, so each one must pass through
that same reservation rather than around it, or resilience work would quietly reopen the spend
hole a release blocker just closed. Role-specific budgets therefore layer on `SpendLimits` and
must stay fail-closed when pricing is unknown.

No source file has changed yet; this entry records the transition and the selection correction.
Next: the capability-detection and role-fallback clauses, each with its own test, before breakers.

## 2026-09-15T04:35:00Z · P12-T01b — done; the decision is to stop, and it is now measured

P12-T01b was written as an either/or: justify a new boundary before splitting `run`/`impl Ctx`
further, or record the decision to stop. The decision is to stop, on both the split and the
rename. What changed today is not the conclusion but its basis — P12-T01 asserted that the two
share the same eight private `Ctx` fields, and that assertion had never actually been checked.

It now has been. Every unit was scored for which of the eight fields it touches, and six seams a
reasonable reviewer might propose were scored for what they would take versus what they would
leave behind:

- `cancellation` (39 lines) needs `store`, `request`
- `permissions` (44) needs `request`, `session`
- `verification` (70) needs `store`, `request`, `session`, `model`
- `tools` (157) needs six of the eight
- `delegation` (288) needs seven of the eight
- `orchestration` (350) needs seven of the eight

Not one of them has a single exclusive field. Every seam needs fields the remaining module still
needs, so each split would produce two files reaching into the same state — worse than one
cohesive file, because coupling the compiler currently enforces would have to be re-exposed as
`pub(super)` or threaded through call sites by hand. The parent is 2,662 lines, but 1,570 of
those are tests; the code actually under debate is about 1,090. Line count was never the
argument and still is not.

The rename to `agent` is also declined, with its cost measured rather than guessed: 33 references
across eight source files plus a Python test, and 52 doc mentions, 23 of which are journal
entries that were true when written and that the done-when requires leaving untouched. Renaming
would therefore manufacture a split vocabulary — code saying `agent`, accurate history saying
`agent_loop` — in exchange for no structural change.

Two corrections to the inherited notes. The claim of "37 loop tests" is wrong: the test binary
lists 23 under `agent_loop::`, 21 in the parent and 2 already in `compaction`. The 1,570-line
figure is right, but that module holds 21 tests and 14 shared helpers. And `race_cancellation`
(18 lines) and `refuse_delegation` (15) touch no `Ctx` field at all, so they could become free
functions today — 33 lines, no new boundary, not worth the churn, but recorded so the next reader
need not rediscover it.

No source file changed in this task, by design. A decision to stop is the deliverable.

## 2026-09-15T04:20:00Z · P12-T03 — done; the contract is now checked from four sides

All four `done-when` clauses are met, each on its own gated commit:

- `f4c8fdf` errors carry a stable `code` and a `retryable` flag. The code is derived from the
  status, so all 37 construction sites stayed unchanged and the human sentences kept their exact
  wording — the envelope is additive, not a rewrite.
- `c63da5d`, `0951fbe`, `ac61e91` typed DTOs replace external `Value` indexing: `RequestState`
  instead of string comparisons, `ReceiptView` and `ChangeRow` for the fields handlers branch on.
- `4d2a3fa`, `27bd6a5`, `ac61e91` `docs/api.yaml` is published and compared against the router,
  the ARCHITECTURE inventory and the receipt builder, in both directions.
- `3208aeb` `static/api.js` is a schema-checked client; `app.js` delegates to it.

Three real defects surfaced from writing the checks rather than from reading the code. Cancel
never documented its ordinary 202. Retry documented a 200 no code path can produce. And the
frontend recognised failures by status number and by reading the human sentence, so a handler
answering a different status for the same situation would have silently changed the UI's
behaviour; it now branches on codes, and the client's codes and states are compared against
`src/api/error.rs` and `recording::RequestState` on every gate run.

Two things worth recording for whoever touches the assets next. Splitting the client into its own
file is not free: `/api.js` is a real route, precompressed by `build.rs`, fingerprinted in
`index.html`, listed in the contract and the inventory (43 operations on 40 paths), and mocked in
both browser fixtures — which would otherwise 404 it and break the page under test. Concatenating
it into `/app.js` to avoid that was rejected: `APP_JS_GZ` is gzipped from `static/app.js` alone,
so gzip clients would have received a bundle with no client at all. Separately, the auth guard in
`src/main.rs` asserted `app.js` contained the `/auth/session` fetch; moving the exchange broke it,
which is the guard working. It now checks both files for credential leaks.

Every new cross-check was mutation-tested before being trusted: dropping or inventing an error
code or a request state, leaving a state unlabelled, and branching on a code the server cannot
send are all caught, with a clean control run.

Deliberately left open: the crate-wide state-string swap in `context.rs`, `agent_loop.rs` and
`main.rs` (~150 mostly-test sites), field-level schemas for the other 40 operations, and the
readiness payload shapes. The receipt is the one success payload specified in full, and the
contract's header says so rather than implying complete coverage.

## 2026-09-15T03:20:00Z · P12-T03 — doing; the task's own verify command names a test that does not exist

P12-T03 is `doing` on branch `p12-t03-api-contracts` (worktree `/root/development/harness-p12-t03`,
cut from `dd82434`). The P12-T02 lane was retired first: worktree removed, branch deleted after
confirming it was merged.

Reconnaissance changed the shape of this task. Three of the four declared artifacts do not exist
yet — `docs/api.yaml`, `tests/test_api_schema.py` and `static/api.js` are all absent, so this is
mostly greenfield contract work rather than a refactor of an existing contract. The task's
`verify:` command is `cargo test --locked api && python3 tests/test_api_schema.py`, which cannot
run today. That went unnoticed because `check_docs.py`'s verify-cmds check only resolves paths
matching `scripts/`, so a missing `tests/` target never warned. Widening that checker belongs
with this task, since this task is what makes the referenced test real.

Current surface being typed: `src/api/` is 1652 lines across assets/auth/error/mod/routes/stream,
with `routes.rs` the bulk at 956. Handlers return `ApiResult<Json<Value>>` and index untyped
`Value` receipts by string key (`receipt["state"]`, `receipt["request_id"]`), which is exactly the
external `Value` indexing the done-when clause targets. Errors are today a two-field
`ApiError(StatusCode, &'static str)` serialised as `{"error": "..."}` — no stable machine code and
no retryability signal.

Planned seams, each committed and gated separately: (1) stable error contract with
code/status/retryability, preserving the existing human sentences; (2) typed DTOs and state enums
replacing `Value` indexing in handlers; (3) `docs/api.yaml` plus `tests/test_api_schema.py`
checking it against the router inventory the routes check already counts; (4) a schema-validated
`static/api.js` client used by `app.js`; (5) widen the verify-cmds checker, then docs, gate,
integrate and promote. Nothing is claimed done until its gate runs.

## 2026-09-14T18:52:00Z · P12-T02 — integrated, pushed and promoted; the gate caught the live binary trailing HEAD

The decomposition existed only on `p12-t02-decompose` until now: `main` was still at `99e3972`
and `git branch --merged main` listed nothing else, so the nine seams, the ledger flip and the
rewritten AGENTS.md path table were not integration evidence yet. Integration was a clean
fast-forward `99e3972..b9e9b9c` (0 commits behind, no conflicts), then `origin/main` was updated.

The first strict gate run after integration reported `1 failing check(s)`:
`deployment: live 99e3972 trails HEAD b9e9b9c and code differs`. That is the gate working as
designed — everything else passed (native, contracts, property/fuzz, release artifact and
reproducibility, HTTP, mocked-browser, and the real browser-to-server E2E including the denial,
crash-recovery, cancellation/retry and unsafe-retry refusal paths), but a merged refactor that
is not the running binary is not a deployed refactor. `scripts/deploy.sh` promoted `b9e9b9c`
(pid 413812, release sha256 `8d6e611b`, schema 10, readiness verified, authenticated API
answering, non-object body refused with 400) after backing up the previous executable, and the
re-run gate reported `0 failing check(s)` with `deployment: live binary matches HEAD (b9e9b9c)`.

Unchanged and still open: the environment SKIPs (coverage, signing, cross-target, public HTTPS
smoke) remain CI-only on this offline host, and the two standing WARNs for the P17-T04/T05
verify commands stay until those tasks build their scripts. Next eligible work is P12-T03
(typed API and database contracts, `parallel: no`); P12-T01b remains optional.

## 2026-09-14T18:40:00Z · P12-T02 — nine seams, and the line-multiset check that made them boring

P12-T02 is done. Storage, the browser tool and the LSP tool were each cut along the boundaries
the task named — protocol, session, validation, apply — in nine separately committed seams, and
the interesting part was the method rather than the result. Every seam was produced by a small
script and then checked with a line-multiset comparison against the previous commit: every
non-blank line of the parent had to reappear somewhere in the module tree, and the only
permitted differences were ones named in advance (`pub(super)` markers, a module doc comment,
`use super::*;`, and occasional rustfmt signature reflow). That check is what turns "I moved
code" into evidence. It caught nothing dramatic, which is the point: a refactor whose safety
rests on reading the diff is a refactor that silently drops a validation arm.

Where the files landed:

- `src/storage.rs` 1475 -> 1138, with `storage/scope.rs` (scope limits, plan constraints
  mirroring the 003 CHECKs, `ScopePatch`, root-path canonicalisation) joining the existing
  `config`, `jobs`, `memories`, `provenance`, `provider` and `turns` modules. `DbStore` now only
  persists values those types already validated.
- `src/tools/browser_tool.rs` 1953 -> 549, with `protocol` (caps, destination validation,
  argument parsing), `snapshot`, `cdp` (socket, launch, process-group isolation) and `session`.
- `src/tools/lsp_tool.rs` 1607 -> 306, with `protocol` (caps, position conversion), `session`
  (framing, diagnostics wait, kill-group teardown), `format`, and `rename` (workspace planning
  and application).

No cap, permission diff, rollback path or recording call changed value or order; the seams moved
code, not behaviour. Gate after every seam and again at the end: the full test suite passes and
`scripts/verify_release.sh` reports `0 failing check(s)` with `documentation-claims` and the
strict release gate PASS.

Two things stay open on purpose. The remaining bulk in all three parents is `mod tests`, which
cannot move without inventing test-only visibility — the same boundary P12-T01 recorded and
declined to cross. And the environment SKIPs are unchanged: coverage, signing, aarch64 and the
public HTTPS smoke are CI-declared and still have not run on this offline host.

## 2026-09-14T16:15:00Z · P12-T06 — the deploy gate caught the new release lane clobbering the live binary

P12-T06 is integrated, pushed and deployed (`ae97327`, pid 342042, schema 10), and the strict
gate re-run after promotion is green including `deployment: live binary matches HEAD`.

One real defect surfaced during promotion, and it was mine. The new `release-quality` lane built
an optimized binary into the default `target/release/harness`, which is the exact path the
systemd unit runs. That overwrote the on-disk predecessor while the old process kept serving, so
`scripts/deploy.sh` refused to promote with `BLOCKED: current on-disk executable differs from
served health identity`. The refusal was correct: without a matching predecessor on disk there is
no attestable rollback target. I recovered by copying the still-running executable from
`/proc/<pid>/exe`, checking its SHA-256 against the served `binary_sha256`, and restoring it
before retrying; deployment then succeeded.

The cause is now removed rather than worked around. `scripts/release.sh` honours
`CARGO_TARGET_DIR`, and `check_release_quality.py` builds evidence artifacts into
`target-release-evidence/`, so running the verification gate can no longer disturb the deployable
binary or the rollback attestation. The coverage, aarch64, Sigstore and public-smoke lanes remain
CI-only and still have not run on this offline host.

## 2026-09-14T16:05:00Z · P12-T06 — runnable evidence passed; networked evidence stays CI-only

The exact task command, `bash scripts/verify_release.sh && scripts/verify_e2e.sh`, exited 0
after the final workflow and release-script changes. The local gate now runs six named
production property contracts and refuses a silently smaller inventory; deterministic fuzzing
covers 2,000 import payloads, file-count/symlink boundaries, and every existing provider-stream
protocol boundary. The storage budget uses 10,000 sessions and 100,000 messages. Disposable
rollback passed. Two packages from the same release binary were byte-identical and each carried
a CycloneDX SBOM, manifest, and verified SHA-256 checksum.

The split is deliberate and visible. `cargo llvm-cov` is not installed, only the host Rust
target is installed, cosign is absent, and no public HTTPS URL/token was supplied. Those four
lanes printed **SKIP**, not PASS. A pinned networked CI job owns the 50% line floor, host and
aarch64 checks, two independent optimized builds, and keyless Sigstore bundle. The production
workflow owns the authenticated HTTPS smoke and cannot run without its environment variable
and secret. These CI-only steps are automated but have not run here, so this commit is not
evidence that coverage, cross-target compilation, signing, or public smoke passed.

The gate itself was negative-tested before completion: deleting the `cargo llvm-cov`
declaration made `check_release_quality.py` exit 1 naming the missing contract, and the workflow
was restored byte-identical. One additional leak was removed while wiring the performance gate:
`scripts/benchmark.py` now closes its in-memory SQLite connection explicitly. No production
service was restarted, no branch was pushed, and the 18 pre-existing worktrees were untouched.

## 2026-09-14T16:05:00Z · P12-T06 — release evidence started in an isolated worktree

P12-T05b is integrated and deployed, so this dependency is now eligible. Work is isolated on
`p12-t06`; the 18 pre-existing stale worktrees are intentionally untouched because some may
contain unmerged work. The plan is fail-closed and offline-honest: deterministic property and
protocol/import fuzz cases plus artifact/rollback checks run here; coverage, extra targets,
continuous fuzzing, public smoke, SBOM publication and signing run on a networked CI runner.
The local gate will distinguish a missing tool or unreachable public endpoint from a pass.

## 2026-09-14T15:45:00Z · P12-T05b — the gate caught my own last commit

Two findings, both from gates rather than from reading.

Promoting `ResourceWarning` to an error was supposed to be routine. It exposed 9 real leaks:
`with sqlite3.connect(...)` commits the transaction but does **not** close the connection, so
every read in `tests/recording_integration.py` leaked a handle. Worse, the promotion alone did
not fail anything — a warning raised inside a deallocator is printed as "Exception ignored" and
the process still exits 0. A leaky probe run under raw `python3 -W error::ResourceWarning`
exited **0**. So the suites now run through a wrapper that fails on the *text* as well as the
exit status, negative-tested both ways: leaky probe exit 1, clean probe exit 0.

The second finding is the one worth writing down. My previous commit, cb999fe, was a whole-tree
`cargo fmt` I described as "no behaviour change", and `cargo test` agreed: 220 passed. It still
broke two Python suites. They scrape Rust source as text for
`pub const NAME: &str = r#"..."#;` with the `=` required on one line, and rustfmt wrapped 7 of
31 declarations. The scrape returned 24 of 31 constants and `test_all_constants_present`
failed — but the quieter half is that the other tests kept "passing" against a silently smaller
dictionary. I verified the cause rather than assuming it: the suite is OK at `cb999fe~1` and
FAILED at HEAD, in a throwaway worktree. Both scrapers now tolerate whitespace around `=` and
assert the scraped set equals the declared set, so a reformat cannot quietly shrink coverage
again.

What this task did **not** earn: `cargo audit` and `cargo deny` are declared in the CI workflow
and have never run. They are not installed here and crates.io returns 403, so the local check
asserts only that those steps exist. `shellcheck` and `ruff` are also absent, so `bash -n` is a
syntax check and nothing more, and the supply-chain `lock` check stays SKIP offline and is
counted separately from passes. The 7 GitHub Action references are pinned to immutable commit
SHAs, resolved over SSH because HTTPS to GitHub is blocked here too.

The strict gate's one remaining FAIL is honest and expected: `deployment` reports that live
7480753 trails HEAD and that Rust files differ. cb999fe is the first non-docs-only divergence,
so the check that has been quiet for several doc commits finally has something to say. Deploy
follows this commit.

## 2026-09-14T15:30:00Z · P12-T07b — a gate for the failure mode this session kept demonstrating

Chosen because the drift was mine, twice in one session: I wrote that archiving was
"deliberately left unconfigured in production" and made it false an hour later, and I said
P13-T03 was next when the ledger said `done`. Both were caught by a human reading, not by a
gate. The clincher was that this task's own `verify:` command named `scripts/check_docs.py`,
which did not exist — the check that would have caught my stale claim had never been built.

`scripts/check_docs.py` now runs five mechanical checks and is wired into
`scripts/verify_release.sh` as `documentation-claims`, so it cannot be forgotten. The
deployment check encodes a distinction worth keeping: a live binary behind HEAD is only a
failure when *code* differs. Trailing by documentation commits is normal and reported as a
pass, which is exactly today's state (live 7480753, HEAD ahead by docs only).

**The first run failed 24 checks, and most of them were my fault, not the docs'.** Four were a
parser bug: `depends: —` treated an em dash as a task name. Twenty were a design mistake —
failing on every route README does not mention, which treats a quickstart as an API spec.
That is precisely the noise that gets a gate switched off, so the heuristic was replaced with
an explicit `## HTTP surface` inventory in `docs/ARCHITECTURE.md` compared exactly against
`router()` in both directions. The inventory also closed a real gap: the archive, privacy and
provenance routes shipped without appearing in any architecture doc.

**Two disclosures.** First, the initial 39-route inventory was generated from the router, not
hand-audited, so the first green comparison was circular; independence starts with the next
change. Second, a gate that has never failed is unproven, so it was made to fail on purpose:
deleting `/archives/{id}` from the inventory and inserting an invented route each produced
exit 1 naming the drift, and the file was restored byte-identical afterwards.

Two real findings remain as warnings rather than being silently fixed: P17-T04 and P17-T05
verify with scripts that do not exist. They are `todo`, so a warning is the honest level; the
check escalates to a failure if either is ever marked `done` while the script is still
missing. The volatile-count clause was also narrowed on purpose — counts live only in the
dated journals, and freezing evidence is correct, so the checker guards the living docs and
leaves history alone.

No Rust changed, so no compile or test gate was re-run; the task's own verify passed with 0
failing checks and a clean `git diff --check`.

## 2026-09-14T15:15:00Z · P13-T02b — key backup, and a rotation drill run for real

Two loose ends from the previous entry, in the order that risk demanded: back up the key
first, then test rotation. Rotating before a backup exists would have put two irreplaceable
keys on one disk instead of one.

**Backup, with its limit stated.** A second copy sits at `/root/keybackup-harness/` alongside
a `FINGERPRINT.txt` that holds key_ids and file digests — not secret, so it can be stored
anywhere and used to verify a restored copy later. This is redundancy against deletion, not
against losing the host. Copying a secret off this machine is not something this session can
do, so it stays an owner action rather than being quietly marked done.

**The rotation drill.** An archive was written under key_id `7a28e8208dee9282`. The key was
then rotated: a fresh key became current (`2e21f1c53993f75c`) and the retired one was kept as
`HARNESS_ARCHIVE_KEY_PREVIOUS`. After restart, the pre-rotation archive read back
byte-identical, sha256 `7f228e81…`. That is the whole point of the drill: it exercises the
`by_id` lookup resolving a header key_id to the *retired* key, which is precisely the code
path the P13-T02b bug destroyed, now proven in the deployed configuration rather than in a
unit test. A post-rotation write is stamped with the new key_id and round-trips as well, and a
header census showed both generations coexisting in one archive root. Both drill archives were
then deleted; the root is empty.

**One check failed and was redone rather than reported.** The first census loop printed
nothing — a `jq` parse against a `strings`-extracted line, with the error swallowed by
`2>/dev/null`. An empty result is not a passing result, so it was rerun with a parser that
reads the header directly. A verification step that cannot fail loudly is not a verification
step.

**Operational consequence, now documented.** The previous key is not decoration: while any
pre-rotation archive exists, losing `archive.key.prev` loses those archives. Both files must
be backed up, and a retired key may only be dropped once nothing references its key_id.

No code changed, so no gate was re-run and nothing was rebuilt beyond the restart needed to
load the new keyring.

## 2026-09-14T15:05:00Z · P13-T02b — archiving turned on, and the round trip that proves it

The previous entry said archiving was deliberately left off. The owner decided otherwise, so
that sentence is now wrong and is corrected here rather than edited out of the record.

A 32-byte key was generated at `/etc/harness/archive.key`, mode 0600, outside the repository.
`.env` gained two paths and no secret: `open_from_env` reads `HARNESS_ARCHIVE_KEY` as a
filesystem path, so putting key material in the environment would have been both wrong and
unnecessary. `.env` is gitignored and untracked, which was checked before anything was written
rather than assumed.

**The round trip, on the wire, against the deployed binary.** 4 KB of random bytes POSTed to
`/sources/src-roundtrip/archive`; read back byte-identical, sha256 `00940261…` in both
directions. That is the assertion the feature exists for, and it was unreachable until the key
existed. The stored `.har` is AES-256-GCM ciphertext behind a `HARNESS-EXACT` header naming
key_id `7a28e8208dee9282`, so "encrypted at rest" is observed on disk, not inferred from the
code. `forget` recorded 202 and an unknown action was refused 400 at the edge. After DELETE the
read is 404 — and an id that never existed returns the same 404 with the same wording, so a
deletion cannot be distinguished from an absence. Unauthenticated reads are still 401.

This also retires the last caveat on the `by_id` fix: a single-key deployment is exactly the
configuration that bug broke, and it is now the configuration running in production, reading
back what it wrote.

**Open, and owner-owned.** The key has no off-machine backup. There is no recovery path
without it, so until it is backed up, every archive written is one disk failure from being
permanently unreadable.

## 2026-09-14T15:00:00Z · P13-T02b and P12-T04 — the dead-code clause closes in two directions

The archive group and the retention group were both unreachable, and P12-T04 had kept both
behind documented allows. They did not deserve the same answer. Archiving had no second
implementation and no owner, so it was wired. Retention had one: `scripts/maintenance.py`
already does the same deletions against the same tables with the same evidence rows. Exposing
the Rust half would have shipped two implementations of the same destructive logic, kept in
sync by hand, one of them reachable with a stolen HTTP token. So it was retired (3165eed,
−11.7 KB from `src/storage.rs`), and the script is the single owner. Migration 010 and the
readiness projection stay, so `/memory/status` still reports when each maintenance action last
ran; the kept test seeds `maintenance_runs` the way the script does and proves the projection
is real rather than hard-coded.

**Wiring found a production bug that the library tests could not.** `KeyRing::by_id` was
`[current, previous].into_iter()` gated behind `self.previous.as_ref()?` — the `?` returns
`None` from the whole function when no rotation key is configured, which is the default. A
single-key deployment could write archives it could never read back. Every unit test passed,
because the one single-key read asserted a "deleted" error that fired before the key lookup.
The HTTP round-trip test caught it on the first run. This is the argument for the routing work
stated as evidence rather than as principle: a feature reachable end-to-end gets tested
differently from a library.

**Deploying found a second one, and only the wire could.** With archiving unconfigured, `GET`
and `DELETE /archives/{id}` answered `no bytes were stored` — a write-path sentence describing
an action that was never attempted. The tests asserted the status code, not the prose, so they
were green. The four routes share one refusal, so the sentence is now action-neutral
(8c9f241). Small, but it is the class of thing that only exists in the deployed artifact.

**On the 501.** Unconfigured archiving is not a fault, so 500 is wrong; nothing was stored, so
2xx is a lie. 501 naming the missing configuration is the only answer that an operator can act
on. Authentication still runs first, so an unauthenticated caller gets 401 and learns nothing
about which routes exist.

**Archiving is deliberately left off in production.** The key is long-lived and there is no
recovery path: lose it and every archived byte is unreadable. Generating and backing up that
key is an owner decision, not something a deploy should do quietly on someone's behalf.
`README.md` now documents the three variables and that constraint.

**Evidence.** clippy `-D warnings` exit 0 · 220 passed, 0 failed · `verify_local.sh` exit 0 ·
`verify_release.sh` exit 0, 12 [PASS] · deployed 8c9f241, pid 316123, `/health` `commit` and
`binary_sha256 25c91fa7…` both matching HEAD, schema 10, workers up. Checked on the wire: 401
before 501 on all four routes, provenance 200 for a valid UUID and 400 for a malformed one.

**P12-T04 is now `done`.** The sixth clause holds in the code, not by decision. Deviations (2)
and (3) stand — the provenance validation chain is still test-only, and nine browser/LSP panic
sites are still retained deliberately, because converting them reshapes the call path.

## 2026-09-14T14:30:00Z · P12-T04 — the last three implementation clauses; still `doing`

Three clauses, taken in order, each gated before the next began.

**Shared limits.** `src/limits.rs` now defines the verification bounds that `memory_agents.rs`
and `storage.rs` must agree on. They were duplicated const groups plus two bare `500`s in
`agent_loop/verification.rs`; nothing in the type system forced them to match, and a change to
one side would have been a silent truncation on the other. No value moved — the change is that
drift is now impossible. Caps used by exactly one module stayed local, because centralizing
those buys indirection and no invariant.

**Checked casts.** 68 production `as` casts, of which seven were converted. The filter was not
"could this ever be wrong" but "is this safe only because of a line somewhere else": the
archive header length (the on-disk format is a 4-byte field, so `MAX_HEADER` should not be the
sole guard), the stored embedding dimension, two LSP position casts, the `Drop` kill of a
browser process group (negating an out-of-range pid would signal the wrong group), the
edit-tool anchor line and pid registration, and `arg_usize` in `fs_tools`. The rest are
documented widenings — `len() as i64`, the intentional u64→usize fold in `embeddings.rs` — and
were left alone. Clippy's cast lint family was considered and rejected: turning it on is a
repo-wide diff, which is not what a cleanup task may smuggle in.

**Patch-option semantics.** `Option<Option<T>>` compiles and is wrong to read: nothing in the
type says which nesting level means "clear", so a dropped layer turns an explicit `null` into a
no-op and a partial settings save quietly wipes fields the caller never named. `ScopePatch` now
uses `crate::patch::Patch<T>` — `Unchanged`, `Clear`, `Set(T)` — with a `Deserialize` impl that
maps absent→`Unchanged` (through `Default`, since serde never calls the impl for a missing
field), `null`→`Clear`, value→`Set`. `apply` is the only merge path into `upsert_scope`, so an
`Unchanged` field cannot write by accident. Three tests pin the contract at the wire level, and
the empty-patch rule (`is_empty` → return the stored row, leave `updated_at` alone) is
unchanged.

**Evidence.** clippy `--all-targets --all-features -D warnings` exit 0 · 220 passed, 0 failed
(217 + the three new contract tests) · `verify_local.sh` exit 0 · `verify_release.sh` exit 0
with the real browser-to-server lane green. No behaviour changed, and nothing was deployed.

**Still `doing`, deliberately.** Five of the six done-when clauses now hold in the code. The
sixth — "dead code is connected or removed" — is answered by a documented decision, not by the
code: the archive and retention/maintenance surface is implemented, test-covered and reachable
from no route. Connecting it is routing work, tracked as P13-T02b. Flipping the status on the
strength of a deviation note is exactly the kind of accounting this journal exists to prevent.

## 2026-09-14T09:45:00Z · P12-T01 — done; seven verified seams, deployed as `b50811f`

P12-T01 is `done`. The decomposition was carried out as seven verbatim seams, each compiled,
tested and release-gated before the next began, so no seam could hide behind a later one.

**HTTP surface.** `src/main.rs` 3,947 → 2,530 lines. `src/api/` now owns it: `routes.rs` (813)
the routing table and handlers, `auth.rs` (260) `AuthState`/`AuthKind`, the constant-time token
comparison, the authenticate and response-hardening middleware, `stream.rs` (217) SSE,
`assets.rs` (117), `error.rs` (90), `mod.rs` (9). `main.rs` keeps only the process entry:
`Harness`, runtime identity, startup and shutdown.

**Agent loop.** `src/agent_loop.rs` 3,214 → 2,658 lines, with three child modules:
`steps.rs` (340) the durable step/permission transitions and the `impl DbStore` half,
`compaction.rs` (171) the provider-window compaction plus its two pure unit tests, and
`verification.rs` (96) the verification evidence. The verification seam was deliberately
non-contiguous: `CompletedToolCall` and `Delegated` sat inside the same block but belong to the
loop and to delegation, so they stayed in the parent.

**Two defects the process caught, worth recording.** First, the compiler: a pre-move grep said
`Ctx::allow` had no external callers, a test disagreed, and three `E0624: method is private`
errors followed — the grep was a hypothesis, the compile was the evidence. Second, and not
catchable by any gate we run: the compaction seam left `DEFAULT_CONTEXT_TOKENS`'s doc comment
behind in the parent, where it silently became the documentation for `MAX_VERIFICATION_STEPS`.
It compiled, 217 tests passed, and the documentation was wrong. That prompted a doc-adjacency
re-check of all earlier seams (`git show --unified=4` filtered for `///`/`//!` on removed lines
across `8c51c94 fdd830a fea3f18 40dd26d b30bcdc`), which came back clean: every doc comment had
travelled with its item.

**Not done, on purpose.** The planned six-way `src/agent/` split and the `agent_loop` → `agent`
rename were both dropped, and P12-T01b records why. `run` (~320 lines) and `impl Ctx` (~620)
share the same eight private `Ctx` fields: splitting them yields smaller files and no new
boundary, which is churn dressed as architecture. The rename would have invalidated ~30
truthful historical references in this journal and in `docs/TASKS.md` while changing nothing
structural. The 37 loop tests (~1,570 lines) stay in the parent because they drive `run`
through a scripted provider; only the two pure compaction tests could move without inventing
test-only visibility.

**No behaviour was changed.** The task was decomposition, so it did not touch performance,
hardening or bounds — those belong to P12-T04 and P12-T05b, separately gated, not smuggled into
a move commit.

**Evidence.** `cargo test --locked` → 217 passed, 0 failed; `cargo test --locked --no-run`
clean of new `unused`/`error` lines (the 31–32 pre-existing dead-code warnings are the unchanged
baseline); `scripts/verify_release.sh` → exit 0 with 12 `[PASS]`; `git diff --check` → exit 0.
Deployed from a clean tree: `scripts/deploy.sh` restarted the unit (pid 289107) and `/health`
answers `ready: true`, commit `b50811fda8b7eb09189cc44956fc0fec41018f28`, binary sha256
`8b934fc6…de34dd` matching `target/release/harness`, `schema_version` 10, `quick_check: ok`,
both workers live, and `POST /chat/submit` with `[]` still refused with 400. Rollback artifacts
captured under `.harness/deploy/` for the previous binary (`a9de940`).

Commits: `8c51c94` assets/error, `fdd830a` stream, `fea3f18` auth, `40dd26d` routes,
`b30bcdc` steps, `40536bb` compaction, `3f5af41` verification, `b50811f` test move + map.

## 2026-09-14T08:25:00Z · P12-T01 — started; HTTP assets and error layer extracted

P12-T01 is `doing`. `src/main.rs` was 3,947 lines and held the whole HTTP surface, so the work
is being done as a sequence of verified extractions rather than one sweeping rewrite: each step
moves code verbatim, keeps public behaviour identical, and is proved by the existing suites
before the next step starts.

Two cohesive modules exist so far, both under the new `src/api/`:

- `src/api/assets.rs` (117 lines) owns static delivery: the `ASSET_IMMUTABLE`/`ASSET_REVALIDATE`
  policies, the build-time gzip blobs, `accepts_gzip`, `asset`, and the `index`/`js`/`css`
  handlers. `include_str!`/`include_bytes!` paths were repointed to `../../static/…`.
- `src/api/error.rs` (90 lines) owns the JSON failure shape and the object-only body extractor:
  `ApiError`, `ApiResult`, `db_error`, `invalid`, `JsonBody`, `reject_body`, `json_content_type`,
  and the `FromRequest` implementation. `default_scope` stayed in `main.rs`, where it is a serde
  field default rather than part of the error layer.

`main.rs` is now 3,756 lines and its axum/serde imports shrank to what it still uses. Nothing was
rewritten while moving, so the behaviour these modules define is the behaviour that shipped in
a9de940.

Evidence: `cargo test --locked` 217 passed, 0 failed. `scripts/verify_local.sh` LOCAL_EXIT=0 and
the task's own gate `scripts/verify_release.sh` RELEASE_EXIT=0, with `[PASS]` for rust-tests,
rust-clippy, rust-build, python-contracts, integration-smoke, recording-integration,
javascript-syntax, mocked-browser, local-contract, real-browser-to-server, and the strict release
gate (`/tmp/p12t01_local.log`, `/tmp/p12t01_release.log`).

Not done yet, so the task stays `doing`: SSE/streaming (`STREAM_*`, `Frames`, `activity_stream`,
`generation_stream`), the middleware and routing table, and the `src/agent/*` split of the
3,214-line `src/agent_loop.rs` into orchestration, budgets, permissions, tools, delegation, and
verification. Those touch shared `Harness` state, so they are deliberately left to their own
verified steps instead of being rushed into this checkpoint.

## 2026-09-14T07:25:00Z · P11-T06 — deployed to the harness unit

`scripts/deploy.sh` deployed a9de940 to the `harness` unit: pid 263233, release sha256
9195b239cc72cd20de8016d6934e5a9e16ba082c0dd5bf74c7dac8a17b8cd8c7, schema 10, readiness
verified, API answering, non-object body refused with 400. Authenticated `/health` reports
commit a9de940c44517fdf7a58b58f319d9ef9265d7c27, ready true, and the same binary sha256.

Live delivery checked against the running service rather than trusting the tests alone:
`GET /app.js` with `Accept-Encoding: gzip` returns 200, `content-encoding: gzip`,
`vary: accept-encoding`, `etag "a9de940…-app.js-gzip"` and 18136 bytes; the same request
without `Accept-Encoding` returns 66176 identity bytes, no `content-encoding`, and the plain
`"a9de940…-app.js"` validator. Piping the compressed response through `gzip -dc` and comparing
with `cmp` against the identity response matched exactly, so the stored member really is the
same script the server would otherwise serve. Re-requesting with the gzip validator returned
304 with an empty body and `vary: accept-encoding` intact. `/` returned 4407 compressed bytes
(15847 identity) and `/style.css` 6304 (26459 identity), so the initial page load drops from
roughly 108 KB to 29 KB, a 73% reduction.

## 2026-09-14T07:23:00Z · P11-T06 — static assets compressed at build time; task complete

Closed the compression gap left by P11-T05 without adding a dependency. The obvious route
(`flate2` / `async-compression` / the `tower-http` compression feature) is unavailable here:
`static.crates.io` answers 403, so the crate registry cannot be reached and nothing new can be
pinned in Cargo.lock. This is a network egress restriction, not a missing account — crates are
fetched anonymously — so waiting for credentials would not have changed anything.

Instead the assets are compressed once at build time, which suits them: `index.html`, `app.js`
and `style.css` are fixed at compile time and already embedded in the binary. `build.rs` applies
the build-commit substitution first, pipes the result through the system `gzip -9 -n` (`-n`
omits the timestamp so the output is reproducible), and writes each member to `OUT_DIR`;
`src/main.rs` embeds them with `include_bytes!`. Serving a stored member costs no CPU per
request, unlike recompressing every response, and the compressed bytes are byte-identical
across builds of the same commit.

`asset()` now negotiates: it parses `Accept-Encoding`, honours `q=0` rejections and `*`, and
serves the stored member with `Content-Encoding: gzip` only when the client actually accepts it.
Every response carries `Vary: Accept-Encoding`, including 304s, and the two representations
carry different validators (`"<commit>-<asset>"` vs `"<commit>-<asset>-gzip"`), so a shared
cache cannot serve encoded bytes to a client that cannot decode them and revalidation stays
correct per encoding. If `gzip` is absent at build time the build still succeeds with a cargo
warning and an empty member, and the server simply serves identity bytes — graceful degradation
rather than a broken build.

Equivalence is tested without a decompression crate: a gzip member ends with the CRC32 and the
length of the original input, so the tests recompute both over the identity response body and
compare. That catches a stale or mismatched `.gz` serving different bytes than its ETag claims,
which is the failure mode that would actually hurt. A separate test fails if any embedded member
is empty, so compression cannot silently switch itself off.

Verified: `cargo test --locked precompressed` (2 passed), `cargo test --locked gzip` (1 passed),
`cargo test --locked fingerprinted_assets` (1 passed), `node --check static/app.js` OK,
`scripts/verify_browser.sh` exit 0, `scripts/verify_local.sh` exit 0 with rust-tests,
rust-clippy, rust-build, python-contracts, integration-smoke, recording-integration,
javascript-syntax and mocked-browser all [PASS], and strict `scripts/verify_release.sh` exit 0
adding local-contract and real-browser-to-server.

Not done, deliberately: brotli is not served (no `brotli` binary or Python module in this
environment) and dynamic JSON responses stay uncompressed, since they are small and `no-store`.
Both remain open only if crate-registry egress is ever granted.

## 2026-09-14T07:12:00Z · P11-T05 — deployed to the harness unit

- Pushed `5c69518` to `origin main` (`e52d0c9..5c69518`), then ran `scripts/deploy.sh` on the committed tree.
- Deploy evidence: `deployed 5c69518 to harness: pid 259501, release sha256 c069191f14133ecb82000fc5c93d5cfb72a86994f763a88a10d191101a9df94c, schema 10, readiness verified, API answering, non-object body refused with 400`.
- Live delivery check against the running unit: `/health` reports commit `5c69518a991df6ef6c4b61a7f97497c11e894c0d`; `/` returns `cache-control: no-cache`; `/app.js?v=<commit>` returns `public, max-age=31536000, immutable` with etag `"5c69518…-app.js"`; the same request with `If-None-Match` returns `304` with 0 bytes; the served document references `app.js?v=<commit>` and `style.css?v=<commit>`.

## 2026-09-14T07:10:00Z · P11-T05 — frontend delivery and idle work verified; task complete

- Idle work: the two always-on timers (1 s turn clock, 5 s status/inline-suggestions) are now a single `idleClock`. A hidden tab runs no timer at all — the clock is stopped on `visibilitychange` and, on becoming visible, does one immediate catch-up tick before restarting. The work each tick performs is unchanged, so this changes when idle work runs, never what it reads or renders.
- Bounded views: at most 300 rendered messages and 200 session entries. Trimming removes rendered nodes only and re-exposes "Load older messages"; the server stays the source of truth, so no history is lost.
- Delivery: `index.html` now requests `/app.js?v=<commit>` and `/style.css?v=<commit>`. Those two are served `public, max-age=31536000, immutable` with a build-scoped ETag and answer a matching `If-None-Match` with `304` and an empty body; `/` is `no-cache` so a cached document can never point at a retired build and trip the "Stale UI detected" guard. The `no-store` middleware now only fills in a missing `Cache-Control`, so every API response is still uncacheable — asserted by a new test.
- New tests in `src/main.rs`: `fingerprinted_assets_are_cacheable_and_revalidate_without_a_body`, `api_responses_are_still_never_stored`, `frontend_runs_one_visibility_aware_clock`, `frontend_bounds_long_lists`. `tests/recording_ui.cjs` now matches the stylesheet link by regex so the `?v=` fingerprint cannot silently skip its snapshot rewrite.
- Verified (exact `verify:`): `node --check static/app.js && scripts/verify_browser.sh` exit 0 (20 + 24 browser checks, no JavaScript exceptions). Then `scripts/verify_local.sh` exit 0 and strict `scripts/verify_release.sh` exit 0 (rust-tests, rust-clippy, rust-build, python-contracts, integration-smoke, recording-integration, javascript-syntax, mocked-browser, local-contract, real-browser-to-server).
- Gap, not a pass: response compression (gzip/brotli) is NOT implemented. It needs a crate that is absent from `Cargo.lock` and the local registry cache, and the sandbox cannot fetch it, so it is tracked as new task P11-T06 rather than claimed. No optional-panel work was needed — the initial path is already one script and one stylesheet.
- Status: P11-T05 `doing` -> `done`. Deployment recorded separately below.

## 2026-09-14T07:00:00Z · P11-T05 — frontend delivery and idle work started

- Baseline: `main` clean at `e52d0c9`, nothing unpushed, P11-T04 deployed (schema 10, readiness verified). `depends: P11-T01` is `done`, so P11-T05 is the highest-priority eligible task.
- Status: P11-T05 `todo` -> `doing`.
- Plan: consolidate the remaining unconditional 5s/idle timers behind visibility-aware scheduling, bound long conversation/session lists, serve `/app.js`, `/style.css` and `/index.html` with compression plus fingerprinted immutable caching and ETag/304 revalidation, and load optional panels only when first opened. UI contracts in `tests/ui_smoke.cjs` and `tests/recording_ui.cjs` must keep passing unchanged.
- Verification target (exact): `node --check static/app.js && scripts/verify_browser.sh`, then `scripts/verify_local.sh` and strict `scripts/verify_release.sh` before any commit or deploy.

## 2026-09-14T06:55:30Z · P11-T04 — deployed to the harness unit

- Pushed `184ddbf` to `origin main` (`34fe779..184ddbf`), then ran `scripts/deploy.sh` on the committed tree.
- Deploy evidence: `deployed 184ddbf to harness: pid 251951, release sha256 e8bad476aaf68129f60376de3edd5497f0703d2e4206708ea04c82708fd4b482, schema 10, readiness verified, API answering, non-object body refused with 400`.
- The live schema is now 10 and readiness reports maintenance timestamps; retention stays disabled until an owner enables a policy with `scripts/maintenance.py policy`.

## 2026-09-14T06:55:00Z · P11-T04 — retention, WAL and compaction maintenance verified; task complete

- Shipped append-only migration `010_retention_maintenance.sql` (user_version 9 -> 10): `retention_policies` (targets restricted to `generation_chunks` and `activity_events`, `keep_days >= 1`, disabled by default), `maintenance_runs` evidence table with an action allow-list, and `generation_events.compacted_chunks`.
- `src/storage.rs`: `retention_policies`, `set_retention_policy` (unknown target or `keep_days < 1` is refused), `compact_generation_chunks` (merges chunk rows of terminal-state turns only, keeps the earliest seq with a chunk count), `apply_retention` (enabled policies only, finished receipts only, terminal generation rows always preserved), and `maintenance` (WAL checkpoint TRUNCATE, optimize, ANALYZE, incremental vacuum reported as unavailable when auto_vacuum is off). Readiness now requires schema_version 10 and reports journal mode plus the last checkpoint/retention/compaction timestamps.
- Added `scripts/maintenance.py` (status/policy/retention/compact/checkpoint/run plus a `--check` self-test on a throwaway database) and updated the health assertion in `src/main.rs` to schema_version 10.
- Verification: exact gate `python3 tests/test_migrations.py && cargo test --locked retention` -> `migrations OK: 001 -> ... -> 010, user_version=10` and 3 passed / 0 failed; `bash scripts/verify_local.sh` -> exit 0 (66 python contract tests, integration-smoke, recording-integration, JS syntax, both mocked-browser suites); `bash scripts/verify_release.sh` -> exit 0 strict gate including real browser-to-server success/denial/crash-recovery/cancellation/unsafe-retry suites. No service restarted during verification.
- Status: P11-T04 `doing` -> `done`. Committed and pushed to `origin main`; deployment recorded separately below.

## 2026-09-14T06:45:00Z · P11-T04 — retention, WAL and compaction maintenance started

- Reviewed the committed baseline first: `main` clean at `34fe779`, nothing unpushed, P11-T03's exact gate reran green (`cargo test --locked storage` 14 passed / 0 failed; `python3 scripts/benchmark.py --check` reported indexed plans available) and `bash scripts/verify_local.sh` exited 0.
- Status: P11-T04 `todo` -> `doing`. Highest-priority eligible task; `depends: P11-T03` is `done`.
- Plan: append-only migration `010_retention_maintenance.sql` (user_version 9 -> 10) adding disabled-by-default `retention_policies`, `maintenance_runs` evidence and `generation_events.compacted_chunks`; retention/compaction/WAL-checkpoint/optimize/analyze/incremental-vacuum operations in `src/storage.rs`; new `scripts/maintenance.py` operator entry point. Receipts, provenance and user deletion semantics stay out of scope for deletion.
- Verification target (exact): `python3 tests/test_migrations.py && cargo test --locked retention`, then `scripts/verify_local.sh` and the strict `scripts/verify_release.sh`. No deployment until those pass on a committed tree.

## 2026-09-14T03:07:07Z · P14-T01 — independent continuation verified; task complete

- Resumed the dirty `p14-durable-cancellation` worktree at `9947c0b` and preserved all existing modified/untracked work. No commit, push, deploy, or production restart.
- Reviewed the current cancellation/retry state machine, recovery ordering, terminal transaction guard, provider/sub-agent/permission cancellation, process-group registration seal, retry admission, migration and UI/test contracts. The latest terminal-state/recovery and late-registration regressions are present and pass.
- Fresh verification: `cargo test --locked` -> 207 passed, 0 failed; `bash scripts/verify_release.sh` -> exit 0 with Rust/clippy/build, Python/HTTP/recording contracts, JavaScript, both mocked-browser suites, and real browser-to-server success/denial/crash-recovery/cancellation/safe-retry/unsafe-retry-refusal scenarios; `git diff --check` clean.
- Status: P14-T01 `doing` -> `done`. This is non-deploying verification only; publication remains owner-controlled because the worktree is still uncommitted and the branch has no upstream.

## 2026-09-13T17:04:09Z · P14-T01 — independent review resumed; durable cancellation fixes started

- Status: `needs-verify` -> `doing`. Resumed the existing dirty `p14-durable-cancellation` worktree at `9947c0b`; preserved all prior changes. No commit, push, deployment, or production restart.
- Completed independent read-only reviews of SHA-256-verified copies of all changed implementation/test files, migration 009, and adjacent recording/tool contracts. Both reviewers found accepted cancellation can lose to a later completion/failure transaction; recovery also strands cancellation intent as `process_restarted`.
- This increment fixes those durable terminal-state defects first, with fail-before/pass-after regression evidence. Process ownership/signalling, auxiliary provider waits, permission/retry and UI findings remain open; review is not release clearance.
- Verification target: focused cancellation tests, full native/HTTP/contracts, and exact browser/release gates only with a permitted browser runtime. Do not treat historical browser passes as current evidence.
- Next: reproduce the terminal-state/recovery defects, make cancellation win inside the terminal transaction, then record fresh results and remaining findings.

## 2026-09-13T15:46:39Z · P14-T01 — checkpoint disimpan; review kode ditunda

- User meminta update progress saja di repo untuk dilanjutkan nanti. Perubahan ini hanya dokumentasi; source tidak diubah dan tidak ada pengujian kode baru.
- P14-REVIEW-01 (child-provider cancellation race + post-response guard) dan P14-REVIEW-02 (late process registration seal) telah diimplementasikan di `src/agent_loop.rs` dan `src/processes.rs`; keduanya masih menunggu review independen.
- Hasil terakhir yang dilaporkan implementer: kedua regresi fail-before/pass-after; `cargo test --locked` 203 passed; E2E dan strict non-deploy release gate exit 0. Ini bukan hasil audit independen. Ledger task ID/log ada di checkpoint terbaru `docs/HANDOFF-P14-T01.md`.
- Status tetap **needs-verify, bukan done**. Review kode belum dijalankan; snapshot MCP mengalami timeout/perbedaan hitungan baris dan belum berhasil diverifikasi lengkap.
- Branch `p14-durable-cancellation`, HEAD `9947c0b7798c9d7d42b791b1b29df080043f137f`; worktree 16 modified + 3 untracked, tanpa staging. **Tidak commit/push/deploy/restart**; perubahan disimpan pada worktree remote.
- Lanjutkan dari handoff terbaru: verifikasi artefak lengkap dengan hash, audit seluruh cancellation/retry (termasuk bounded process seal/PID lifecycle), perbaiki jika perlu dan rerun gate sebelum clearance/publikasi.
- Entri di bawah bersifat historis; angka 201 berasal dari sebelum dua regresi terbaru.

## 2026-09-13 · P14-T01 — finalization BLOCKED: independent audit not run

- **Automated gates all green.** `scripts/verify_release.sh` -> **exit 0** (rust-tests 201 passed; clippy, build, python-contracts, integration-smoke, recording-integration, javascript-syntax, mocked-browser; real-browser-to-server E2E/DENIAL/CRASH-RECOVERY/CANCELLATION/UNSAFE-RETRY). Full ledger in `docs/HANDOFF-P14-T01.md`.
- **Independent verification did NOT run.** The verification sub-agent was rejected twice: the first packet was missing `<verification_packet>`, and the corrected body was rejected as *not valid JSON*; the tool returned `verification_packet_validation_failed_twice` and forbade further verifier attempts this request. This is an orchestration validation failure, not a code failure.
- **Status:** `docs/TASKS.md` P14-T01 `doing` -> `needs-verify` (**not done**). Push withheld; no commit and no push; branch `p14-durable-cancellation` still has no upstream; HEAD `9947c0b`.
- **Next session:** independently audit all cancellation/retry changes using **exactly one `verification_packet` with a valid JSON body**, rerun affected/strict gates on any fix or drift, then commit and push to `origin p14-durable-cancellation`. No production deployment.
- **Evidence references (remote MCP run_command task ids / logs):** setup `9d882be49907`; verify_local `6f0d5713b909` (rerun mocked-browser `/tmp/p14_vb2.log` exit 0); verify_e2e `851ceaba8d9f` (exit 1, defect found) then `8e1ce0ae5adb` (exit 0); verify_release `470cb50e9cd0` (exit 0). Logs under remote `/tmp/p14_*.log`.
- UTC stamp: 2026-09-13T13:54Z.

## 2026-09-13 · P14-T01 — strict release gate GREEN
## 2026-09-13 · P14-T01 — strict release gate GREEN

- **Strict non-deploying release gate:** `scripts/verify_release.sh` -> **exit 0**. local-contract (rust-tests 201 passed, clippy, build, python-contracts, integration-smoke, recording-integration, javascript-syntax, mocked-browser) and real-browser-to-server all PASS.
- **Real e2e scenarios passed:** `E2E`, `DENIAL`, `CRASH-RECOVERY`, `CANCELLATION` (Stop during an 8 s slow provider wait, then exactly one safe-boundary retry, no replayed side effect), `UNSAFE-RETRY` (completed side effect -> hard 409 refusal, no retry turn created).
- **Defect found and fixed by the e2e run:** a cancelled turn left its in-flight `model_call` step `running`; `agent_loop` now closes it as `interrupted` on every cancellation path (`finish_cancelled_step`).
- **No commit or push performed.** Branch `p14-durable-cancellation` still has no upstream; HEAD `9947c0b`. Handoff: `docs/HANDOFF-P14-T01.md` (session update appended).
- Task board: task 2 (implementation) completed; task 3 (independent verification) in_progress; task 4 (finalize/push) parent-owned.
- UTC stamp: 2026-09-13T13:51:19Z.

## 2026-09-13 · P14-T01 — mid-flight provider cancel, UI fix, real e2e coverage
## 2026-09-13 · P14-T01 — mid-flight provider cancel, UI fix, real e2e coverage

- **Mid-flight provider cancellation.** `agent_loop` now races every provider wait (streaming and tool-calling) against the durable cancel intent via `Ctx::race_cancellation` + `PROVIDER_CANCEL_POLL` (200 ms). On cancel the abandoned provider future is dropped, aborting the in-flight request instead of letting a stopped turn finish and commit. This closes the "cancel only before/after a provider call" gap.
- **UI correctness fix.** The composer **Stop** control was briefly disabled while `busy`, i.e. exactly during the turn it must cancel. Stop is now gated only on having a pending request; the already-running receipt poll observes the `interrupted` terminal state. No `resumeRecording()` re-entry (avoids the busy guard).
- **Real browser e2e coverage (not just mocked).** `tests/browser_e2e_failure.cjs` gains provider modes and two scenarios over the real axum + SQLite + filesystem stack: `cancellationThenSafeRetry` (Stop during an 8 s slow provider wait, then exactly one safe-boundary retry, asserting `run_controls` lineage and `safe_boundary_seq`) and `unsafeRetryIsRejected` (edit applies, provider then fails → retry is a 409 refusal and no retry turn/lineage is created). `configure()` gained a permission-mode argument.
- **Contract fixture fix.** `tests/test_recording_contracts.py` applies migration `009_run_cancellation.sql` so the receipt read's `run_controls` join prepares against the actual schema.
- **Evidence.** `scripts/verify_local.sh` (non-browser portion): rust-tests **201 passed**, rust-clippy PASS, rust-build PASS, python-contracts PASS, integration-smoke PASS, recording-integration PASS, javascript-syntax PASS, mocked-browser PASS (`stop_records_cancellation`, `safe_boundary_retry_posts_once`). Browser deps provisioned via `scripts/setup_browser_tests.sh` (exit 0). `scripts/verify_e2e.sh` in progress.
- UTC stamp: 2026-09-13T13:48:27Z.

## 2026-09-13 · P14-T01 durable cancellation — implementation increment
## 2026-09-13 · P14-T01 durable cancellation — implementation increment

- **Backend.** New `src/processes.rs` in-memory registry of live process groups per request. `run_capped_for` registers the foreground `bash`, edit-diagnostics and LSP-command process group under the request; `POST /chat/requests/{id}/cancel` commits durable intent and then terminates the live group. `agent_loop::run_task` now observes cancellation between provider calls and between sub-agent tool calls, records `subagent::Stop::Cancelled`, and finishes the delegation and its parent tool-call step as `interrupted` instead of a completed exploration.
- **UI.** `static/index.html` adds a composer **Stop** control; `static/app.js` adds `cancelPending()` and `retryFromBoundary()`, a per-turn **Retry from safe boundary** button on terminal `failed`/`interrupted` turns, and surfaces 409 refusal reasons (unsafe / busy / not terminal). CSP-safe DOM construction retained.
- **Tests.** `src/main.rs` adds five retry regression tests (admitted from a recorded non-mutating boundary with lineage; refused when a side-effecting tool completed; refused while the session has an unfinished turn; refused when not terminal; idempotent replay). `src/processes.rs` and `src/subagent.rs` gain unit tests; `tests/recording_ui.cjs` gains mocked Stop/Retry coverage.
- **Evidence so far.** `node --check` app.js and all `.cjs` OK; `python3 tests/test_migrations.py` OK (user_version=9); `git diff --check` clean. Full non-browser `scripts/verify_local.sh` in progress (first run caught a moved-value compile error in a new test, now fixed and re-running).
- UTC stamp: 2026-09-13T13:40Z.

## 2026-09-13 · P14-T01 durable cancellation and safe-boundary retry — checkpoint
## 2026-09-13 · P14-T01 durable cancellation and safe-boundary retry — checkpoint

- Task: P14-T01; lane: workflow; status `doing` (marked done only after independent review).
- Worktree: `p14-durable-cancellation` at `/root/development/harness-p14-cancellation`; HEAD `9947c0b`; no upstream configured yet; no commit or push performed.
- Durable handoff: `docs/HANDOFF-P14-T01.md` — worktree/branch, exact task scope, existing changes, pending backend audit/UI/testing, latest evidence, blockers, commands and next actions (no secrets).
- Preserved: schema v9 `run_controls` migration (untracked), cancel/retry endpoints and cooperative cancel checks; corrected `schema_version == 9` health assertion.
- Pending: sub-agent and live process-group cancellation, Stop/Cancel and server Retry UI, focused race/restart/permission/side-effect regressions, then migration/cargo/JS/`verify_release.sh` gates. `verify_e2e.sh` remains browser-blocked in this runtime.
- UTC stamp: 2026-09-13T13:35Z.

## 2026-09-13 · Notion AI via Local · P14-T04a started

- Task: P14-T04a; priority: high; blocker: release-blocker; lane: provider-cost-safety. Status: `todo` → `doing`.
- Worktree: `p14-cost-safety` at `/root/development/harness-p14-cost-safety`, based on `main` commit `8085a42`.
- Scope: add configurable per-turn and UTC-day provider request/token/cost ceilings, reject before dispatch, persist refusal/unknown-usage reasons, and never count unavailable cost as zero.
- Verification target: `cargo test --locked provider && python3 tests/recording_integration.py`, then the strict non-deploying release gate before integration.


## 2026-09-13 · Notion AI via Local · P12-T07a completed

- Task: P12-T07a; priority: high; blocker: release-blocker; lane: docs-baseline. Status: `doing` → `done`.
- Reconciled the three entry documents with the compiled baseline: P0–P10 are explicitly historical/completed; P11–P18 are active continuation whose exact status comes from `docs/TASKS.md`. Removed stale P1 pending labels, v2-only schema text, unmerged-UI text, ONNX wording, and incomplete tool/runtime/security/deployment descriptions.
- `README.md` now distinguishes capabilities from fresh evidence, local from strict verification, verification from promotion, cursor-idempotent replay from transport guarantees, and historical migration procedure from current production state.
- `AGENTS.md` now prioritizes release blockers, fail-closed evidence, separate schema-aware deployment, current tools/migrations, and the no-replay recording contract. `docs/PLAN.md` now separates current architecture from historical acceptance scope and active continuation.
- Verification: `git diff --check`, focused entry-document baseline assertions, and the strict non-deploying release gate passed on the task branch. The strict gate is rerun after integration; documentation-only work does not justify a production restart.
- Next release blocker by the current priority contract: P14-T04a fail-closed spend ceilings; P13-T03 remains dependency-unblocked and is also required for the daily-use boundary.


## 2026-09-13 · Notion AI via Local · P12-T07a started

- Task: P12-T07a; priority: high; blocker: release-blocker; lane: docs-baseline.
- Scope: reconcile `README.md`, `AGENTS.md`, and `docs/PLAN.md` with the compiled P0–P10 baseline and active P11–P18 backlog; label historical phase/evidence prose and remove stale pending claims.
- Evidence source: current routes, tool registry, migrations 001–007, verification scripts, and completed task metadata. Historical journal counts and deployment identities will not be recast as current evidence.


## 2026-09-13 · Notion AI via Local · P10-T06 completed

- Task: P10-T06; priority: high; lane: reliability-tests. Status: `doing` → `done`.
- Added a synthetic disposable-database fixture that kills helper processes before and after commit, reconciles a committed-but-unacknowledged admission, forces `SQLITE_FULL`, enforces URI read-only behavior, corrupts a WAL frame checksum and database page, saturates admission/extraction queues, and resets a running extraction job without losing work.
- Extended the real recording integration with a detached bash process: the tool step and verification become durable before restart, the child finishes its side effect exactly once, and neither provider calls nor the process are replayed.
- Verification: `python3 tests/fault_injection.py`, recording integration, `scripts/verify_e2e.sh`, denial/crash-recovery E2E, and the strict release gate all passed. All fixtures used temporary synthetic paths; production was not restarted.
- Next release blockers: P12-T07a baseline reconciliation and P14-T04a fail-closed spend limits. P13-T03 is now dependency-unblocked.


## 2026-09-13 · Notion AI via Local · P10-T06 started

- Task: P10-T06; priority: high; lane: reliability-tests; dependency P10-T02 is complete.
- Scope: add deterministic disposable-database crash, ENOSPC/read-only/I/O, WAL/checksum, integrity, ambiguous-admission, queue-backpressure, and detached-process lifecycle fixtures.
- Safety: fixtures use temporary paths and synthetic data only; they do not mutate or restart production. Exact verification remains `python3 tests/fault_injection.py && scripts/verify_e2e.sh`.


## 2026-09-13 · Notion AI via Local · P10-T05 completed

- Task: P10-T05; priority: high; blocker: release-blocker; lane: release.
- Status transition: `doing` → `done`.
- Runtime: SIGINT/SIGTERM stop HTTP admission, wake idle workers, and let claimed generation/extraction finish a durable boundary within a bounded shutdown window. Abrupt kill recovery remains no-replay.
- Deployment: snapshots the served executable and identity before building; verifies candidate commit/SHA-256/schema/workers/API; atomically restores the previous binary only for schema-compatible failures. Newer or unreadable schema stops the service and requires explicit owner-approved database recovery with backup age and post-backup writes reported.
- Verification: 11 focused recording tests passed; the recording integration proved SIGTERM drained an in-flight foreground tool and terminal receipt, restart caused no replay, and SIGKILL recovery still interrupted without duplicate side effects. The disposable rollback fixture proved compatible executable restoration and fail-closed newer/unknown-schema behavior.
- Full strict gate: all 187 Rust tests, 66 Python contracts, integration/recording smoke, JavaScript syntax, both mocked-browser suites, and real browser-to-server E2E including denial/crash recovery passed. No production service was restarted by verification.
- Next release blocker: P10-T06 fault injection, then P12-T07a baseline reconciliation and P14-T04a fail-closed spend limits.


## 2026-09-13 · Notion AI via Local · P10-T05 started

- Task: P10-T05; priority: high; blocker: release-blocker; lane: release.
- Worktree: `p10-rollback` at `/root/development/harness-p10-rollback`, based on deployed `main` commit `d61fbd4`.
- Status transition: `todo` → `doing`.
- Scope: drain accepted HTTP requests and claimed workers on SIGTERM, add bounded shutdown, preserve no-replay recovery, and make deployment rollback schema-aware with a disposable fixture rather than using production promotion as verification.
- Verification target: real E2E plus `tests/deploy_rollback.py`; production deployment remains a separate post-integration action.
- Open policy: binary rollback is automatic only while the upgraded database remains readable by the previous binary. A newer schema requires explicit database recovery with backup age and post-backup writes reported; no automatic database restore.


## 2026-09-13 · Notion AI via Local · P12-T05a completed

- Task: P12-T05a; priority: critical; blocker: release-blocker; lane: release-contract.
- Status transition: `doing` → `done`.
- Changes: split permissive local checks from strict release proof; made every suite emit RUN/PASS/FAIL/SKIPPED/BLOCKED; made release verification require mocked-browser and real browser-to-server E2E evidence without deployment; made deployment reject dirty source by default.
- Plan review integration: added a safe daily-use release boundary and blocker ordering; strengthened rollback/schema compatibility acceptance; split baseline docs, cost safety, CI, and provider scheduling into focused tasks; narrowed strict replay to deterministic integrity and first-divergence evidence.
- Verification: missing browser dependencies failed the strict gate with status 2 and an explicit BLOCKED result; shell syntax passed; the strict gate passed all 187 Rust tests, 66 Python contracts, integration/recording smoke, JavaScript syntax, both mocked-browser suites, and real browser→axum→SQLite→filesystem/provider E2E including denial and crash recovery. No service was restarted.
- Next release blockers: P10-T05 rollback/schema policy, P10-T06 fault injection, P12-T07a baseline reconciliation, P14-T04a cost ceilings, then P13-T03 tool boundaries and P14-T01 cancellation as their dependencies clear.


## 2026-09-13 · Notion AI via Local · P12-T05a started

- Task: P12-T05a; priority: critical; blocker: release-blocker; lane: release-contract.
- Worktree: `p12-release-contract` at `/root/development/harness-p12-release-contract`, based on deployed `main` commit `70aee88`.
- Status transition: `todo` → `doing`.
- Review decision: accepted the plan review's release-gate, rollback-policy, milestone-ordering, strict-replay, documentation, task-sizing, and daily-use-boundary findings. This task implements the smallest truthful release-contract slice and records narrower follow-ups.
- Verification target: shell syntax plus the strict non-deploying release gate, including mocked-browser and real browser-to-server E2E lanes.
- Blockers/open questions: production rollback semantics, baseline documentation cleanup, cancellation/tool policy, and minimal cost ceilings remain explicit release-blocker follow-ups.


## 2026-09-13 · Notion AI via Local · P13-T02 completed

- Task: P13-T02; priority: high; lane: privacy.
- Worktree/parallel slot: `p13-privacy` at `/root/development/harness-p13-privacy`.
- Status transition: `doing` → `done`.
- Changes: added migration 007, an opt-in versioned AES-256-GCM exact-original archive with current/previous external keys, append-only privacy events, distinct deletion state, and previous-key backup restore support.
- Verification: migration chain 001→007 passed; 3 archive tests passed; all 187 Rust tests passed; all 5 encrypted-backup tests passed; `git diff --check` passed.
- Blockers/open questions: implementation is unblocked. Production key placement, archive retention, and off-host copy policy remain owner deployment decisions; no real key was generated or stored.


## 2026-09-13 · Notion AI via Local · P13-T02 started

- Task: P13-T02; priority: high; lane: privacy.
- Worktree/parallel slot: `p13-privacy` at `/root/development/harness-p13-privacy`, based on deployed `main` commit `00d05c1`.
- Status transition: `todo` → `doing`.
- Scope: add an explicitly invoked AES-256-GCM exact-original archive with external current/previous keys, append-only privacy events, and distinct forget, source-delete, index-purge, and archive-delete state.
- Verification target: `python3 tests/test_migrations.py && cargo test --locked archive`, backup rotation tests, and `git diff --check`.
- Blockers/open questions: production key placement and off-host archive policy remain owner deployment choices; no real key will be generated or stored by this task.

## 2026-09-13 · Notion AI via Local · P13-T01 completed

- Task: P13-T01; priority: critical; lane: auth.
- Worktree/parallel slot: `p13-auth` at `/root/development/harness-p13-auth`.
- Status transition: `todo` → `doing` → `done`.
- Changes: added current/previous master-token rotation, delayed authentication failures, short-lived bounded browser sessions, per-route rate and body limits, explicit trusted-proxy identity, conditional HSTS, and memory-only browser credential handling.
- Verification: all 10 auth-focused tests, all 184 Rust tests, compiled integration smoke, JavaScript syntax, both mocked-browser suites, and `git diff --check` passed.
- Implementation commit: `af5808b` (`feat(auth): harden browser sessions and request boundaries`).
- Blockers/open questions: none.

## 2026-09-13 · Notion AI via Local · P13-T01 started

- Task: P13-T01; priority: critical; lane: auth.
- Worktree/parallel slot: `p13-auth` at `/root/development/harness-p13-auth`, based on deployed `main` commit `c576b19`.
- Status transition: `todo` → `doing`.
- Scope: add route-specific body/request-rate limits, uniform delayed authentication failure, restart-safe current/previous token rotation, short-lived in-memory browser sessions, explicit trusted-proxy identity configuration, and HTTPS/HSTS deployment guidance without credentials in URLs or browser storage.
- Verification target: `cargo test --locked auth && python3 tests/integration_smoke.py` plus frontend syntax and focused browser checks.
- Blockers/open questions: none; loopback-first remains authoritative, and proxy identity will be opt-in with explicit overwrite/strip requirements.

## 2026-09-13 · Notion AI via Local · P10-T03 completed

- Task: P10-T03; priority: critical; lane: runtime.
- Worktree/parallel slot: `p10-readiness` at `/root/development/harness-p10-readiness`.
- Status transition: `todo` → `doing` → `done`.
- Changes: embedded the source commit, calculated the running executable's SHA-256, added authenticated readiness with database/schema/queue and tracked worker state, exposed build identity in the UI with stale-client detection, and made deployment smoke fail closed on commit/hash/schema/worker/database mismatch.
- Verification: 3 focused health tests, all 177 Rust tests, all 65 Python tests, compiled recording integration, both mocked-browser suites, JavaScript/shell syntax checks, `git diff --check`, and the production deployment/readiness smoke passed.
- Deployment: implementation commit `1c4d699` started as PID 116606 with release SHA-256 `d3096e3e6b976bedc158cf4324a3aed6714cbacb267b017f19adab55f7a5dfd3`; commit, executable, schema 6, database, both workers, API, and request refusal were verified.
- Blockers/open questions: none for P10-T03; automatic rollback intentionally remains P10-T05.
- Next highest-priority eligible task: P13-T01, harden request authentication and abuse boundaries.

## 2026-09-13 · Notion AI via Local · P10-T02 completed

- Task: P10-T02; priority: critical; lane: runtime.
- Worktree/parallel slot: `p10-runtime` at `/root/development/harness-p10-runtime`.
- Status transition: `todo` → `doing` → `done`.
- Changes: added a kernel-backed per-database process lock acquired before SQLite startup recovery, owner-only diagnostic metadata, safe stale-file replacement, and live-process contention coverage.
- Verification: `cargo test --locked process_lock` passed 3 focused tests; `cargo build --locked && python3 tests/recording_integration.py` passed with a real contender process, unchanged live receipt/step, and subsequent crash recovery without replay.
- Files/commit: `src/process_lock.rs`, `src/main.rs`, `src/storage.rs`, `tests/recording_integration.py`, and task/progress journals; commit recorded with the task branch.
- Blockers/open questions: none for P10-T02; the lock is Unix-specific, matching the current Linux deployment contract.
- Next eligible task: P10-T03, expose readiness and deployed identity.

## 2026-09-13 · Notion AI via Local · P10-T04 completed

- Task: P10-T04; priority: critical; lane: backup.
- Worktree/parallel slot: `p10-backup` at `/root/development/harness-p10-backup`.
- Status transition: `todo` → `doing` → `done`.
- Changes: added encrypted rotating backups, external owner-only keys, atomic publication, authenticated clean restore drills, and fail-closed fault coverage.
- Verification: `python3 -m unittest discover -s tests -p 'test_*backup*.py'` passed 4 tests; broader Python contract verification recorded before integration.
- Files/commit: `scripts/backup.py`, `scripts/restore_test.py`, `tests/test_backup.py`, `docs/ARCHITECTURE.md`, README, and task/progress journals; commit recorded with the task branch.
- Blockers/open questions: production key placement and off-host copy policy remain an owner deployment decision; no real key was created or stored.
- Next eligible tasks: P13-T02 is dependency-unblocked after integration; P10-T02 remains the next critical runtime task.

## 2026-09-13 · Notion AI via Local · P10-T01 completed

- Task: P10-T01; priority: critical; lane: incident.
- Worktree/parallel slot: `p10-incident` at `/root/development/harness-p10-incident`.
- Status transition: `todo` → `doing` → `done`.
- Changes: added deterministic break-centered graph selection, closed all returned references, and exposed total/returned/omitted counts with opaque expansion anchors.
- Verification: `cargo test --locked storage` passed 11 tests; `cargo build --locked && python3 tests/recording_integration.py` passed the compiled-server suite.
- Files/commit: `src/storage.rs`, `tests/recording_integration.py`, `docs/design/causal-observability.md`, and task/progress journals; commit recorded with the task branch.
- Blockers/open questions: none for P10-T01. Repository-wide `cargo fmt --check` still reports pre-existing formatting debt outside this task's files and belongs to P12-T05's ratchet work.
- Next eligible tasks: P10-T02 is eligible; P10-T04 and P12-T05 remain isolated parallel candidates.

## 2026-09-13 · Notion AI via Local · P10–P18 roadmap accepted

- Converted the full optimization, feature, security, UX, memory, observability, and research review into the active P10–P18 roadmap and executable task backlog.
- Added explicit `priority`, `lane`, and `parallel` metadata. Parallel work requires completed dependencies, separate worktrees, disjoint lanes/files, serialized migrations/contracts, and integration verification after one-at-a-time merges.
- Updated `AGENTS.md`, `PLAN.md`, `ROADMAP.md`, `TASKS.md`, and README so roadmap coverage and progress journaling are mandatory parts of every task transition.
- Verification: docs-only structural checks (`git diff --check`, task-ID/dependency/metadata validation, and Markdown reference checks); no runtime behavior changed.
- Next: begin critical Wave A. `P10-T01`, `P10-T04`, and `P12-T05` are the first disjoint parallel set; `P10-T02` follows `P10-T01` integration because both currently declare `src/storage.rs` and `tests/recording_integration.py`.

## 2026-09-13 · Notion AI via Local · P9-T02 closed SQLite handles

- Registered deterministic cleanup for every Python test SQLite connection, including migration helpers and online backup endpoints.
- Replaced misleading SQLite transaction context usage with explicit closing contexts where ownership ends locally.
- Verified: `PYTHONWARNINGS=error::ResourceWarning python3 -m unittest discover -s tests -p 'test_*.py'` passed, 61 tests, with no resource-warning output.
- Next: run the complete release gate, push both review fixes, and redeploy.

## 2026-09-13 · Notion AI via Local · P9-T01 closed incident projection

- Fixed the bounded incident read model so truncation cannot leave dangling edge or adjacency references.
- Added explicit node/edge truncation metadata and a 401-node regression.
- Verified: `cargo test --locked storage` passed, 11 tests.
- Next: P9-T02, close Python SQLite test connections and make `ResourceWarning` fatal in its gate.

## 2026-09-12 · ClickUp Brain via Local MCP · P8-T04 incident graph UI

- Added an inline incident graph to the activity rail, not a modal: relation filtering, earliest-break summary, node selection, edge traversal, explicit known/unknown provenance, and durable row IDs.
- Step and mutation nodes jump back to the recorded step or file-change view; saved message receipts can reopen historical incidents.
- Verified at 13:00 WIB: `scripts/verify_browser.sh && node --check static/app.js` passed. `scripts/verify_e2e.sh` also passed against the real Rust server and Chromium after the UI change.
- P8 causal observability is now complete through its four executable tasks. Next work must be chosen from the remaining ordered backlog rather than inventing a P8 follow-up.

## 2026-09-12 · ClickUp Brain via Local MCP · P8-T03 failure attribution

- Added runtime provenance for permission dependencies and decisions, file mutations, stale-anchor contradictions, and restart recovery.
- Recovery events now carry the exact interrupted step and a durable `step triggers recovery` edge; receipt-level `process_restarted` remains the terminal summary, not the earliest break.
- Extended the real denial, stale-anchor, and SIGKILL fixtures to navigate `/chat/requests/{id}/incident`, distinguish contradiction from unknown provenance, and assert no duplicate side effect.
- Verified at 12:57 WIB: `scripts/verify_e2e.sh` passed with real Chromium, axum, SQLite, filesystem, and loopback provider. The full Rust suite also passed: 170 tests.
- Next: P8-T04, ship the interactive incident graph in the dashboard.

## 2026-09-12 · ClickUp Brain via Local MCP · P8-T02 incident read model

- Added the authenticated read-only `/chat/requests/{id}/incident` endpoint.
- The projection is bounded to 400 nodes and 2,000 edges, includes explicit upstream/downstream adjacency, returns earliest-known-break evidence, and marks unlinked rows as `unknown` rather than guessing support.
- Added the denial-path HTTP assertion to `tests/recording_integration.py`.
- Verified: `cargo build --locked && python3 tests/recording_integration.py && node --check static/app.js` passed.
- Next: P8-T03, prove denial, stale-anchor, and crash-recovery attribution with navigable graphs and no duplicate side effects.

## 2026-09-12 · ClickUp Brain via Local MCP · P8-T01 durable provenance edges

- Added append-only migration `006_provenance_edges.sql`: six durable node kinds, seven relations, a 2,000-edge request cap, endpoint validation, request/scope isolation, uniqueness, and delete guards that preserve row-backed references.
- Added storage validation plus write/read APIs without a freeform reasoning field, so the graph cannot become a hidden chain-of-thought store.
- Extended migration, SQL contract, and Rust storage tests across evidence, step, permission, mutation, memory, recovery, bad kinds/relations, missing rows, self-edges, and referenced-row deletion.
- Verified: `python3 tests/test_migrations.py && cargo test --locked storage` passed (001→006, 10 storage tests); `python3 -m unittest tests/test_agentic_sql.py` passed (11 tests).
- Next: P8-T02, build the bounded causal incident read model and earliest-known-break projection.

## 2026-09-12 · AI session · P8 causal observability direction

- Researched current agent observability gaps: outcome-only evaluation, weak span-level failure localization, memory failures whose cause predates the visible error, and missing cross-component provenance/recovery links.
- Updated `docs/PLAN.md` and `docs/ROADMAP.md` with P8, a bounded causal observability phase rather than another generic trace viewer.
- Added `docs/design/causal-observability.md` with the edge vocabulary, guardrails, first stale-memory/stale-anchor experiment, and success criteria.
- Added P8-T01 through P8-T04 to `docs/TASKS.md`. The next executable task is P8-T01: define durable provenance edges.
- Not verified: docs-only planning change; no schema or runtime code changed.

## 2026-09-11 · Notion AI via Local · P7-T05 design and executable publication spec

- Drafted `docs/design/incremental-publication.md`: today's whole-answer boundary, the constraint that actually blocks incremental publication, invariants I1–I5, the `safety::StreamRedactor` shape, the rejected alternative, storage/UI impact, and the test matrix.
- Grounding finding: `safety::redact` decides per line, replaces a matched line whole, and carries `-----BEGIN … PRIVATE KEY-----` state across lines. A pattern can therefore still be completed by later bytes of the same line (`pass` + `word=hidden`), and publication is durable and append-only, so a released partial line could never be retracted. The safe publication unit is a completed line, not a provider chunk and not a token.
- Recorded the rejection of intra-line masking: it would require changing `redact` from dropping a line to masking a span, which weakens a deliberately conservative module and needs its own task rather than arriving as a side effect of streaming work.
- Added `tests/test_incremental_publication.py` as the executable spec. It derives the marker vocabulary, the `sk-`/`AKIA` thresholds and the redaction marker from `src/safety.rs` so the reference cannot drift, then proves chunk-split equivalence, monotone-prefix publication, holdback until a line terminator, and tail discard on failure — over split-secret, private-key, CRLF, Unicode, token-shape, trailing-newline and no-newline fixtures, across every single cut, one-character chunks and seeded multi-cuts.
- Repointed `P7-T05`'s design link from the PLAN anchor to the new doc, added the spec to its file list, appended a design-ready note, and refreshed the PLAN P7 paragraph.
- Verified here: `python3 tests/test_incremental_publication.py` (7 tests, OK) and `python3 -m unittest discover -s tests -p 'test_*.py'` (60 tests, OK, up from 53), with migrations `001 -> 002 -> 003 -> 004 -> 005` and tool schemas 13 files / 17798 bytes still reported by the suite.
- Not verified here: no `cargo`, so `safety::StreamRedactor` is designed and specified but not implemented or gated; `npm`, `npx` and the `playwright` module are still absent, so `P7-T06` stays blocked.
- Next: on a cargo host, implement `StreamRedactor` and the per-publication `chunk` row against this spec, then run `cargo test --locked streaming && cargo test --locked redact && bash scripts/verify_release.sh`.

## 2026-09-11 · Notion AI via Local · P7 phase closure and remaining-work split

- **Resumed and re-verified the checkpoint before changing anything**: `/root/development/harness` on `p7-frontend-generation-stream`, clean tree, HEAD `b6a5fb1` "feat(ui): render durable generation events", and `git rev-list --left-right --count` against `origin/p7-frontend-generation-stream` reporting `0 0`.
- **Closed the umbrellas**: `P7-T01` and `P7-T02` were still `doing` even though every subtask beneath them (`P7-T02a`, `P7-T02b`, `P7-T02c`, `P7-T04`, `P7-T03`) is `done`. Both are now `done` with notes that map each `done-when` clause to the subtask that satisfied it and cite the `b6a5fb1` gate run (161 Rust tests, Clippy/build, 53 Python contracts, migration chain through 005, both local HTTP suites, frontend syntax) as their verification evidence. No code changed, so no new Rust result is claimed.
- **Scheduled what is actually left**: the two open items are now tasks instead of prose. `P7-T05` (todo) owns safe incremental publication and inherits the `P7-T02a` decision that whole-answer buffering holds until a boundary-aware redactor proves a secret split across provider chunks is never published early. `P7-T06` (blocked) owns making the browser suites executable and records the exact owner commands.
- **PLAN refreshed**: the P7 section no longer prescribes the finished `P7-T02c` → `P7-T04` → `P7-T03` order; it states what landed, that the umbrellas are closed on that evidence, and names the two remaining threads.
- **Verified here (docs-only change)**: `python3 -m unittest discover -s tests -p 'test_*.py'` 53 tests OK, `python3 tests/test_migrations.py` reporting `001 -> 002 -> 003 -> 004 -> 005, user_version=5, data/FTS/FKs preserved`, `python3 tests/recording_integration.py` PASS, `python3 scripts/gen_tool_schemas.py` 13 files, `node --check` on `static/app.js`, `tests/recording_ui.cjs` and `tests/ui_smoke.cjs`, and `git diff --check`.
- **Not verified here**: this host has `node`, `python3` and `git` but no `cargo`, `rustc`, `npm`, `npx` or `playwright` module, so `bash scripts/verify_release.sh` exits 2 (`BLOCKED: Rust/cargo required`) and the browser fixtures cannot run. `require('playwright')` returns `MODULE_NOT_FOUND`, which confirms the reported Playwright limitation is a host gap rather than a defect.
- **Next**: `P7-T05` is the next implementation task and needs the Rust toolchain, so it must run on a cargo-capable host. `P7-T06` needs an owner decision about installing a browser runtime.

## 2026-09-11 · Notion AI via locally · P7-T03 durable generation UI

- **Merged baseline**: fast-forwarded the completed P7 replay-attribution and migration-reporting commits into `main` at `c7e35bf` and pushed `origin/main` before starting the UI work; no conflicts or history rewrite were needed.
- **Durable answer path**: the chat subscribes with authenticated `fetch` to `/generation/stream`, parses split UTF-8/SSE frames, advances only on increasing database sequence IDs, stores the cursor with the pending request, and resumes from it after reload. `/generation` is the fallback when streaming is unavailable.
- **Visible states**: whole answers appear atomically from persisted `complete` events rather than the receipt response or a typewriter effect. Generating, failed, and interrupted states have explicit text-only cards, and reopened failed/interrupted history keeps the same distinction.
- **Coverage**: the mocked browser fixture now serves generation polling/SSE rows and asserts durable complete content, persisted cursor resume, authenticated generation subscription, and failed/interrupted cards. All content is assigned through `textContent`.
- **Verification**: `node --check static/app.js`, `node --check tests/recording_ui.cjs`, `git diff --check`, and `bash scripts/verify_release.sh` passed: 161 Rust tests, Clippy/build, migration chain through 005, 53 Python contracts, schemas, both local HTTP suites, and frontend syntax. The Playwright browser fixture could not execute because this checkout has no `playwright` module; its syntax passed and the release gate does not include browser suites.

## 2026-09-11 · Notion AI via locally · P7-T04 truthful migration reporting

- **Fixed**: `tests/test_migrations.py` derives both its displayed migration sequence and expected latest `user_version` from `CHAIN`, eliminating the stale hard-coded `004` / version 4 success line.
- **Correction**: the earlier P7-T02a journal note saying Python migration coverage stopped at 004 was inaccurate. The test already applied and asserted migration 005; only its printed summary was stale. Historical P6 entries describing 001→004 remain accurate for their dates.
- **Verification**: `python3 tests/test_migrations.py` exits 0 and now reports `001 -> 002 -> 003 -> 004 -> 005, user_version=5, data/FTS/FKs preserved`; `git diff --check` passes.
- **Next**: `P7-T03` is unblocked for frontend durable generation-feed integration.

## 2026-09-11 · Notion AI via locally · P7-T02c attributed generation replay

- **Fixed**: generation polling and SSE projections now preserve each durable event's `request_id`, so multiple turns in one session can be rendered under the correct message.
- **Replay coverage**: native tests prove ordered two-turn attribution, bounded cursor paging, exact tails, wrong-session isolation, completed events, and interrupted recovery attribution. The compiled-server suite compares authenticated SSE frames with polling rows and proves cursor resume has no duplicates.
- **Failure coverage**: the HTTP fixture now verifies interrupted and provider-failed generation rows carry the originating request ID and terminal error code. Invalid cursors, malformed sessions, and unauthenticated polling/streaming remain rejected.
- **Verification**: `cargo test --locked streaming` passed (9 tests), `cargo test --locked generation` passed, and `bash scripts/verify_release.sh` exited 0 with 161 Rust tests, Clippy/build, 53 Python contracts, both local HTTP suites, frontend syntax, and `git diff --check`. Browser suites were not run because no UI changed.
- **Review notes**: existing Rust dead-code warnings and Python SQLite `ResourceWarning`s remain non-blocking. The release gate's stale migration summary remains isolated as `P7-T04`, which is next before frontend integration in `P7-T03`.

## 2026-09-11 · Notion AI via Local · Fast-forward merge and P7 continuation plan

- **Merged**: fast-forwarded `autonomous-development-streaming` into `main` at `554fa6f` and pushed `origin/main`; no conflict resolution or history rewrite was needed.
- **Baseline**: the merged checkpoint had a clean tree and passed the full release gate before merge: 160 Rust tests, Clippy/build, 53 Python contracts, both local HTTP suites, frontend syntax, and `git diff --check`.
- **Next task**: `P7-T02c` is the next executable correctness task. It now explicitly depends on `P7-T02b` and names the storage, HTTP, native test, and integration-test files it owns.
- **Execution order**: finish attributed/resumable multi-turn generation replay (`P7-T02c`), correct migration reporting (`P7-T04`), then integrate the durable feed into the frontend (`P7-T03`). The frontend task depends on both backend replay and truthful migration-gate evidence.
- **Safety decision**: whole-answer buffering remains the active boundary. Incremental publication stays deferred until redaction can prove that secrets split across chunks never become visible early.

## 2026-09-11 · Notion AI via Local · P7-T02b idempotent generation recovery

- **Reviewed**: resumed the in-progress recovery fix on `autonomous-development-streaming`, inspected its existing diff, recovery ordering, recording SQL, generation projection, pending P7 tasks, and release gates without overwriting unrelated work.
- **Fixed**: `recover` now writes a generation `interrupted` event only for receipts that are still `generating`, before the receipt-state transition. Historical terminal receipts therefore cannot gain duplicate events or advance replay cursors on later startups.
- **Regression coverage**: the restart test captures the first generation feed, runs recovery again, proves byte-for-byte identical replay, and proves an unclaimed receipt in another session remains event-free and `captured`.
- **Verification**: `cargo test --locked restart` passed (2 tests), `cargo test --locked streaming` passed (9 tests), and `bash scripts/verify_release.sh` exited 0 (160 Rust tests, Clippy/build, migrations/contracts, 53 Python tests, both local HTTP suites, frontend syntax). `git diff --check` passed. Browser suites were not run because no UI changed.
- **Review notes**: the gate still reports three pre-existing Rust dead-code warnings and Python SQLite `ResourceWarning`s. These are non-blocking cleanup candidates. P7-T02c remains the next focused correctness task; P7-T04 already tracks the stale migration summary.

## 2026-09-10 · Codex via @local · P7-T02a provider boundary repair

- **Located/resumed**: `/root/development/harness`; clean inherited `main` at `227bd7e`; work is on `autonomous-development-streaming`.
- **Root causes reproduced**: role-only deltas stopped consumption; CRLF/multiline data disappeared; arbitrary network chunks used lossy UTF-8; incomplete/error streams succeeded; split secrets reached the sink; the text fallback wrote its answer twice; completion events were outside the receipt transaction.
- **Changes**: bounded byte-level SSE framing, strict UTF-8, content/body limits, role/comment/usage handling, explicit DONE and HTTP/error checks. Answers are buffered and redacted as a whole before sink delivery. Removed the unbounded asynchronous writer; one answer chunk and completed event now commit with the answer receipt. Restored the missing test attribute on the provider tool-choice contract.
- **Decision**: whole-answer buffering is the smallest reversible safety repair. This does not deliver incremental display. P7-T01/P7-T02 remain open rather than inheriting an unsupported completion claim.
- **Verification**: eight focused streaming tests and the fallback duplicate assertion pass. Final release gate exits 0: 160 Rust tests, Clippy/build, 53 Python tests, both HTTP integration suites, and frontend syntax. `git diff --check` passes. Existing warnings remain; browser suites were not run because no UI changed.
- **Environment failures resolved**: Cargo was installed outside PATH. Installed missing Clippy/rustfmt and Node.js to run the existing release gate; no live application or database was started or modified by this task.
- **Next**: generation transport/reconnect tests, request IDs in session generation rows, and restart-event deduplication; then safe incremental publication and P7-T03 frontend integration. Existing Python migration coverage stops at 004, while runtime migration 005 is exercised by the native DbStore tests.

## 2026-09-11 · AI session · P7-T02 runtime streaming integration

- Added text-only provider streaming path through `stream_turn` and a persistence forwarding sink.
- Tool-call turns continue using the existing completion loop.
- Verified: cargo test suite passes after runtime integration.
- Open: final release verification and commit.

## 2026-09-10 · AI session · P7-T02 stream boundary foundation

- **Delivered**: added provider SSE boundary parsing primitives for durable generation streaming.
- **Delivered**: added stream request builder contract and tests without changing the existing completion path.
- **Verified**: stream frame parsing now separates complete SSE events from partial network chunks.
- **Open**: wire the streaming HTTP response into `GenerationSink`, then persist incremental generation deltas.

Append-only. Newest entry first. Each entry: date, who (human / AI session), what changed,
## 2026-09-11 · AI session · P7-T02 provider stream response consumer

- Added the persistence bridge foundation for incremental generation streams.
- `GenerationEventWriter` now provides an async path from streaming code into `generation_events` using the existing `DbStore::append_generation` API.
- Next: wire stream deltas into this writer and add replay ordering tests.

## 2026-09-11 · AI session · P7-T02 async sink bridge

- Added a channel-based sink adapter between synchronous provider callbacks and async persistence code.
- Next: connect the channel consumer to ordered `generation_events` writes and terminal state handling.

- **Delivered**: Added the provider streaming response consumer boundary. `reqwest::Response` chunks are incrementally buffered, SSE events are extracted, provider deltas are decoded, and validated content is forwarded into `GenerationSink`.
- **Safety**: Invalid stream frames now fail through the sink instead of silently corrupting generation state.
- **Next**: Wire the streaming consumer into the generation persistence transaction so each provider delta becomes a durable `generation_events` record.
what was verified and how, what is open. Keep entries short; details go in TASKS.md status
and the design docs.

## How to resume (for an AI or a human)

1. Read `AGENTS.md`, then `docs/PLAN.md` (why), `docs/TASKS.md` (what, in order), this file (where we are).
2. Check the **Open questions** below. If any block the next task, ask the owner before coding.
3. Take the first `todo` task whose dependencies are `done`. Set it to `doing`. Read its `design:` section.
4. Implement. Run its `verify:` command. If you cannot run `cargo`, set `needs-verify` and say so in your entry.
5. Update TASKS.md status and append an entry here in the same change.

Suggested first message to an AI continuing this work:

> Read AGENTS.md, docs/PLAN.md, docs/TASKS.md and docs/PROGRESS.md in this repo. Tell me the current phase, the next task by ID, and whether anything in "Open questions" blocks it. Then implement that task following its design section and verify command. Do not skip the P0 gate result.

---

## 2026-09-10 · AI session (Notion AI via Local) · P6-T02 LSP diagnostics, references and rename

- **Delivered**: `lsp` is the twelfth registered tool. It selects `rust-analyzer` or `clangd` from the target extension, runs one fresh bounded stdio JSON-RPC session, converts 1-based Unicode positions to LSP UTF-16 positions, and returns capped diagnostics or reference locations with current whole-file hashes.
- **Approval-safe rename**: only `operation=rename` is side-effecting per call. The model supplies current hashes for every possible file; planning emits a capped combined multi-file diff while disk and `/changes` stay untouched, and the approved call starts a new server and rechecks all hashes before any write.
- **Bounded and recoverable**: 4 MiB frames, 2 MiB files, 20 files, 200 edits, 100 diagnostics/references, a 20-second default / 60-second maximum deadline, no model-controlled process command, no outside-root/non-file/new/resource paths, no overlapping edits, and rollback of earlier atomic writes if a later write fails. Each successful file becomes its own ordinary durable artifact.
- **Integration and guidance**: the schema generator, registry/order assertion, main-agent prompt and tool contract now include `lsp`. A deterministic fake stdio server drives the real HTTP fixture: read-only diagnostics/references bypass approval, and a reviewed two-file rename produces two applied/revertable change rows and two activity events.
- **Verification**: focused LSP suite 7/7, schema validation, Python compilation, direct HTTP integration, Clippy/build and `git diff --check` passed. Final release gate exited 0: 143 Rust tests, migrations 001→004, 12 tool schemas, 53 Python contracts and both mock-provider HTTP suites. Browser suites were not run because no UI changed.
- **Open**: the active Rust toolchain has only a `rust-analyzer` shim and reports the component missing, so live Rust calls currently return actionable `lsp_unavailable`; deterministic coverage requires no machine server and `/usr/bin/clangd` is available. Next is P6-T03 (`browser` via CDP).

## 2026-09-10 · AI session (Notion AI via Local) · P6-T01 ast_edit via ast-grep

- **Delivered**: `ast_edit` is the eleventh registered tool. It structurally rewrites every matching site in one existing Rust file with in-process ast-grep, then uses the same diff, atomic-write, diagnostics and durable change-artifact path as `edit`.
- **Approval-safe**: calls require the eight-hex `content_hash` from the latest `read`. Planning puts a bounded unified diff, before/after hashes and +/− counts in the pending permission while disk and the changes feed stay untouched; the approved run checks the hash again before writing, preventing approval-time diff drift.
- **Bounded refusals**: Rust only, 512 KiB per file, 20 matches by default and 200 maximum. Stale content, zero matches, over-broad patterns, unsupported files and invalid patterns all fail without writing.
- **Integration and guidance**: schema generation, the registry-order assertion, tool contract and main-agent prompt now include `ast_edit`. The HTTP fixture proves `read → pending approval → approved structural rewrite → applied/revertable file_changes` across two differently formatted matches.
- **Verification**: focused `ast_edit` suite 6/6, schema validation, Python compilation, direct HTTP integration and `git diff --check` passed. Final release gate exited 0: 136 Rust tests, Clippy/build, migrations 001→004, 11 tool schemas, 53 Python contracts and both mock-provider HTTP suites. Browser suites were not run because no UI changed.
- **Open**: nothing blocking. Next is P6-T02 (`lsp` diagnostics, references and rename).

## 2026-09-10 · AI session (Notion AI via Local) · P5-T03 task tool (read-only explore sub-agent)

- **Delivered**: The model can delegate exploration. `task` is registered like any other tool (tenth in the array), but the loop intercepts the call before `Registry::invoke`, because a sub-agent needs the provider while `Tool::run` is synchronous and filesystem-bound.
- **One request, one ordered list, still a tree**: the parent opens a `subagent` step whose `parent_step_id` is the `task` tool-call step, and the sub-agent's own model and tool calls hang off that. `STEP_BEGIN` carries the parent id at `?3` and `begin_child_step` is the only way to set it. No migration: schema 003 already had the column and the `subagent` kind.
- **Read-only by construction, not by permission**: only `read`, `grep` and `glob` are offered, the allow-list is re-checked when the model's call returns, and anything else is refused as `unknown_tool` without reaching the registry — so no approval can be raised inside a delegation even in `auto_all`. `TOOLS` excludes `task`, so a sub-agent cannot spawn one.
- **Bounded and shared**: a fresh context (own system prompt plus one exploration message, never the parent's history), ≤ 8 model calls, and the parent's remaining steps, tool bytes and wall deadline; what it spent is added back to the parent's counters. The parent sees a capped report (1000-char summary + ≤ 12 paths + why it stopped), never the sub-agent's transcript.
- **Deviations**: orchestration lives in `agent_loop` rather than `src/subagent.rs`, which is contract-only (tools, bounds, message and report shapes), because only the loop owns the provider, step writer and budgets; the planned `bounded_summary` helper collapsed into `report_content`.
- **Verification**: release gate exit 0 — 130 Rust tests (9 new), Clippy/build, migrations 001→004, 10 tool schemas, 53 Python contracts, both mock-provider HTTP suites. `recording_integration.py` gained a delegation leg asserting the step tree, the read-only tool array, the report shape and that a refused `write` left `deny.md` untouched. `git diff --check` clean. Browser suites not run: no UI change.
- **Open**: nothing blocking. Next is P6-T01 (`ast_edit` via ast-grep), which opens P6.

## 2026-09-10 · AI session (Notion AI via Local) · P5-T02 skills with progressive disclosure

- **Delivered**: A project can keep reusable procedures in `skills/<name>/SKILL.md`. Each turn's window lists only name, one-line description and path; the model calls the new `skill` tool to pull one body on demand, capped at 16 KiB and cut on a UTF-8 boundary after frontmatter is stripped.
- **Bounded and sandboxed**: 32 skills, 200-char redacted descriptions, 64-char single-segment names, 256 KiB per file, real directories only (no symlink traversal), with `paths::resolve` still gating the read. Skipped and over-cap directories are counted in the index instead of vanishing; a failed scan degrades to one `skills:not_indexed` line.
- **Untrusted by construction**: a body arrives as a tool result behind a banner saying project text cannot grant tool permissions, approve a denied command or override system rules.
- **Deviation from the task wording**: discovery runs per turn before the first provider call, not at process startup, because a scope's root path is configurable at runtime.
- **Two magic numbers removed**: `src/agent_loop.rs` and `tests/recording_integration.py` both asserted exactly eight tools; they now count the registry's schemas and `tools/schemas/*.json`.
- **Verification**: release gate exit 0 — 121 Rust tests (9 new), Clippy/build, migrations 001→004, 9 tool schemas, 52 Python contracts, both mock-provider HTTP suites. `recording_integration.py` gained a `skills/review` fixture proving the index reaches the window and the body does not. `git diff --check` clean. Browser suites not run: no UI change.
- **Open**: this repo ships no `skills/` directory, so the feature stays inert here until one exists. Next is P5-T03 (read-only explore sub-agent).

## 2026-09-10 · AI session (Notion AI via Local) · P5-T01 verifier step

- **Delivered**: Every text answer is now audited by a separate `verification` step that runs after the main model call and before `answer_saved`. The verifier sees the redacted answer and an evidence manifest of this request's durable tool steps only, and returns claims marked verified / unverified / skipped with the step ids they rest on.
- **Advisory by construction**: the answer is written first and never rewritten. Strict JSON parsing rejects unknown fields, oversized reports and any "verified" claim citing a step outside the turn; a provider or parse failure records `verification_failed` / `unavailable` on the verification step alone. Input and projection are bounded (24 steps, 12,000 answer chars, 20 claims, 8 evidence ids, 10 diagnostics, 500 chars per projected field).
- **UI**: a header badge derived only from the persisted step shows Verified, "N unverified", skipped or unavailable, with claim reasons in its tooltip; claim text is inserted as text, never markup. Settings gained a `verification` model role that falls back to the turn model and is cleared by lock.
- **Fixture correction**: `tests/mock_provider.py` and `tests/integration_smoke.py` assumed the last provider call of a turn was the answer, which the audit call broke. Both now route marker-carrying calls to a verification reply; the release gate caught this, not the browser suites.
- **Verification**: release gate passed (exit 0): 113 Rust tests, Clippy/build, migrations 001→004, 8 tool schemas, 52 Python contracts, and both mock-provider HTTP suites including new verification step, projection and event-order assertions. Browser suites run separately with a local Playwright and Chrome: 24 ambient-UI checks (3 new) and 15 recording checks. `git diff --check` clean.
- **Open**: nothing blocking; next is P5-T02 (skills with progressive disclosure). Note that `scripts/verify_release.sh` still does not run the browser suites, so they must be run by hand.

## 2026-09-10 · AI session (Notion AI via Local) · P3-T02–P4-T04 context and ambient memory

- **Delivered**: Tool-result compaction with a stable three-call boundary, unchanged-read references with full durable audit output, 70%-token turn compaction with exact receipts and review-only episodic candidates, and deterministic per-scope repository maps capped at 8 KiB.
- **Memory**: Added schema 004 with three new memory kinds and embedding storage. Hybrid recall now unions FTS5 and bundled deterministic offline vectors, preserves project shadowing, uses recency/usefulness in reranking, and retains the 6,000-byte output ceiling. Extraction separates untrusted plan context from exact user evidence and gives explicit corrections high priority.
- **Ambient UI**: Chat suggestions now appear beneath their source turn with Save/Edit/Dismiss; the Inbox is import-only. Candidate edits preserve evidence and expected revision, hostile text remains inert, and the real HTTP smoke suite covers filtering and editing.
- **Verification**: Release gate passed: 107 Rust tests, Clippy/build, migration 001→004, 8 tool schemas, 51 Python contracts, and both mock-provider HTTP suites. Both browser suites passed (21 ambient-UI checks and 15 recording checks); JavaScript syntax and `git diff --check` passed.
- **Delivery**: implementation commit `4fbc037` (`P3/P4: ship context management and ambient memory`).

## 2026-09-10 · AI session (Notion AI via Local) · P3-T01 deterministic context manager

**Why.** The first provider window had one coarse 24 KB history trim, embedded memory and plan text inside the prompt, and no receipt explaining what fit. Tool schemas were regenerated later by the loop, so the immutable receipt could not prove the definitions on the first call.

**Changed.**
- `docs/design/context.md`, `src/context.rs` — defined and implemented nine stable categories with independent source-byte budgets totalling 95,872 bytes. Every successful build emits a fixed-order ledger with budget/candidate/included/excluded byte counts and explicit part IDs; no category borrows from another. Required system rules/current message/registry fail closed, optional structured parts stay whole, and recent chat history keeps the newest complete turn suffix. UTF-8 and overflow behavior are unit-tested.
- `src/recording.rs`, `src/agent_loop.rs`, `prompts/main_agent.md` — both project and chat-only turns now use the same builder. The write-once format-v2 receipt stores exact first-call `provider_messages` and `provider_tools`, only memories that actually reached the window, and the category receipt before any provider call. The loop receives that exact tool array instead of regenerating it. Synthetic skills/map/memory/plan/summary material is one explicitly untrusted reference message; the current user message remains last and exact.
- `tests/recording_integration.py`, `docs/RECORDING_PROTOCOL.md` — the real HTTP gate proves stored messages and full tool definitions equal the first provider request and validates all nine ledgers. The old claim-time 24 KB trim is gone so exclusions inside the existing 20-message source bound are auditable.

**Verified.** `cargo test --locked context` → 7 passed; `cargo test --locked` → 93 passed; `python3 -m py_compile tests/recording_integration.py` → OK; `git diff --check` → clean; `bash scripts/verify_release.sh` → exit 0 (93 Rust tests, Clippy/release build, migrations 001→003, 8 tool schemas, 51 Python contracts, both local mock-provider HTTP suites). Log: `/tmp/verify-p3t01.log`. Browser suites were not rerun because no UI asset or browser behavior changed.

**Open / next.** The typed `skills_index`, `repo_map`, and `compacted_history` sources remain empty until P5-T02, P3-T04, and P3-T03. Current-turn tool bodies are still appended verbatim after the immutable first window; P3-T02 is next and owns old-result compaction plus the unchanged-file read cache.

## 2026-09-10 · AI session (Notion AI via Local) · P2-T03 create-revert coverage follow-up

**Why.** A post-ship audit found that the modify/revert path was proved end to end, but undoing a file the turn created had never crossed the real HTTP handler, and the browser mock never returned its distinct `status="deleted"` result. That was a small but real first-use risk.

**Changed.**
- `src/main.rs` — extended the existing revert integration test with an `action="create"`, `before_hash=NULL` change. It proves the card is initially revertable, the handler returns `status="deleted"`, the file is absent rather than zero-byte, a second request is 409, exactly one `file_reverted` event exists for that change, and the relisted card says Already reverted.
- `tests/ui_smoke.cjs` — added a second create card and mock result so the browser clicks both kinds of Revert, proves the create-specific "file this turn created was removed" notice, and stops offering the action afterward. The suite now reports 17 checks.
- `docs/TASKS.md` and the handler comment — clarified that `action="delete"` restoration is forward-compatible only. No current tool emits delete rows, so that branch is not described as a shipped or end-to-end-covered user path. The browser API remains mocked; the Rust test is what proves server behavior.

**Verified.** `cargo test --locked changes` → 5 passed; `node --check static/app.js` → OK; ui_smoke → 17/17 including `diff_card_create_revert`; `git diff --check` → clean; `bash scripts/verify_release.sh` → exit 0 (89 Rust tests, Clippy/build, migrations 001→003, 8 schemas, 51 Python contracts, both HTTP suites). Final gate log: `/tmp/verify-p2t03-followup-final.log`.

**Open.** The filesystem restore and `reverted_at` bookkeeping still cannot be one atomic operation; a database failure after the file write can leave a stale card. `/changes` also re-hashes each changed file on rail refresh. Both are deferred follow-ups rather than hidden inside this coverage-only correction.

## 2026-09-09 · AI session (Notion AI via Local) · P2-T03 diff cards with undo

**Why.** The rail could say a file changed but not show what changed, and there was no way back. `file_changes` has carried `before_hash`, `after_hash` and the unified diff since P1-T14, so the data for both a card and an undo was already recorded — only the read side and one endpoint were missing.

**Changed.**
- `src/tools/textdiff.rs` — `reverse(after, diff)` rebuilds pre-edit text by reverse-applying a recorded diff. It refuses a diff carrying the `[diff truncated]` marker and verifies every hunk's after-side against the file it was handed, so a stale diff cannot invent content.
- `src/agentic_sql.rs`, `src/storage.rs` — `FILE_CHANGE_GET` reads one change joined to its turn's scope and session, so a revert cannot be aimed at another project. `record_revert` sets `reverted_at` only `WHERE reverted_at IS NULL` (single-shot) and appends a `file_reverted` activity event in the same immediate transaction. `activity_events.kind` is open-vocabulary, so no migration was needed.
- `src/main.rs` — `GET /changes` enriches each row with `revertable` and a `revert_note` by hashing the file on disk in `spawn_blocking`. `POST /changes/{id}/revert` restores the previous content behind two proofs — the file must still hash to `after_hash`, and the rebuilt text must hash to `before_hash` — writing through the existing `edit_tools::atomic_write` (now `pub(crate)`). Refusals are 409 with the reason and touch nothing: already reverted, file changed since, no project root, no recorded previous content. A revert of a file the turn created deletes it again.
- `static/*` — the rail grew a "File changes" panel below the steps: `path · +A −B · applied HH:MM`, the diff coloured per line by CSS class on spans built from `textContent` (CSP is `style-src 'self'`, and this also keeps a hostile diff inert), then Revert or the server's muted note. A revert refreshes the turn, so the card flips to "Already reverted" from the server's own state rather than a local guess.
- `tests/*` — ui_smoke renders a card from an XSS-laden diff, asserts the markup stays text, then clicks Revert and waits for the footer to change; the SQL contracts cover the revert roundtrip and its single-shot update.

**Scope call.** The ticket said "accept/reject". A row only exists after `agent_loop::finish_step` wrote it with `applied=1`, so there is nothing left to accept; the card offers Revert alone instead of a button pretending to gate an edit that already landed.

**Verified.** `cargo test --locked` 89 passed (4 new); `node --check static/app.js`; ui_smoke 16/16 including `diff_card_rendered` and `diff_card_revert`; recording_ui 15/15; `tests/test_agentic_sql.py` 8/8; `bash scripts/verify_release.sh` exit 0 (clippy, build, migrations, 51 Python contracts, both HTTP suites). One self-inflicted failure on the way: a first unit test asserted `reverse` would refuse a file with a foreign line appended after the hunk. It cannot — no diff describes lines it never touched — so the test now asserts the honest property and documents that the endpoint's two hash checks are what catch that case.

**Open.** Revert has no keyboard shortcut and cards are not grouped by step. Carried over: the composer still does not auto-grow (CSP blocks the inline style it would need) and assistant messages render as plain text pending a sanitizing markdown renderer.

## 2026-09-09 · AI session (Notion AI via Local) · P2-T01 SSE endpoint over activity_events

**Why.** The rail was accurate but late. P1-T13's 1 s poll re-fetched the whole turn on a timer, so a step appeared up to a second after it committed and every open tab paid a full `/activity` read per second. P2-T02 deliberately left the transport alone so this task only had to swap it.

**Changed.**
- `src/main.rs` — `GET /activity/stream?session_id=&after_seq=N`, inside the existing auth and Origin layer. A spawned task polls `agentic_sql::EVENTS_AFTER` every 200 ms (batch 200, matching the statement's own LIMIT) and pushes `id: <seq>` / `event: <kind>` / `data: <row>` frames into a 64-frame channel drained by `Frames`, a small `futures_core::Stream` handed to `Body::from_stream`; a `: heartbeat` comment goes out after 15 s of quiet, and the task gives up after 25 consecutive read failures. Bounds match `/activity`: 400 on a non-UUID `session_id` or a negative cursor, 401 without the bearer token.
- `static/app.js` — `followActivityStream(sessionId)` reads that stream with `fetch` + `TextDecoder` and an `AbortController`, subscribed per turn from `followReceipt` and closed on a terminal state. Header auth keeps the token out of the URL, which is the whole reason this is not `EventSource`. Frames advance a cursor and mark the rail live; refreshes are debounced 150 ms; a hidden tab marks itself stale instead of rendering; reconnects back off 1 s → 5 s and after 3 failures fall back to the old poll, which stays as the safety net.
- `Cargo.toml` — `futures-core` for the stream impl, tokio's `test-util` dev feature for virtual time. `README.md` documents the endpoint and why callers must use `fetch`, not `EventSource`.
- `tests/recording_integration.py` — a `stream a turn` script, an SSE reader, and the gate's new assertions: frames from a stream opened before the turn equal the `/activity` rows for that session (seq, kind and payload), resuming at `next_after_seq` replays nothing, `after_seq=-1` and a bad `session_id` are 400, no token is 401.
- `tests/recording_ui.cjs` — a mock `/activity/stream` route returning a finite SSE body plus a heartbeat, and a new `activity_stream_subscribed` check asserting the rail actually subscribes.

**Verified.** `cargo test --locked` → 85 passed (83 + `activity_stream_frames_recorded_rows_and_resumes_from_the_cursor` and `activity_stream_heartbeats_a_quiet_session`). `python3 tests/recording_integration.py` → PASS. `bash scripts/verify_release.sh` → exit 0 (clippy, build, migrations 001→003, 8 schemas, 50 Python contracts, both HTTP suites). `node tests/ui_smoke.cjs` → 14/14, `node tests/recording_ui.cjs` → 15/15, `node --check static/app.js` → OK. Browser suites again needed playwright-core in /tmp/harness-qa symlinked as `playwright` plus `CHROMIUM_PATH`.

**Continuity note.** The code landed in a session that was cut before any gate ran; this session re-verified the working tree unchanged, then did the bookkeeping and the commit. Nothing was marked done on trust.

**Gotchas.** (1) A missing `session_id` is an axum `Query` rejection with a plain-text body, so that status is asserted in the Rust test — the Python helper parses JSON. (2) Never hand a streaming path to the suite's `call` helper; it reads to EOF and hangs. (3) The 15 s heartbeat is only testable under `#[tokio::test(start_paused = true)]`; real time would add 15 s to the suite. (4) Exactly-once lives in the cursor: it advances only past frames already queued, so a mid-turn disconnect neither repeats nor drops a row.

**Open / deferred.** Frames still come from a 200 ms DB poll rather than a write notification — fine for a single-user SQLite install, revisit if the loop ever grows a broadcast channel. The two clippy dead-code warnings (`ToolCtx.request_id`, `Tool::plan`) stay parked on P2-T03. Assistant markdown rendering and composer auto-grow remain open P2 items.

**Next.** P2-T03 diff cards with accept/reject and undo (`POST /changes/{id}/revert`) — now the first `todo` whose dependencies are done.

## 2026-09-09 · AI session (Notion AI via Local) · P2-T02 full UI redesign

**Why.** The owner's verdict on the P1 minimal UI: too rigid, too bland, too much chrome on the chat. They asked for a full redesign before P2-T01 and picked the direction by survey: terminal-flat chat (no bubbles, like omp/claude-code), a collapsible right activity rail, dark-first theme with a light toggle, cozy 15px density. Task was reordered ahead of P2-T01 with the owner; the rail still uses the P1-T13 polling and T01 now only swaps the transport.

**Changed.**
- `static/index.html` — new shell: topbar (nav drawer button, brand, New chat, rail toggle, theme toggle, lock, connection badge) + left sidebar (scope picker with datalist, Conversations, nav: Chat/Inbox/Imports/Settings, stats footer) + centered chat column + right `#rail` hosting `#agent-turn` (plan, steps, context placeholder). Auth is a centered card. The permission card moved out of the agent panel to sit sticky above the composer (ui.md: "impossible to miss"). Every element id referenced by app.js and the browser suites is preserved (checked programmatically, 69/69); the stylesheet link tag stays byte-identical for recording_ui snapshots.
- `static/style.css` — full rewrite. Palette/typography ported from `reference/renewed-ui-original` into `:root` (dark default) and `:root[data-theme="light"]`; mono accents for roles/tools/timestamps. `.message` is now a flat grid row (label gutter + hairline separator), `#notice` is a floating toast (pointer-events:none so it never blocks clicks), sidebar drawer ≤920px, rail overlay ≤1100px.
- `static/app.js` — additive shell block only: theme init/toggle (localStorage `harness_theme`; not sensitive — tokens and drafts still never persist), drawer + rail toggles, Enter-to-send with Shift+Enter newline (ui.md#keyboard). Behavior edits: session list only auto-closes in drawer mode; rail auto-opens on wide screens while a turn is active and closes when it ends; chat scroll now targets the new `#chatscroll` container (loadHistory used to scroll `#log`, which no longer scrolls); a running step's meta shows live elapsed m:ss from `started_at`, re-rendered by the existing 1s poll; the rail context line shows real tokens-so-far summed from step receipts.
- `tests/ui_smoke.cjs`, `tests/recording_ui.cjs` — added the missing `/scopes` mock route. Pre-existing gap since P1-T15: `refreshScopeSetup()` 404'd in the mock and the auth handler re-hid the workspace, so both suites timed out at connect. The browser suites had never actually run; they pass now.

**Verified.** `node --check static/app.js` OK. `node tests/ui_smoke.cjs` → passed (14 checks). `node tests/recording_ui.cjs` → passed (14 checks). `bash scripts/verify_release.sh` → exit 0 (83 Rust tests, clippy, release build, migrations 001→003, 8 schemas, 50 Python contracts, both mock-provider HTTP suites). Browser suites ran with playwright-core in /tmp/harness-qa (symlinked as `playwright`) and `CHROMIUM_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`. Screenshots in docs/qa reviewed across desktop dark/light, 820px rail overlay and 390px mobile.

**Gotchas found.** (1) Playwright fullPage screenshots paint a transform-hidden fixed sidebar — the closed drawer is now `visibility:hidden` as well, which is also more robust for real renderers. (2) While QA-ing, a scope-mismatched mock made `loadHistory` refuse to render another scope's messages — the guard worked as designed; mock data just has to match the page scope. (3) CSP `style-src 'self'` forbids inline style attributes, so all presentation state moves via classes/hidden/data-theme; composer auto-grow skipped for that reason.

**Open / deferred.** Assistant markdown rendering (ui.md allows in P2 with a sanitizing renderer); context budget bar fills in P3; diff cards are P2-T03. QA screenshots under emulated light scheme show dark unless the toggle is clicked — tests assert overflow/behavior, not palette.

**Next.** P2-T01 SSE endpoint over `activity_events` — `refreshAgentTurn`'s 1s poll is the single call site to swap; then P2-T03 diff cards.

## 2026-09-09 · AI session (Notion AI via Local) · P1-T15 first-run project setup

**Why.** The owner ran the finished P1 harness for the first time and it looked broken: in the default `global` scope the agent answered "I don't have access to a terminal or file system tools in this conversation." Nothing was broken — `global` had no `root_path`, so the loop attached zero tools exactly as P1-T04 requires. The defect was that no layer said so. The workaround was a hand-written `curl POST /scopes/myharness`, which is not a product.

**Changed.**
- `prompts/main_agent.md` line 2 is now `{{tools}}`. `window()` fills it with `TOOLS_ATTACHED` (the old sentence) or `TOOLS_WITHHELD`, which tells the model to blame the missing project root, name `Project & models → Project scope settings` / `POST /scopes/{scope}`, and *not* to claim the conversation or provider lacks tool support.
- `agent_loop::run` records one `tools_withheld` activity event (`reason: no_root_path`) beside `turn_started` when the tool list is empty, so `/activity` and the UI carry the same fact as the answer.
- New `GET /scopes` (`SCOPES_LIST`, `DbStore::scopes`, `list_scopes`) lists every configured scope, a null `root_path` included — that scope exists, it just cannot run tools.
- UI: the scope field is backed by a `datalist` of real scopes (each labelled with its root path, or "no project root · tools off"), and the chat view shows a `#setup-banner` whenever the current scope has no root, with a button that jumps to the existing project form and focuses `Root path`. Refreshed on connect, on scope change, on New conversation, on reopening a conversation, and after saving project settings; cleared on Lock.

**Verified.** `bash scripts/verify_release.sh` → exit 0: 83 Rust tests (up from 81), clippy, release build, migrations, 8 tool schemas, 50 Python contracts, both mock-provider HTTP suites. New tests: `configured_scopes_are_listed_for_the_picker` (empty on a fresh install; ordered; a rootless scope is still listed) and `the_prompt_names_the_missing_project_root_instead_of_promising_tools` (withheld text names scope and fix; the configured prompt still says "You have tools." and never says "NO tools"). `SCOPES_LIST` gained a contract assertion in `tests/test_agentic_sql.py`. `node --check static/app.js` → OK.

**Not verified.** The banner and datalist are not exercised by the gate — `tests/ui_smoke.cjs` / `tests/recording_ui.cjs` need a running server and are still run by hand.

**Decisions.**
- The tool gate itself did not move. An unconfigured scope still gets no tools; only the explanation changed. Making `global` auto-adopt a working directory would have turned a safety property into a surprise.
- The banner reads the same `/scopes` data the picker uses, so it cannot disagree with what the loop will do; it does not ask the server "are tools on?" as a separate opinion.
- Still open: the two clippy warnings (`ToolCtx.request_id`, `Tool::plan`) remain parked on P2-T03, and `GET /scopes` has no pagination — a single-user install with hundreds of scopes is not a case worth code yet.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1 review pass before P2

**Changed.** No behaviour; this is the review of P1-T10…T14 read back as a whole.
- `request_permission` had grown to eight positional arguments, five of them `String`, so a caller could transpose `request`/`session`/`step` and still compile. It now takes a `NewPermission` struct, like `NewStep` and `StepOutcome` already do. This also retired the `clippy::too_many_arguments` warning it had earned.
- Rewrote the `grep` literal fallback's two context loops as slice iterations, retiring the `needless_range_loop` pair that had been carried since P1-T06, and dropped a `redundant_closure` in a P1-T11 test. Clippy is now down to the two deliberate dead-code warnings.
- **That refactor was untested**, because `rg` is installed here and the fallback never runs: the existing grep tests all pass `context: 0`. Added `grep_fallback_numbers_its_context_window`, which calls the fallback directly and pins the rendered window — match lines keep `:`, neighbours keep `-`, each with its own number — including the clamp at the first and last line. Hand-numbered context is exactly the arithmetic that rots silently.
- `ToolCtx.request_id` and `Tool::plan` now say in the source that they are waiting on **P2-T03** (accept/reject a diff card), not P1-T11. P1-T10 records changes as `applied=1` because the tool has already written the file, so the plan-then-apply split has no caller until a diff card can reject one.

**Verified.**
- `bash scripts/verify_release.sh` → exit 0, unchanged: 80 Rust tests, clippy, release build, 50 Python contracts, migrations, 8 schemas, both mock-provider HTTP suites.
- Read the T11–T14 diffs against the design doc. The permission gate is right where it counts: `resolve_permission` decides on the stored `status` and never on the clock, which is safe because something always flips a pending row — the loop at its own deadline, or `RECOVER_PERMISSIONS` at startup — and the "approved a moment before the deadline" race is handled by `expire_permission` returning the current status so a late `approved` is still honoured.
- The T13 UI never touches `innerHTML`; every model- and tool-authored string goes through `textContent` or `createElement`. No DOM-injection path from tool output or a diff preview.

**Open.**
- Unchanged caveats: `.harness/logs/` is never pruned; a background `pid` is the owning shell, not the job; `permission_payload` diffs cap at 64 KiB; the browser suites (`tests/ui_smoke.cjs`, `tests/recording_ui.cjs`) are still not run by the gate.
- The `rg`-vs-fallback caveat is now narrower, not gone: the fallback's own rendering is pinned, but nothing asserts the two engines agree on the same corpus, and the `.gitignore` behaviour only ripgrep provides is still untested.
- While a turn is pending the UI polls three endpoints every second and the API admits eight concurrent requests. It degrades quietly today (the interval swallows the error), and P2-T01 removes two of the three polls, but the SSE stream must not hold its semaphore permit for the life of the connection.

**Next.**
- P2-T01 resumable authenticated SSE over `activity_events`. `/activity` already returns a DB-sequence cursor that provably does not replay, which is the hard half.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T14 scripted tool-call HTTP coverage

**Changed.**
- Added `tests/mock_provider.py`, a reusable OpenAI-style loopback provider with per-prompt canned responses, tool-call construction, failures, and captured request bodies.
- Expanded `tests/recording_integration.py` to configure a temporary project scope and exercise the real HTTP agent loop: read → edit → bash → answer, assistant/tool message replay, applied file changes, stale anchors, sandbox path escape, permission denial, budget exhaustion, provider failure, and SIGKILL during a running bash step.
- Recovery assertions verify the interrupted tool is recorded as `interrupted`, the receipt is interrupted, and restart does not make another provider call or re-execute the tool.

**Verified.**
- `python3 -m py_compile tests/mock_provider.py tests/recording_integration.py` → pass.
- `python3 tests/recording_integration.py` → PASS.
- `bash scripts/verify_release.sh` → exit 0: 80 Rust tests, migrations, tool schemas, 50 Python contracts, and the real HTTP suite.

**Next.**
- P2-T01: authenticated resumable SSE over `activity_events`, replacing the UI's 1-second polling path.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T13 minimal UI

**Changed.**
- Restored the existing local-first conversation shell after the initial UI pass and added a turn record panel in `static/index.html`, `static/app.js`, and `static/style.css`.
- The panel polls the durable steps, plan, and pending-permission APIs while a request is active. Steps are native collapsible rows with input/output previews; approval cards show the tool-owned diff or command and send idempotent Approve/Deny decisions; plans render as a checklist with progress.
- Renamed the Models view to Project & models and added scope settings for root path, permission mode, diagnostics command, and budgets via `/scopes/{scope}`.
- Kept the existing security posture: relative API calls, bearer token only in tab memory, no inline styles, no `innerHTML`, and all untrusted text rendered through DOM nodes / `textContent`.

**Verified.**
- `node --check static/app.js` → pass.
- `git diff --check` and HTML-hook check → pass; all 64 direct JS element hooks resolve.
- `cargo test --locked` → **80** passed.
- Browser harness was attempted but is blocked in this checkout because the `playwright` Node module is not installed.

**Next.**
- P1-T14: extend the synthetic HTTP provider to emit scripted tool calls and cover the full read → edit → bash → answer path over HTTP.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T12 API for steps, plan, activity

**Changed.**
- Readers in `src/storage.rs`: `turn_steps`, `activity_since`, `turn_changes` (`plan` already existed), with thin handlers in `src/main.rs` under the existing auth and Origin middleware. They only read rows the loop already committed — nothing recomputes a summary or re-renders a diff — so the UI cannot show a version of a turn that the record disagrees with.
- `GET /chat/requests/{id}/steps` returns steps in `seq` order with 2 KB previews. Two decisions worth knowing: it **404s** on an unknown request, because an empty step list would otherwise read as "this turn did nothing"; and `previews_capped` is reported separately from `truncated`, because the preview hitting 2 KB and the tool's own output being capped are different facts.
- `summary` is the tool's own phrase, read back from the finished step's output. A step that is still running therefore has none — its `tool_started` activity event carries it, which is what the live UI polls anyway. No summary is ever recomputed from the model's text.
- `GET /activity?session_id=&after_seq=N` returns ≤ 200 events plus `next_after_seq`, which only advances when rows came back, so a poll that finds nothing cannot skip an event that commits a moment later. This feed is the agentic log only; `captured`/`generation_started` stay in `recording_events` behind `/chat/requests/{id}/context`.
- `GET /sessions/{id}/plan` treats an unknown session and a session without a plan identically (`{items:[]}`), matching `/sessions/{id}/messages`. The plan is a view of a session, not proof one exists.
- Added `GET /changes?request_id=` from the same design section, beyond the task's done-when: ten lines, it consumes the last dead SQL constant, and P1-T13 / P2-T03 both need it. `POST /changes/{id}/revert` stays P2-T03.

**Verified.**
- `python3 tests/recording_integration.py` → PASS, now including the read side over real HTTP: a completed turn's single `model_call` step (bounded previews, no `error_code`) and its `turn_started → model_call_started → model_call_finished → answer_saved` feed with cursor paging that neither replays nor skips; a provider failure reading `failed / provider_failed` and ending in `turn_failed`; a SIGKILL'd step reading `interrupted`, not `failed`; empty plan and empty changes for a turn that ran no tools; 401 / 403 / 404 on the new routes.
- `cargo test --locked` → **80** passed (new: steps/plan/activity/changes read back what `begin_step`/`finish_step` wrote, including the preview cap, the cursor and every bounds case). `bash scripts/verify_release.sh` → exit 0.

**Open.**
- The suite's provider is text-only, so no *tool* step, plan item or file change is asserted over HTTP yet — those paths are covered by `cargo test` against the scripted provider, and P1-T14 owns the scripted-tool-call HTTP suite.
- Every endpoint is polling. SSE is P2, and `/activity` has no per-request filter (session only), which is all the P1 UI needs.
- `ToolCtx.request_id` is now the only remaining deliberate dead-code warning (with `Tool::plan` / `PendingChange`, which wait on a dry-run tool contract). All 23 SQL constants are used.
- Unchanged: `.harness/logs/` is never pruned; a background `pid` is the owning shell, not the job; `permission_payload` diffs cap at 64 KiB; the browser suites were not run.

**Next.**
- P1-T13 minimal UI for steps and permissions — its dependencies (T04, T11, T12) are all done now, and it is the first task that makes any of this visible without curl.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T11 permission gate

**Changed.**
- `GET /permissions?scope=` lists the approvals still waiting, each with the tool's own `summary` and its `args_json` payload (the diff preview, the command) so a card can be rendered without asking the model to describe what it is about to do.
- `POST /permissions/{id}` body `{decision:"approve"|"deny", scope}`. `DbStore::resolve_permission` writes the decision, reads the session through `SESSION_OF_REQUEST` and appends `permission_resolved` in ONE Immediate transaction.
- Idempotency comes from the `status='pending'` guard already in `PERMISSION_RESOLVE`, not from a second read: the first decision wins, a replayed click answers `200 {"recorded":false}` and logs **no** second event, the opposite decision is `409` and changes nothing, a decision that arrives after the loop gave up is `410`, and a foreign scope is `404` rather than a hint that the row exists.
- The loop needed no new waiting logic. It already polls `status` every 500 ms, so committing the decision is what unblocks the turn — which is exactly the path T10 could not test.
- **The T10 open question is answered: the earlier deadline wins.** `permission_ttl()` writes `expires_at` as `min(30 min, remaining wall budget)`, and `request_permission` now takes that TTL from its caller. A default turn offers a 15-minute window and says so, instead of advertising 30 minutes it will not honour. Documented in `docs/design/agentic-turn.md#permissions`.
- `auto_edit` / `auto_all` needed no new code — `Registry::requires_permission` already held the matrix — but they are now covered end to end rather than by inspection.
- README's API sketch lists both endpoints; the deny test in `agent_loop` now denies through `resolve_permission` instead of raw SQL, so the test drives the same code the endpoint does.

**Verified.**
- `cargo test --locked permissions` → 8 passed: an approval lets a write reach the disk with exactly one `permission_resolved` event and an empty pending list afterwards; `auto_edit` applies a write with no approval row at all yet still stops `bash`; `auto_all` still asks before a deny-listed `git push --force` and reports the denial to the model; resolution is idempotent and refuses a flip; an expired row cannot be approved afterwards; the TTL clamp picks the earlier clock; the endpoints reject an unknown id (404), a bad decision word (400) and an unauthenticated caller (401).
- `bash scripts/verify_release.sh` → exit 0: **79** Rust tests (up from 71), clippy, release build, 50 Python contracts, migrations, 8 schemas, both mock-provider HTTP suites.

**Open.**
- No UI yet: approving still means calling the endpoint by hand. The Approve/Deny card is P1-T13, and a python HTTP approval round-trip belongs to P1-T14; `recording_integration.py` does not exercise these endpoints yet.
- A single turn's `expires_at` is fixed when the row is created; raising a scope's `max_wall_seconds` mid-wait does not extend a row that is already pending.
- Dead-code warnings left for P1-T12: `STEPS_LIST`, `EVENTS_AFTER`, `FILE_CHANGES_LIST`, `ToolCtx.request_id`. (`PERMISSION_GET`, `PERMISSION_RESOLVE` and `PERMISSIONS_PENDING` are now used.)
- Unchanged: `.harness/logs/` is never pruned; a background `pid` is the owning shell, not the job; `permission_payload` diffs cap at 64 KiB; the browser suites were not run.

**Next.**
- P1-T12 API for steps, plan and activity — its only dependency (T10) is done, and it consumes `STEPS_LIST` and `EVENTS_AFTER`.

---

## 2026-09-09 · AI session (Notion AI via Local) · P1-T10 agent loop

**Changed.**
- `src/agent_loop.rs` (new) is the whole turn: `window()` renders `prompts/main_agent.md` with the scope root, recall and plan as the system message, and `run()` drives model call → tool calls → model call until the provider returns text with no tool calls.
- The storage half lives in the same file, next to its only caller. `begin_step` (next sequence, step row, activity event) and `finish_step` (result, event, artifacts) each commit in ONE Immediate transaction, so a step is never readable without the event that announces it.
- `Artifact::FileChange` writes `file_changes` with **`applied=1`**: the tool has already written the file by the time the loop sees the artifact. The design doc's plan → approve → write ordering needs a dry-run tool contract, so `PendingChange` and `Tool::plan` stay unused until P1-T11 — they warn as dead code today, deliberately.
- `Artifact::Plan` now goes through a new `storage::write_plan`; `DbStore::replace_plan` is gone. The plan lands in the same transaction as the step that produced it instead of a second one that could fail on its own.
- Budget is checked before every provider call, and the answer an exhausted turn returns says in as many words that the task is **not** finished — an out-of-budget stop must not read as success.
- `context_json` keeps the first window only, as the contract says; each `model_call` step then stores its own message array plus the tool **names**. Storing the schemas per step would have multiplied ~8 KB of JSON by up to 40 steps.
- Tool arguments that are not a JSON object fail the step as `invalid_arguments` before any tool runs. A provider that rejects the `tools` field once is retried without tools (`tools_unsupported`); a provider that fails otherwise fails the turn rather than inventing an answer.
- Text-only turns (no scope root) run the same loop with an empty tools array, so `memory_agents::chat_prepared` was deleted rather than left as a second code path.
- `recording.rs`: `recover()` also marks running steps `interrupted`, denies orphaned permissions and writes the activity rows; `complete_recording` writes `answer_saved`; `fail_recording` writes `turn_failed {error_code}`. All 13 activity kinds in the events contract are now emitted by something.
- The permission gate is built as far as this task reaches: create the `pending` row plus `permission_requested`, poll every 500 ms, expire on timeout, and hand the denial to the model as a tool error it can adapt to. P1-T11 owns the endpoints and the approval path.

**Verified.**
- `cargo test --locked agent_loop` → 7 passed, against an in-process scripted provider on loopback and the real `recording::generate`: a tool turn records every step and change before answering, an exhausted budget answers without claiming success, unreadable arguments never reach a tool, a denied tool changes nothing and is reported to the model, a provider without tool support falls back to text, a provider failure fails the turn, and a restart interrupts running work and reruns nothing.
- `bash scripts/verify_release.sh` → exit 0: 71 Rust tests, clippy, release build, 50 Python contracts, migrations, 8 tool schemas, and both mock-provider HTTP suites — including `recording_integration.py`'s SIGKILL-and-restart check that no tool is replayed.

**Open.**
- In `ask` mode nothing can approve a request until P1-T11's endpoints exist, so a side-effecting call waits out the turn's wall budget (15 min) and is then denied. The row's own TTL is 30 min and therefore unreachable from the loop; T11 should decide which limit wins.
- Dead-code warnings that P1-T11/T12 will consume: `STEPS_LIST`, `EVENTS_AFTER`, `PERMISSION_GET`, `PERMISSION_RESOLVE`, `PERMISSIONS_PENDING`, `FILE_CHANGES_LIST`, `ToolCtx.request_id`, `Tool::plan`.
- `.harness/logs/` still grows without pruning; the background `pid` is the owning shell, not the job; `permission_payload` diffs are capped at 64 KiB; the browser suites (`tests/ui_smoke.cjs`, `tests/recording_ui.cjs`) were not run.

**Next.**
- P1-T11 permission gate — its only dependency (T10) is now done.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T09 think / todo_write

**Changed.**
- Implemented P1-T09 as one file `src/tools/meta_tools.rs` (the name the `mod.rs` stub reserved) instead of `think.rs` + `todo.rs`. All eight tools of P1 are now registered.
- Neither tool is `side_effecting`, so a scratchpad note and a plan update can never sit waiting for an approval, in any permission mode.
- `think` returns the trimmed text as the step **output** — the collapsed reasoning card the UI will render — and puts the "noted" acknowledgement in the `summary`, because one result field cannot be both. Over 4000 characters is `too_large`; blank is `invalid_arguments`.
- `todo_write` normalizes (trim, absent `status` → `pending`, `seq` = position) and enforces ≤ 30 items, ≤ 200 characters and one `in_progress` as `invalid_arguments`, so the model can repair its own plan instead of hitting a CHECK. The result goes to the loop as `Artifact::Plan`; `DbStore::replace_plan` does the writing, so tools still never touch the database.
- `src/storage.rs` gains `replace_plan` (one Immediate transaction: `PLAN_CLEAR`, `PLAN_INSERT` per item, read back through `PLAN_LIST`) and `plan` for the `plan_updated` event and the P1-T13 panel. It re-checks the same limits, because a CHECK failure would otherwise reach the caller as an opaque 500. `MAX_PLAN_ITEMS` / `MAX_PLAN_TEXT` / `PLAN_STATUSES` live there as the single source of truth and the tool reads them.
- `truncate_chars` moved into `src/tools/mod.rs` so `bash` and `todo_write` share one label shortener.
- The old `verify:` line named `tools::todo`, which matches no test path. It is now `cargo test --locked plan`, which covers both halves of the task — the tool tests and the storage test — in one command.

**Verified.**
- `cargo test --locked plan` → 6 passed (think's output and refusals, todo_write normalization and artifact, seven rejected plan shapes, whole-plan replacement with rollback, plus the pre-existing edit_tools plan test the filter also catches).
- A new registry test asserts all eight schema files parse and their `function.name`s match the registered tools in order — this also retired the `schema`/`schemas` dead-code warnings.
- `bash scripts/verify_release.sh` → passed: 64 Rust tests, Clippy, release build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- `think` echoes the thought back to the model, which costs tokens on every turn it is used. Worth revisiting if P3 compaction shows it dominating.
- Nothing calls `replace_plan` or `plan` outside tests until P1-T10 consumes `Artifact::Plan` and emits `plan_updated`.
- The Local Ops bridge dropped mid-task; the code was already on disk and was verified once the connection returned.

**Next.**
- P1-T10: the agent loop in the generation worker — all its dependencies (T03, T05, T07, T08, T09) are now `done`. It must call `plan` → record `file_changes(applied=0)` → approve → `run` → flip to `applied=1`, and persist `Artifact::Plan` through `replace_plan`.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T08 bash

**Changed.**
- Implemented P1-T08 as `src/tools/bash_tool.rs` — the name the `src/tools/mod.rs` stub already reserved, rather than the `src/tools/bash.rs` in TASKS.md. The `verify:` filter still matches, because `tools::bash` is a prefix of `tools::bash_tool::tests`.
- Foreground calls reuse `edit_tools::run_capped`, so the env whitelist (`PATH HOME LANG LC_ALL TERM` + `HARNESS_SCOPE`), the cwd and the temp-file output capture are shared with `diagnostics_cmd` and cannot drift apart.
- `run_capped` gained two things for this task. The child now runs in its own process group (`process_group(0)`) and a timeout kills the **group**, so a `make`-style process tree cannot outlive the step that started it. A `started` flag also separates "could not spawn" (`spawn_failed`) from "killed by a signal" (`signal`).
- A non-zero exit is deliberately not a failed step: status stays `complete`, the code goes into the new `ToolResult::exit_code` (the `tool_finished {exit_code?}` field the events contract already promised) and the output ends with `[exit N]`. A red test run is information the loop has to act on, not a harness error.
- A timeout **is** a failed step but keeps its partial output, via a new `ToolResult::failed` that leaves the content as raw output instead of the JSON envelope `err` produces.
- `timeout_seconds` over the 600 s maximum is clamped, with a note appended to the output; a fractional or `< 1` value is `invalid_arguments`. A missing `description` falls back to the first 60 chars of the command instead of failing the call.
- Background runs `sh -c '<cmd>' >>log 2>&1 &` and reports `$!`, not `setsid`: macOS does not ship `setsid`, and the util-linux builds that fork print the wrapper's pid rather than the job's. The outer shell exits immediately, so the job is reparented to init, and the new process group already puts it out of reach of signals aimed at the harness. Logs go to `<root>/.harness/logs/<step_id>.log`, with the step id reduced to a safe filename stem.
- The deny-list was already in `mod.rs` (`is_dangerous_command`, consulted by `Registry::requires_permission`). This task covers it with tests and surfaces it to the future UI as `dangerous` in `permission_payload`, alongside the command, cwd, timeout and background flag.

**Verified.**
- `cargo test --locked tools::bash` → 8 passed (cwd + stripped env, failing command, timeout with partial output, clamped timeout, seven bad argument shapes, background pid/log round trip, deny-list in every permission mode, summary and payload).
- `bash scripts/verify_release.sh` → passed: 59 Rust tests, Clippy, release build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- The reported pid is the shell that owns the job, so `kill <pid>` stops a simple command but not every child of a compound one. The returned note tells the model how to tail and stop the job.
- Nothing prunes `.harness/logs/`; a long-lived scope will accumulate one log per background step.
- `exit_code` is plumbed but unread until P1-T10 emits `tool_finished`.

**Next.**
- P1-T09 (`think` / `todo_write` over the existing `plan_items` SQL), then P1-T10 wires the provider adapter, the registry, the permission gate and `file_changes` into the generation worker.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T07 edit / write

**Changed.**
- Implemented P1-T07 as one new file `src/tools/edit_tools.rs` (`Edit` + `Write` + shared plan/apply/diagnostics helpers), matching the `fs_tools.rs` precedent. `docs/TASKS.md` named `src/tools/edit.rs` and `src/tools/write.rs`; the `verify:` line named `tools::write`, which matches no test path, so it was corrected.
- The prepare/record/apply requirement became a default trait method in `src/tools/mod.rs`: `Tool::plan(ctx, args) -> Option<Result<PendingChange, ToolResult>>`, plus a `PendingChange` struct. `plan` never writes, so P1-T10 can record `file_changes(applied=0)`, render the diff for approval, then call `run` and flip the row to `applied=1`. Non-editing tools inherit `None` and are unaffected.
- `run` re-plans before applying rather than trusting the plan it was approved on: the file can change while a permission request is pending, and the hash anchors must still hold at write time.
- `edit` anchors on the `content_hash` and per-line hashes that `read` emits. A drifted anchor fails with `stale_anchor` and three lines of current context instead of overwriting; `old_string` must match exactly once (`no_match` / `ambiguous_match` with the offending line numbers). `write` refuses an existing file unless `overwrite:true`, caps input at 512 KiB, and rejects directories.
- Writes are atomic: `.<name>.harness-tmp-<step>` in the destination directory, then rename, preserving the existing file mode; the temp file is removed on failure. An identical rewrite reports "already had this content" and records no `file_changes` row, so a model re-issuing the same edit does not queue a redundant approval.
- `permission_payload` carries `{path, action, plus, minus, before_hash, after_hash, diff}` (diff capped at 64 KiB) for the P1-T13 UI. `summary` stays `edit <path>` because the trait gives it no filesystem access, so the ± counts live in the payload.
- Diagnostics go through a shared `run_capped` helper (`sh -c`, cwd = root, env stripped to `PATH HOME LANG LC_ALL TERM` + `HARNESS_SCOPE`, 60 s cap, 4 KiB of output kept, output interleaved through a temp file so a full pipe cannot deadlock the timeout poll). It is `pub(crate)` because P1-T08 reuses it for `bash`.

**Verified.**
- `cargo test --locked tools::edit_tools` → 7 passed (stale anchor, ambiguous and missing `old_string`, first write, clobber refusal, no-op rewrite, atomic replace with mode preserved).
- `cargo clippy --locked --all-targets` → exit 0; only the pre-existing `needless_range_loop` pair in the `grep` fallback plus the dead-code cascade below.
- `bash scripts/verify_release.sh` → passed: 51 Rust tests, Clippy, release build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- The diff in `permission_payload` is truncated at 64 KiB; a larger change is still applied in full but reviewed partially. P1-T13 should say so in the UI.
- `PendingChange`, `Registry::invoke` and the new caps report dead-code warnings until P1-T10 consumes them.
- Nothing has been committed yet this session.

**Next.**
- P1-T08 (`bash`: `sh -c` at the scope root, env whitelist, 120 s default / 600 s max, `background:true` via `setsid` logging to `<root>/.harness/logs/<step_id>.log`, deny-list that always prompts even in `auto_all`), then P1-T09 (`think` / `todo_write`), then P1-T10 wires the loop.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T04 scopes + P1-T05/T06 verified

**Changed.**
- Implemented P1-T04. `src/storage.rs` gains `ScopeConfig` (with `mode()`, `budgets()` defaulting to 40 / 400000 / 900, and `tool_ctx()`), a `ScopePatch` request type, `canonical_root()`, and `DbStore::scope_config` / `upsert_scope` on the existing `SCOPE_GET`/`SCOPE_UPSERT` constants. `src/main.rs` gains `GET`/`POST /scopes/{scope}` behind the existing auth + origin middleware.
- `POST` merges into the stored row: an absent field keeps its value, an explicit `null` clears it. This is why the request type uses `Option<Option<T>>` — a partial POST must not be able to silently drop `root_path` and disable tools.
- Validation runs before SQLite so a bad request is a 400 instead of a CHECK failure: `root_path` must be absolute, is canonicalized (symlinks resolved), must be an existing directory, must not be the filesystem root, and must not sit inside the harness data dir; `permission_mode` goes through `PermissionMode::parse`; `diagnostics_cmd` is trimmed, capped at 512 chars, and refused when it matches the bash deny-list; the three budgets are range-checked to mirror the 003 CHECKs.
- Tool gate: `Registry::invoke(ctx, name, args)` in `src/tools/mod.rs` is now the loop's single entry point. `ctx` is `None` for a scope without `root_path`, and every call then fails with `tools_disabled` instead of guessing a working directory. `paths::harness_data_dir()` became `pub` so P1-T04 can reuse it.
- P1-T05 and P1-T06 moved from `needs-verify` to `done`: both compiled unchanged in this checkout, and `src/tools/fs_tools.rs` gained end-to-end tests for `read`, `grep` and `glob` over a temp project (numbered lines and `content_hash`, `offset`/`limit` windowing, `.env` refusal, directory and missing-argument errors, match counts, glob filtering, `..` rejection).
- P1-T06's `verify:` line named three module paths (`tools::read`, `tools::grep`, `tools::glob`) that never existed — all three tools share `tools::fs_tools`. Corrected the command rather than splitting the file; the existing note already documented the one-file decision.

**Verified.**
- `cargo test --locked scopes` → 6 passed (storage merge/gate, validation rejections, HTTP round trip, HTTP 400).
- `cargo test --locked tools::paths` → 4 passed. `cargo test --locked tools::fs_tools` → 4 passed. `cargo test --locked tools::` → 10 passed.
- `bash scripts/verify_release.sh` → passed: 41 Rust tests, Clippy, build, 50 Python contracts, both mock-provider HTTP suites, `node --check static/app.js`.

**Open.**
- `rg` is installed here, so `grep`'s literal fallback branch (and the `.gitignore` behaviour that only ripgrep provides) is still untested.
- Nothing calls `Registry::invoke`, `ScopeConfig::budgets` or `tool_ctx` outside tests yet, so the crate still reports dead-code warnings; P1-T10 consumes them.
- No UI for scopes yet; that is P1-T13. Browser suites remain separate.

**Next.**
- P1-T07 (`edit`/`write` with hash anchors and `file_changes`), then P1-T08 (`bash`), P1-T09 (`think`/`todo_write`), then P1-T10 wires the provider adapter to the registry.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P1-T03 provider adapter + P1-T01 verification

**Changed.**
- Implemented P1-T03 in `src/memory_agents.rs`: non-streaming OpenAI-style tools, `tool_calls`, usage, assistant-message replay, bounded provider errors, and safe malformed-argument parsing; the text-only extraction path remains intact.
- Marked P1-T01 done after verifying the migration chain and storage test in the owner's checkout.

**Verified.**
- `cargo test --locked provider` → 6 passed.
- `cargo test --locked` → 35 passed.
- `python3 tests/test_migrations.py && cargo test --locked storage` → passed.
- `bash scripts/verify_release.sh` → passed: native, SQL, and local mock-provider HTTP gates.

**Open.**
- Browser UI suites remain separate.
- The provider adapter is ready for P1-T10 to consume; malformed arguments still need to be turned into persisted `invalid_arguments` tool steps by the agent loop.

**Next.**
- P1-T04: scopes (`root_path`, `permission_mode`, `diagnostics_cmd`), then the remaining tool modules and agent loop.

---

## 2026-09-09 · AI session (Notion AI via Local Ops) · P0 baseline verification

**Changed.**
- No application code changed.
- Updated `docs/TASKS.md`: P0-T01 is `done`; clarified that agentic recovery belongs to P1-T10, corrected the P1-T06 file path, added the prepare/record/apply requirement for file changes, and repaired later task dependencies.

**Verified.**
- `bash scripts/verify_release.sh` → passed in the owner's local checkout.
- Cargo/Rust 1.92.0: 29 Rust tests, Clippy, and release build passed.
- Python contracts and both local mock-provider HTTP suites passed.
- `node --check static/app.js` passed.

**Open.**
- Browser UI suites remain separate.
- P1-T01, P1-T05, and P1-T06 retain their existing `needs-verify` status until their task-specific verification and dependency sequencing is completed.

**Next.**
- P1-T03 is now the first dependency-satisfied implementation task: add the non-streaming provider adapter for OpenAI-style tool calls while preserving the extraction path.

---

## 2026-09-09 · AI session (Notion AI, sandbox without cargo) · Planning + P1 groundwork

**Context.** Owner asked for (1) tooling like oh-my-pi / OpenClaude, (2) a thin main agent backed by context/recall/advisor managers so coding does not hallucinate, (3) a chat UI that shows plan, running tools, logs and diffs, and (4) a written plan an AI can continue from.

**Changed.**
- New: `AGENTS.md` (entry point + operating rules), `docs/PLAN.md`, `docs/TASKS.md`, `docs/PROGRESS.md`, `docs/design/agentic-turn.md`, `docs/design/tools.md`, `docs/design/ui.md`.
- New: `migrations/003_agentic.sql` (scopes, turn_steps, activity_events, permission_requests, file_changes, plan_items; user_version=3).
- New: `tools/schemas/*.json` for read, grep, glob, edit, write, bash, think, todo_write, generated by `scripts/gen_tool_schemas.py`.
- New: `prompts/main_agent.md` (evidence rules, work loop, boundaries; `{{root_path}} {{scope}} {{recall}} {{plan}}` placeholders).
- New tests: `tests/test_migrations.py`, `tests/test_tool_schemas.py` (both unittest-discoverable, so `verify_release.sh` and CI already run them).
- Edited: `src/storage.rs` — accept `user_version` 1..=3 and apply 003 when `< 3` (two lines).
- Edited: `README.md` "Remaining work" and `docs/ROADMAP.md` header now point to PLAN/TASKS.

**Verified here.** `python3 -m unittest discover -s tests -p 'test_*.py'` → OK (16 SQL-contract tests + 2 new). `node --check static/app.js` → OK. Python's SQLite 3.40 has FTS5, so the migration chain 001→002→003 was applied for real, including CHECK/UNIQUE enforcement and the recovery UPDATEs.

**Not verified.** Anything Rust: the sandbox has no `cargo`/`rustc`. The `storage.rs` edit is trivial but unbuilt → P1-T01 is `needs-verify`. The repo's own ROADMAP says the *existing* code has never been compiled in this checkpoint either (P0-T01 remains the gate).

**Decisions made (and why).**
- Separate `activity_events` table instead of widening `recording_events.kind`: keeps the 002 CHECK and the existing receipt timeline stable; UI/SSE reads the new table.
- Per-step rows in `turn_steps` with `input_json` = the exact message array for each `model_call`: gives a full audit trail while keeping the write-once `context_json` semantics from 002.
- Hash-anchored `edit` (`line:hash`, 4-hex per line) borrowed from oh-my-pi; `old_string` allowed only within the anchored region. Rationale: rejects stale edits deterministically and gives the model fresh anchors in the result.
- Permission modes `ask | auto_edit | auto_all`, with a bash deny-list that always asks.
- Memory-category widening deferred to 004 because it needs a table rebuild (SQLite cannot alter a CHECK).

**Open questions for the owner.**
1. Does the target provider (`LongCat-2.0` at `HARNESS_BASE_URL`) support OpenAI `tools`/`tool_calls`? If not, P1-T03 needs a fallback model or a prompt-based tool protocol. The adapter design already records `tools_unsupported`.
2. Should `bash` be allowed at all in `auto_edit` mode without asking, for read-only commands (`git status`, `ls`)? Current design: no; every bash asks unless `auto_all`.
3. Line-hash function: SHA-256 of the right-trimmed line, first 4 hex. OK, or prefer something cheaper (xxhash) once the crate list grows?
4. Do you want `similar` (diffs), `regex`, `ignore`, `sha2` added to Cargo.toml in P1, or keep zero new deps and hand-roll? (Design assumes the crates.)

**Next.** P0-T01 on the owner's machine (`bash scripts/verify_release.sh`). Then P1-T03 → P1-T05 → P1-T06 in that order; all Rust tasks will be `needs-verify` if written from a sandbox without cargo.

---

## 2026-09-09 (session 2) — SQL layer for the agentic turn + first tool modules

**Context.** Owner said "keep going" and asked for all changed/new files as a downloadable bundle (zip downloads are blocked on their side; `.md`/`.txt` work, so bundles are shipped as a zip renamed to `.md`/`.txt` — rename back after download, or use the self-extracting `harness-changes.md`).

**Changed.**
- New: `src/agentic_sql.rs` — every SQL statement the loop, permission gate, plan tool and recovery need (23 `pub const`s: `SCOPE_*`, `STEP_*`, `EVENT`/`EVENTS_AFTER`, `PERMISSION_*`, `FILE_CHANGE*`, `PLAN_*`, `SESSION_OF_REQUEST`, `RECOVER_STEPS/PERMISSIONS/ACTIVITY`). Same style as `recording_sql.rs`.
- New: `tests/test_agentic_sql.py` — 7 offline contract tests that regex-extract those constants and execute them against the real 001→002→003 schema (scope upsert + CHECK, step lifecycle incl. double-finish guard and UNIQUE(request_id,seq), event cursor, permission approve/deny idempotency + expiry, file_changes CHECK, plan replace, recovery). Discoverable by `verify_release.sh`/CI.
- New: `src/tools/mod.rs` — `Tool` trait, `Registry`, `ToolCtx`, `ToolResult` (redact + 32 KB head/tail cap), `Artifact` (FileChange | Plan) so tools never touch the DB, `PermissionMode`, `requires_permission`, `is_dangerous_command`, `line_hash`/`content_hash` (SHA-256 via `ring`), `render_line` (`N:hash│text`).
- New: `src/tools/paths.rs` — `resolve(root, input)` canonicalizes the deepest existing ancestor (works for new files), rejects `..`, absolute paths outside root, symlink escapes, the harness data dir, and secret names (`.env*`, `*.pem`, `*.key`, `id_rsa*`, …). 4 unit tests.
- New: `src/tools/textdiff.rs` — dependency-free unified diff (LCS with a 4M-cell cap, then whole-file fallback). 3 unit tests. Exists so P1-T07 needs no `similar` crate (open question 4 → leaning "zero new deps").
- New: `src/tools/fs_tools.rs` — `Read`, `Grep`, `Glob` tools (P1-T06). `Grep` prefers `rg --json` (with secret globs excluded), falls back to a literal walk and says so. `Glob` is a hand-rolled `**`/`*`/`?` matcher, mtime-desc, ≤500 paths. 1 unit test.
- Edited: `src/tools/mod.rs` declares only modules that exist; T07–T09 modules are commented with their intended names (`edit_tools`, `bash_tool`, `meta_tools`).
- Edited: `src/main.rs` adds `mod agentic_sql; mod tools;` so the new files are part of the crate.
- Edited: `docs/TASKS.md` — P1-T05, P1-T06 → `needs-verify`; notes on T09/T10/T11 saying which parts already exist.

**Verified here.** `python3 -m unittest discover -s tests -p 'test_*.py'` → OK (16 + 2 + 7). `node --check static/app.js` → OK.

**Not verified.** All Rust in `src/tools/*` and `src/agentic_sql.rs` (no cargo in the sandbox). Expect small compile fixes: borrow of `pend`/`row` tuples in tests, `include_str!` paths (relative to `src/tools/`, so `../../tools/schemas/*.json`), and possibly unused-import warnings. `agentic_sql` will warn `dead_code` until P1-T10 uses it — acceptable.

**Decisions.**
- Tools return `Artifact`s; the loop persists them. Keeps `src/tools/*` free of `rusqlite` and unit-testable with a temp dir.
- One `fs_tools.rs` instead of `read.rs`/`grep.rs`/`glob.rs`: they share `read_text`, `walk`, `glob_match`.
- Hand-rolled diff + glob + SHA via `ring` → **no new crates so far**. Revisit if `regex` is wanted for grep fallback.

**Next (in order).** P0-T01 on the owner machine → fix compile errors in `src/tools/*` (journal them here) → P1-T07 `edit_tools.rs` (use `textdiff::unified`, anchors via `line_hash`, emit `Artifact::FileChange`) → P1-T08 `bash_tool.rs` → P1-T09 `meta_tools.rs` → P1-T03 provider adapter → P1-T10 loop.

**Resume prompt for an AI.** "Read AGENTS.md, then docs/TASKS.md. Pick the first `todo` whose deps are `done`/`needs-verify`. For Rust files marked needs-verify, run `cargo test --locked` first and fix errors before adding code. Journal every session in docs/PROGRESS.md."

---

## 2026-09-11 (session 3) — P7-T05 safe incremental generation publication

**Context.** Owner asked to run the autonomous dev loop and start P7-T05. First finding of the session: `cargo` was installed under `~/.cargo/bin` but absent from the non-interactive PATH, which is why earlier sessions recorded "no cargo in the sandbox" and parked Rust at `needs-verify`. Fixed by symlinking every `~/.cargo/bin` binary into `/usr/local/bin`, so `cargo` resolves in the server's bare `/bin/sh`. `cargo build --locked`, `cargo test --locked` (161 passed at the time), `cargo clippy --all-targets`, both Python integration suites, `node --check static/app.js` and `git diff --check` were all green before any edit.

**Changed.**
- Edited `src/safety.rs` — new `classify_line` holds the BEGIN / `in_key` / `sensitive` / keep ladder shared by `redact` and the new `StreamRedactor` so they cannot drift; `REDACTION_MARKER` const; `StreamRedactor` with `push` / `finish` / `pending_len`, `emitted`-guarded `\n` join so dropped key-body lines leave no blank line. 4 new unit tests: chunking equivalence over every single cut and char-wise, split-secret holdback, no-blank-line on a dropped key body, plus an abandonment test.
- Edited `src/memory_agents.rs` — `GenerationSink` is now async (`BoxFuture` alias, `text()` / `usage()` accessors so a sink's accumulated answer can be read back); `BufferedGeneration` implements them; `consume_stream_response` publishes completed lines as they arrive and flushes the final unterminated line only at `[DONE]`, and every `fail` call is awaited.
- Edited `src/recording.rs` — new `RecordingGenerationSink`: one `chunk` row committed **before** delivery, first failure recorded as `generation_stream_save_failed` and surfaced by `generate` through `fail_recording`; `complete` writes no row. `complete_recording` now writes the terminal `completed` row only, so the answer is not duplicated.
- Edited `src/agent_loop.rs` — `run` takes the sink as a parameter instead of constructing a buffer internally.
- Edited `static/app.js` — `chunk` events append to `.generation-content` via `textContent` in `seq` order; `complete` still renders the saved answer.
- Edited `src/recording_tests.rs`, `tests/recording_integration.py` — assertions updated from the fixed two-row model to "one `chunk` row per publication, terminal row last".

**Verified here.** `cargo test --locked` → 165 passed, 0 failed. `cargo test --locked streaming` → 9 passed. `cargo test --locked redact` → 5 passed. `python3 tests/test_incremental_publication.py` → 7 OK. `bash scripts/verify_release.sh` → PASS (60 SQL contracts, migrations 001→005, tool schemas). `node --check static/app.js` → OK.

**Not verified.** The browser fixture (`tests/recording_ui.cjs`) — still blocked by the `P7-T06` runtime gap (no `npm` / `npx` / Playwright on this host). The UI change is syntax-checked only.

**Decisions.**
- Publication unit is a **completed line**, never a provider chunk or token: `redact` drops a matched line whole, and durable append-only events cannot be retracted, so a partial line is never safe. Intra-line masking stays rejected (it would change redaction semantics and needs its own task).
- The terminal `completed` row carries no content now. Chunks are the answer's durable record; re-writing the answer at completion would duplicate it. `SELECT content ... WHERE state='chunk' ORDER BY seq` is unchanged and still equals `redact(full_answer)`.
- A publication failure ends the turn explicitly rather than silently truncating an answer the reader already saw.

**Next (in order).** P7-T05 is done on its branch; P7-T06 (browser suites) remains blocked on the runtime gap and needs `npm`/Playwright or a different host. Unrelated and still open: `development-mcp` has no git repository, and `harness` has grown monolithic (`agent_loop.rs` 118 KB, `main.rs` 85 KB, `storage.rs` 70 KB) — worth a task before more features land.

---

## 2026-09-12 (session 4) — P7-T06 browser suites executable

**Context.** Owner asked to install whatever the tests needed, after P7-T05 landed. P7-T06 had been `blocked` on a runtime gap: the host had `node`, `python3` and `git` but no `npm`, `npx` or Playwright module, so both browser fixtures failed at `require('playwright')`.

**Installed.** `npm` 9.2.0 (`apt-get install --no-install-recommends npm`), then `playwright` 1.63.0 plus Chromium 1243 and its system libraries in the project. `package-lock.json` and `node_modules/` were already git-ignored, so `package.json` is the only new tracked file.

**Changed.**
- Added `scripts/setup_browser_tests.sh` — installs the runtime, fails with a clear message when `npm` is missing.
- Added `scripts/verify_browser.sh` — runs both fixtures after resolving the browser via Playwright's own `executablePath()`, so nothing is hard-coded per machine; honours a `CHROMIUM_PATH` override.
- Edited `scripts/verify_release.sh` — runs the browser suites when `node_modules` exists, skips them with a notice otherwise.
- Edited `README.md` — documents the setup and the reason the suites sit outside the default gate.
- Added `package.json` — the Playwright dev dependency.

**Verified here.** `node tests/recording_ui.cjs` -> `passed`, 18 checks. `node tests/ui_smoke.cjs` -> `passed`, 24 checks. Both run through `scripts/verify_browser.sh` with the `/usr/local/bin/chromium` symlink removed, proving the documented path stands on its own. The release gate was re-run after the edit and stays green.

**Not verified.** The fixtures exercise a mocked API only — neither starts the compiled Rust service, so they are frontend evidence, not end-to-end evidence. That boundary is printed by the fixtures themselves.

**Decisions.**
- The browser suites stay out of the default `verify_release.sh` path but run automatically when the runtime is present. A bare host should not fail the release gate over an optional browser install, and a provisioned host should not silently skip coverage.
- `verify_browser.sh` resolves Chromium from Playwright instead of a fixed path, so the repo carries no machine-specific assumption.

---

## 2026-09-12 — tailnet origin allow-list

**Goal:** make the deployed instance usable from a tailnet browser, which the
origin guard was rejecting with `Origin not allowed`.

**Root cause:** `authenticate` compared the request `Origin` against three
loopback strings built from `HARNESS_ADDR`'s port. A browser reaching the app
through `tailscale serve` sends `https://<machine>.<tailnet>.ts.net:8443`, which
could never match, so every authenticated call failed regardless of a valid
token. The guard itself is correct — arbitrary origins must not be accepted,
since the page sends the bearer token.

**Change:** `Harness` gained `origins: Arc<Vec<String>>`, seeded with the same
three loopback defaults and extended by an optional comma-separated
`HARNESS_ALLOWED_ORIGINS`. The hardcoded inline array is gone; the comparison
now reads the configured list.

**Verified here.** `cargo test --locked` -> 166 passed, 0 failed (up from 165;
new `configured_origin_is_allowed`, existing `foreign_origin_is_rejected`
still passes). Release binary rebuilt and the unit restarted. Replayed the
exact failing request with the real browser origin -> 200; `https://evil.invalid`
-> 403; `http://127.0.0.1:8080` -> 200, so the defaults are intact.

**Decisions.**
- The extra origins come from configuration, not a hardcoded hostname. The
  tailnet name is deployment-specific and must not live in source.
- Loopback defaults stay unconditional: local use should never require config,
  and removing them would break the documented `cargo run` path.
- An unset or empty variable changes nothing, so the default posture is exactly
  as strict as before.

---

## 2026-09-12 (session 5) — chat submit contract fix and main consolidation

**Reported.** Sending a message failed with `Unexpected response (422)` and the composer kept the draft, so chat was unusable and no retry recovered it.

**Root cause.** The live binary was built at 17:49 from `20c90a3` (the P7-T06 tip), 79 seconds before the tree moved to `fix-tailnet-origin-allowlist`, and was never rebuilt. In that build `static/app.js` stores the draft identity with `generation_cursor` and spreads it into the `/chat/submit` body, but `ChatRequest` is `#[serde(deny_unknown_fields)]` and has no such field. axum answered the extractor rejection itself with a `text/plain` 422, which `api()` could not JSON-parse, so the UI fell back to `Unexpected response (<status>)` and held the draft. The cursor belongs to `GenerationQuery` (`/generation/stream?after_seq=`), never to the submit body.

**Changed.**
- `src/main.rs` — added `JsonBody<T>` implementing `FromRequest`, mapping `JsonRejection` onto `ApiError` so a JSON route can never answer `text/plain`; both chat handlers use it; the rejection detail is logged as `request_body_rejected` rather than shown to the reader.
- `static/app.js` — `PENDING_FIELDS` is both the set of fields `/chat/submit` accepts and the only thing put on the wire; a stored identity is sanitized and re-persisted on load, so `generation_cursor` survives for reload-resume but is never sent. A stale draft from another build now self-heals instead of wedging every retry.
- Consolidated onto `main`: fast-forwarded to `20c90a3`, then merged `fix-tailnet-origin-allowlist` (`e264d6e` origin allow-list plus the fix above). `docs/PROGRESS.md` was the only conflict; both journal entries were kept.

**Verified here.** `bash scripts/verify_release.sh` -> PASS: `cargo test --locked`, `cargo clippy --locked --all-targets`, `cargo build --locked`, 60 Python tests OK, migrations `001 -> 002 -> 003 -> 004 -> 005` (`user_version=5`), tool schemas 13 files, both mock-provider integration suites PASS, `node --check static/app.js`, and the browser suites (`recording_ui.cjs` 18 checks including `generation_cursor_resume` and `reload_recovers_without_resend`, `ui_smoke.cjs` 24 checks). Deployed with `systemctl restart harness` (PID 56749 -> 61297); the served `app.js` then matched the tree (`cde612f0`). Live probes: the exact failing payload and malformed JSON both return `400 application/json` `{"error":"Message payload was not accepted. Reload this tab, then send again"}`; an empty prompt still returns `Prompt must contain 1-16000 UTF-8 bytes`; the tailnet origin is accepted; a foreign origin still gets `403 Origin not allowed`; a missing token still gets `401 Bearer token required`.

**Confirmed by owner.** End-to-end chat, including the real provider round-trip, working on the restarted service (2026-09-12 09:07 +07). No provider turn was sent from the agent side.

**Next.** Six `Json<...>` extractors remain (`confirm`, `candidates`, scope patch, decision, memory ingest); converting them to `JsonBody` would make every JSON route answer JSON uniformly. `tests/integration_smoke.py:84` already accepts `400` or `422`, so that change needs no test edit. Also still open from earlier sessions: `development-mcp` has no git repository, and `harness` is monolithic (`agent_loop.rs`, `main.rs`, `storage.rs`).

---

## 2026-09-12 (session 5, follow-up) — uniform JSON rejections and branch cleanup

**Changed.** The six remaining body extractors in `src/main.rs` (`confirm`, `edit_candidate`, `set_config`, `set_scope`, `decide_permission`, `ingest_memory`) now use `JsonBody`, so no served route can answer a malformed body with axum's `text/plain` 422. One plain `Json` extractor remains in `src/agent_loop.rs`: it is the mock provider inside the test module, not a served route, so it stays.

**Branches.** Deleted seven fully merged topic branches locally and on origin: `p7-t05-incremental-publication` (0f4ad37), `p7-t06-browser-runtime` (20c90a3), `p7-frontend-generation-stream` (460f667), `p7-migration-reporting` (c7e35bf), `p7-generation-replay-attribution` (a758030), `autonomous-development-streaming` (554fa6f), `fix-tailnet-origin-allowlist` (965457d). Every SHA is reachable from `main`, so any branch can be recreated with `git branch <name> <sha>`. Kept `upcloud-verify` (not merged) and the `v1`, `v2-upgrade`, `v3` release lines. This removes the divergent tips that caused a build from a stale branch head earlier in the session.

**Verified.** `bash scripts/verify_release.sh` -> PASS (60 Python tests, migrations `001 -> 005`, both mock-provider integration suites, `recording_ui.cjs` 18 checks, `ui_smoke.cjs` 24 checks), then redeployed with `systemctl restart harness` and probed each converted route with a body no target type can accept.

---

## 2026-09-12 (session 5, follow-up 2) — a JSON body must be an object, and a deploy must prove itself

**Found while verifying the previous entry.** `POST /scopes/global` with a body of `[]` answered `200 application/json` with the stored row instead of `400`, and moved that row's `updated_at`. Not owner-reported: the probe was mine, and it was disclosed at the time. Reading the source proved no column value changed (the merge applied six `None`s); `created_at` stayed `2026-09-09T15:49:14`.

**Root cause.** serde's derive accepts the *sequence* form of a struct as well as the map form, and every field of `ScopePatch` is `#[serde(default)]`, so a zero-length array deserializes as a valid all-defaults patch. `#[serde(deny_unknown_fields)]` cannot catch it, because an array carries no field names to reject. The five other routes converted in the previous entry refused `[]` only by luck: their target types have required fields, so the sequence length did not match. Two defects, not one — the API accepted a body shape it never meant to accept, and a patch that named no field still rewrote a row.

**Changed.** `JsonBody<T>` in `src/main.rs` no longer delegates to `Json<T>`'s extractor: it checks the content type itself, buffers with `Bytes`, refuses any body whose first non-whitespace byte is not `{`, then parses with `Json::<T>::from_bytes`. The bound moved from `Json<T>: FromRequest<S, Rejection = JsonRejection>` to `T: DeserializeOwned`, so all eight extractor sites and any target type added later inherit the rule. `upsert_scope` in `src/storage.rs` returns the stored row untouched when `ScopePatch::is_empty()`, while still creating a scope that does not exist yet (`configured_scopes_are_listed_for_the_picker` depends on that). Rejection wording is now `Request body was not accepted. Reload this tab, then send again`, since it is no longer only about chat messages. `docs/design/agentic-turn.md` states the object rule and the `{}` semantics.

**The deploy path was lying.** The gate was green, the commit was pushed, `systemctl restart harness` reported success — and the live service still answered `[]` with `200` and the pre-fix wording. The unit starts `target/release/harness`, but `scripts/verify_release.sh` runs `cargo build --locked`, the debug profile. Nothing in the repo ever built the artifact systemd starts, so the restart faithfully relaunched the binary from `02:16:43`, which predated the fix. This is the same gap that produced the stale-binary incident earlier in the session, which was misread at the time as a branch problem. `scripts/deploy.sh` now owns deployment: it blocks unless the unit's `ExecStart` names the binary it is about to build, builds `--release`, restarts, waits for `active`, compares the md5 of `/proc/<MainPID>/exe` against the binary it just produced, then smoke-tests that the API answers and that a non-object body is still refused. A restart that deploys nothing can no longer look like a success. README documents the path and `verify_release.sh` now states which profile it validates.

**Verified here.** `bash scripts/verify_release.sh` -> exit 0: 169 Rust tests (new `non_object_json_bodies_are_refused_with_json`, `an_empty_patch_still_creates_a_missing_scope`, `an_empty_patch_leaves_the_stored_row_untouched`), 60 Python tests, migrations `001 -> 005` (`user_version=5`), 13 tool schemas, both mock-provider HTTP suites, `recording_ui.cjs` 18 checks, `ui_smoke.cjs` 24 checks. Then `bash scripts/deploy.sh` -> `deployed 5e48f1d to harness: pid 64619, release md5 bb0decb1934012666c827bc6435e6a2e, API answering, non-object body refused with 400`. Live probes against the deployed build: `[]`, `"ask"` and an empty body each return `400 application/json` `{"error":"Request body was not accepted. Reload this tab, then send again"}`, with `{"detail":"Body is not a JSON object","event":"request_body_rejected"}` in the journal; `{}` returns `200` with the stored row and `updated_at` still `2026-09-12T03:02:37.439006652`, unchanged across the post and a following read.

**Side effect, disclosed.** The two pre-fix probes moved the live `global` row's `updated_at` to `03:02:37.425` and then `.439`. No column value changed and `created_at` is intact; with the fix deployed, a body that names no field can no longer move it.

**Next.** `static/app.js` still lists `422` in its retry gate; no served route can produce one now, so it can go. The binary carries no version stamp, so `deploy.sh` proves identity by md5 rather than by commit — stamping the build would be better. Still open from earlier sessions: `development-mcp` has no git repository, and `harness` stays monolithic (`main.rs` is now ~92 KB). The local MCP bridge dropped twice today, once mid-deploy; the work resumed unchanged after waiting it out.


---

## 2026-09-13 — P14-T04a minimal fail-closed provider spend limits

**Delivered.** Added schema 8's append-only `provider_calls` ledger and reserve-before-dispatch accounting for normal model calls, streamed calls, compaction, verification, sub-agent work, and memory extraction. Request caps default to 32 per turn and 500 per UTC day; token and micro-USD ceilings are opt-in. Configured token/cost ceilings refuse when earlier usage is unavailable, cost ceilings require both input/output pricing, refusal reasons are durable and visible in activity, and startup converts abandoned reservations to explicit unavailable failures.

**Verified.** Exact gate: 22 provider-focused Rust tests and `tests/recording_integration.py` passed. Migration chain 001→008 passed at `user_version=8`. Strict non-deploying release gate passed: 189 Rust tests, clippy/build, 66 Python contracts, integration smoke, recording integration, JavaScript syntax, both mocked-browser suites, and the real browser-to-server success/denial/crash-recovery paths.


---

## 2026-09-13 — P13-T03 tool-security hardening started

**Scope.** Started browser destination/transfer policy, structured command and network/protected-path policy, and final write-time path ownership/TOCTOU hardening on `p13-tool-security` from `3873cd5`. Exact gate: `cargo test --locked tools && python3 tests/recording_integration.py`.


---

## 2026-09-13 — P13-T03 tool-security hardening completed

**Delivered.** Browser navigation now rejects credential-bearing and non-HTTP(S) URLs, loopback/private/link-local/metadata/special-use literal destinations by default, and revalidates the captured URL after redirects and interactions; private-network access is an explicit operator opt-in and no model-facing transfer operation exists. Bash permission evidence now classifies network and protected-path access, and those classes cannot pass `auto_all` without approval. Edit, write, AST edit, LSP workspace rename/rollback, and recorded-change revert now revalidate canonical parent containment and Unix ownership immediately before atomic replacement, with a symlink-swap regression.

**Verified.** Exact gate passed: 69 tool-focused Rust tests plus `tests/recording_integration.py`. Strict non-deploying release gate passed: Rust tests/clippy/build, Python contracts, integration smoke, recording integration, JavaScript syntax, both mocked-browser suites, and real browser-to-server success/denial/crash-recovery paths. `git diff --check` passed.

---

## 2026-09-13 — P14-T01 durable cancellation started

**Scope.** Started schema-backed cancellation intent, cooperative turn/permission cancellation, and retry lineage that is accepted only when recorded tool evidence proves no side-effecting call completed or remained uncertain. Work is isolated on `p14-durable-cancellation`; no deployment is part of this tranche.

---

## 2026-09-14 · P11-T01 — Wake streams from committed events

**Delivered.** Replaced 200 ms per-connection SQLite polling with commit notifications (`tokio::sync::broadcast`) plus cursor replay. Activity and generation streams wait via `tokio::time::timeout` utilizing `commit_notify.subscribe()` channel from `DbStore`. Idle stream heartbeat logic is intact, and transient database contentions correctly hold cursor and resume efficiently.

**Verified.** `cargo test --locked streaming` and `python3 tests/recording_integration.py` pass without errors. `cargo test --locked -p harness` gives 207 passes.

---

## 2026-09-14 · P12-T04 — Zero clippy warnings and a gate that keeps them at zero

**Delivered.** `cargo clippy --locked --all-targets --all-features -- -D warnings` passes for the first time in this repo, down from a 42-warning baseline. Real fixes: an orphaned activity-feed doc comment that had drifted onto `reserve_provider_call` was reattached to `activity_since`; a no-op `drop()` removed; `process_lock.rs` made truncation explicit with `.truncate(false)`; `as_chunks::<4>()` in `embeddings.rs` removed an infallible `try_into().unwrap()`; `div_ceil` replaced a manual round-up; five `Budgets` test setups became struct literals. Deployed as 97f39c6.

**Gate hardening.** `scripts/verify_local.sh` ran plain `cargo clippy --locked --all-targets`, so 42 warnings accumulated while the suite reported `[PASS]`. It now runs `--all-targets --all-features -- -D warnings`. This closes a real gate defect found earlier in the same session: `cargo test --locked --no-run | grep unused` cannot see unused imports consumed only by `#[cfg(test)]` code, and four such warnings survived five refactor seams because of it.

**Deviation worth stating plainly.** About 26 warnings were silenced with documented `#[allow(dead_code)]`, not fixed. `src/archive/` (P13-T02) and the DbStore retention/maintenance surface are implemented and test-covered but reachable from no route. `git grep` at fdd830a proved the retention group was already unreachable before the P12-T01 routes seam, so it is pre-existing debt rather than seam fallout. Connecting them is a routing change, now tracked as P13-T02b, which requires removing those allows. The provenance validation chain is test-only, but production inserts go through raw SQL in `agent_loop::steps` where migration 006 CHECK constraints enforce the same kinds, relations, id lengths and self-edge ban, so no correctness hole exists.

**Verified.** clippy `-D warnings` exit 0; `cargo test --locked` 217 passed, 0 failed; `scripts/verify_release.sh` exit 0 with 12 `[PASS]` under the hardened gate; `git diff --check` exit 0. Deploy verified independently of the deploy script: served `/health` reports commit 97f39c6 matching HEAD, `binary_sha256` b6f6fa88 matching local `sha256sum`, schema 10, ready, `quick_check: ok`, recording and extraction workers up, unit active, rollback snapshot capturing 805e648.

**Next.** P12-T04 stays `doing`. Four done-when clauses are untouched: shared limits for magic values, checked numeric casts, explicit nested patch-option semantics, and the production-panic audit.

## 2026-09-14 · P12-T04 — The panic audit, and why 842 was the wrong number

The done-when clause said "production panics are audited". The obvious way to
start is `grep -rn 'unwrap()' src`, which returns 842 hits. That number is
useless: grep cannot tell a production call site from one inside a
`#[cfg(test)]` module, and this repository keeps most of its tests inline
beside the code they cover. Acting on 842 would have meant either a
multi-thousand-line churn commit or, more likely, giving up.

So the audit needed a parser, not a pattern. `/tmp/panic_audit.py` walks each
file tracking brace depth, remembers the depth at which a `#[cfg(test)]` module
or a `#[test]`/`#[tokio::test]` function opened, and strips string literals and
line comments first so that braces inside them cannot corrupt the depth count.
Result: **21 production sites and 841 test sites.** `src/recording_tests.rs` is
entirely tests; `main.rs`, `agent_loop.rs` and `storage.rs` earn their large raw
counts almost wholly from test modules.

That is the same lesson the auth seam taught when the compiler found three
`E0624` errors a grep had missed, and the same one the clippy gate taught when
`--all-targets` without `-D warnings` reported `[PASS]` over 42 warnings: a
grep count is a hypothesis. Twenty-one sites is small enough to read every one
in context, which is what an audit actually is.

Reading all 21 showed none were reachable-and-broken today. But the distinction
worth acting on is not "does it panic" — it is **how far away the invariant
lives from the code depending on it**:

- `api/routes.rs:99` was the one genuine concern: `receipt["request_id"]
  .as_str().unwrap()` directly inside the compatibility `/chat` handler.
  `admit_chat` does populate the field, but that is a cross-module promise, and
  a JSON `unwrap()` in a live HTTP handler converts a broken promise into a
  panicked request. It now returns the durable 202 receipt instead; a receipt
  that cannot be polled is still a correct answer.
- `recording.rs:244,259` read a receipt back inside the very transaction that
  wrote it. `None` means the database contradicted itself — worth an error the
  caller records, not a panic unwinding the DB worker mid-transaction.
- `storage.rs:842` was safe *only* because `cache_hit` included
  `decoded.is_some()`, three lines up. Matching on `decoded` makes the compiler
  enforce what an adjacent boolean used to promise.
- `memory_agents.rs:97,99` were correct only because of an argument on the
  previous line (`Some(32)`); `unwrap_or(32)` states the default at the point
  of use.
- `auth.rs` ×5 parsed static header strings. `HeaderValue::from_static` moves
  the check to compile time, so the panic branch stops existing rather than
  being merely unreachable.
- `ingest.rs:132` was guarded by `starts_with("```")` on the line above;
  `if let Some(kind)` makes the guarantee local and costs nothing.

Twelve removed, **21 → 9**. The nine kept are a decision, not an oversight: 8
validated-before-dispatch arms in `tools/browser_tool.rs` and one
`expect("inserted above")` in `lsp_tool.rs`. Making browser dispatch return
errors reshapes the call path — a behaviour change, which is exactly what a
cleanup task must not smuggle in.

One verification detail mattered more than the code: rewriting the five header
parses is the kind of "obviously equivalent" edit that silently drops a header.
Unit tests would not catch it, so after deploy the hardening headers were read
back off the wire from the running process — `cache-control`, `nosniff`,
`referrer-policy` and the full CSP all still served. Evidence, not equivalence
by inspection.

Gate: clippy `--all-targets --all-features -D warnings` exit 0; 217 tests pass;
`verify_release.sh` exit 0 with 12 `[PASS]`. Deployed 4d3f589 (pid 300082,
sha256 f615c826…, schema 10) and confirmed `/health` `commit` and
`binary_sha256` match HEAD and the local binary.

P12-T04 stays `doing`: three clauses remain — shared limits for magic values,
checked numeric casts, and explicit nested patch-option semantics.
