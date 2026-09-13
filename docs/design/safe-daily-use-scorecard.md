# Safe-daily-use scorecard

Status: defines the release decision for the safe-daily-use milestone (SDU) in
`docs/ROADMAP.md`. This file contains **no invented thresholds and no results**. Every
target cell stays `owner-signed: pending` until the owner sets it from an observed
baseline. A release decision that cites this file without owner signatures is invalid.

## 1. Release boundary

SDU is the first acceptable daily-use release. It is bounded to:

- truthful strict release verification (P12-T05a);
- schema-compatible rollback and crash/disk/fault recovery (P10-T05, P10-T06);
- durable cancellation at a non-mutating boundary (P14-T01);
- browser/command/path tool policy (P13-T03);
- minimal fail-closed per-turn/day/role cost limits (P14-T04a).

Everything else (performance, refactors, dashboards, voice, research, ecosystem) is
outside SDU and is not a release blocker for it.

## 2. Rules

1. Do not write a number in the Baseline or Target columns until it is observed or
   owner-signed. `TBD` is a valid and required placeholder.
2. Every metric names its evidence source; missing evidence is `unavailable`, never `0`.
3. The owner signs the target row-by-row, with a date and the commit that produced the
   observed baseline.
4. A release is approved only when every mandatory metric has a signed target, an observed
   baseline, and recorded evidence for the candidate build.

Signature: `owner: ______  date: ______  baseline commit: ______  decision: go | no-go`

## 3. Metrics

| # | Metric | Definition (what is counted) | Evidence source | Mandatory | Baseline | Owner target |
|---|---|---|---|---|---|---|
| 1 | Task success | Representative coding fixtures whose deterministic acceptance passes | fixture runner + test exit codes | yes | TBD (observed) | owner-signed: pending |
| 2 | Unsupported completion claims | Answers asserting done when the cited evidence is absent or failed | verifier + turn review | yes | TBD (observed) | owner-signed: pending |
| 3 | Stale-edit rejection | Edits rejected because the hash anchor no longer matches | tool receipts (`file_changes`) | yes | TBD (observed) | owner-signed: pending |
| 4 | Unauthorized mutations | Mutating tool actions that ran without approval or outside policy | permission records + policy tests | yes | TBD (observed) | owner-signed: pending |
| 5 | Restart/retry duplication | Side effects repeated after restart or retry | recovery tests + step ledger | yes | TBD (observed) | owner-signed: pending |
| 6 | Recall usefulness | Retrieved memories the owner marks useful vs noise | owner feedback on recall | no | TBD (observed) | owner-signed: pending |
| 7 | Latency | Per-turn wall time (p50/p95) on the fixture set | timing receipts | yes | TBD (observed) | owner-signed: pending |
| 8 | Cost per completed task | Provider spend divided by accepted tasks | provider usage receipts | yes | TBD (observed) | owner-signed: pending |

## 4. Representative fixtures

Fixtures are small, deterministic coding tasks with a compile/test acceptance and no
personal secrets. The exact fixture set is chosen by the owner; until then this scorecard
does not name one. P17 research fixtures are separate and are not required for SDU.

## 5. Decision

SDU release = all SDU gate tasks `done` in `docs/TASKS.md` + `scripts/check_plan.py`
passing + every mandatory metric owner-signed with an observed baseline. No green gate,
score or threshold is asserted in this file.

## 6. Open owner decisions (carried from the plan review)

- Which optional features are explicitly outside the first daily-use release?
- What evidence is mandatory for release versus merely useful for local development?
- Which schema/data changes are reversible by binary rollback, and which need an approved
  restore procedure?
- Where do production archive/backup keys live, what is the retention policy, and how are
  off-host copies managed?
