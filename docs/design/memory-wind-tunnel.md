# Experimental design: Memory Wind Tunnel

Status: proposed experiment, not product scope.

## 1. Research question

When does a specific memory help, harm, or merely change a coding agent's behavior?

The experiment must measure causal impact, not just whether a memory was retrieved. A run is replayed with the same task, model configuration, tool results, filesystem state, and budget while changing only the memory condition.

This is not a claim that memory ablation is novel by itself. Prior work already studies selective deletion, causal memory influence, selective forgetting, stale-memory failure, and memory benchmarks. The defensible contribution is a reproducible, evidence-bound laboratory for **personal coding-agent runs**, with durable replay, per-memory interventions, and tool/action outcomes.

## 2. Harness role versus LLM role

The harness is the experiment controller:

1. Capture the original run before model execution.
2. Freeze the task, initial repository snapshot, model settings, tool results, clock, random seed where available, and budgets.
3. Build a treatment memory state.
4. Run the same agent protocol under that state.
5. Record every prompt, retrieved memory, tool call, permission, file change, test result, cost, and terminal outcome.
6. Compare treatment against the baseline and report uncertainty across repeated trials.

The LLM is the subject under test. It chooses what to say and which available tool to call, but it does not control the memory mutation, replay condition, evidence ledger, or scoring rules.

## 3. Experimental unit

A `run capsule` contains:

- task prompt and sanitized session history;
- project snapshot or clean worktree reference;
- model/provider identifier and parameters;
- initial memory package with stable memory IDs and revisions;
- exact initial context receipt;
- tool schemas and permission mode;
- recorded tool results for strict replay;
- outcome assertions and test commands;
- redacted trace, hashes, and provenance.

Parents are immutable. Each treatment is a child capsule that points to the same frozen prefix and records one intervention. Never mutate the user's live project or approved memory during an experiment.

## 4. Treatment matrix

Start small. One captured run should produce these conditions:

| Condition | Memory state | Purpose |
|---|---|---|
| A | original approved memories | baseline |
| B | no recalled memories | total memory effect |
| C | remove one memory | marginal effect of a memory |
| D | remove one memory family | preference, project, rule, decision, procedural, episodic |
| E | stale version substituted | stale-memory harm |
| F | conflicting version substituted | update/conflict handling |
| G | irrelevant but similar memory added | retrieval pollution |
| H | poisoned memory added in an isolated fixture | safety and recovery |

Do not claim causality from one LLM run. Run each condition multiple times when the provider is live. Strict replay is for debugging and structural comparison; live repeated trials are for behavior distributions.

## 5. Metrics

### Primary

- task success: deterministic test/acceptance result, not an LLM judge alone;
- unsafe or unauthorized tool actions;
- memory-induced regression: baseline passes, treatment fails;
- stale-use rate: an obsolete memory affects the answer or action;
- intervention effect: paired difference against the same baseline task.

### Secondary

- tool-call sequence and unnecessary calls;
- files changed and diff size;
- diagnostics/test failures;
- wall time, model calls, tokens, and estimated cost;
- answer quality and user preference, judged separately from execution success;
- confidence calibration and unsupported claims;
- retrieval precision, memory token cost, and context-budget share;
- effect interactions, such as two memories that are harmless alone but harmful together.

Every metric must identify its source: deterministic assertion, tool receipt, provider usage, or human/LLM judgment. Missing evidence is `unavailable`, never a silent zero.

## 6. Operation flow

### Phase 0: capture

Run a real coding task through the normal harness. Before generation, save the context receipt and memory IDs. During execution, persist every step and tool result. At completion, run the project's diagnostics and acceptance tests, then seal the capsule.

### Phase 1: validate the capsule

Verify hashes, schema version, memory references, tool-result ordering, and that the clean worktree can be restored. Reject incomplete capsules. A capsule with an unrecorded nondeterministic boundary must say so explicitly.

### Phase 2: create treatments

A treatment is a declarative manifest, for example:

```json
{
  "parent_run": "run-id",
  "intervention": {"type": "remove_memory", "memory_id": "mem-id"},
  "replay_mode": "strict",
  "assertions": ["tests_pass", "no_secret_output"]
}
```

The memory store is copied into an isolated child state. The source memory and live database never change.

### Phase 3: replay

Strict mode reuses recorded provider and tool responses, so it answers: "Did the changed memory alter the agent's decision path under identical observations?" Live mode re-executes the model against a clean project and real provider, so it answers: "Does the effect survive model variance?" Hybrid mode replays the prefix and resumes live at a chosen decision boundary.

### Phase 4: compare and explain

Align runs by semantic step identity, not array position. Report the first divergence, changed memories in context, changed tool calls, file/test outcomes, and confidence intervals across live repetitions. Never say a memory caused an outcome solely because it was retrieved; require an intervention effect.

## 7. End-to-end testing strategy

Mocked browser tests stay as fast UI contract tests, but they cannot be the release proof. Add a separate real E2E lane:

### E2E-local, required on every change

- build the actual Rust binary;
- start the actual HTTP server on a random loopback port;
- use a real temporary SQLite database and real filesystem worktree;
- use the actual memory extraction/recall, context receipt, agent loop, permission gate, tools, activity stream, and generation stream;
- drive the browser against the running server, not route.fulfill mocks;
- assert the database, filesystem, HTTP responses, streamed events, and rendered UI together;
- kill and restart the real process during a treatment and prove no side effect re-runs.

The provider boundary may use a deterministic local OpenAI-compatible server for repeatability, but it must be a separately running process speaking HTTP. Do not replace the Rust provider adapter, database, tools, or memory store with test doubles.

### E2E-live-provider, opt-in or nightly

- use a real provider endpoint and a dedicated low-cost model;
- set a hard request/token budget and fail closed when exceeded;
- use a fixed fixture repository and no personal secrets;
- run only read-only or disposable edits by default;
- record model/version/parameters and mark results nondeterministic;
- repeat each treatment enough to report a distribution, not a single score.

### E2E-UpCloud, later research lane

Use an ephemeral UpCloud VM only after the local lane is green. Create it from a pinned image, install the built artifact, run one capsule, upload only a sanitized result bundle, then destroy the VM in a finally-style cleanup path. The token must be injected through the environment or secret store, never written to a capsule. Add a maximum spend, TTL, and a kill switch before enabling this lane.

## 8. Roadmap

### M0: experiment contract

- Define capsule schema and content-addressed IDs.
- Define strict, live, and hybrid replay semantics.
- Add deterministic outcome assertions and an explicit `unavailable` state.
- Decide the first coding benchmark: small Rust bugfixes with compile/test acceptance.

Exit: one manually inspected capsule can be restored and validated without changing live state.

### M1: freeze and fork

- Add immutable run manifest and child-treatment table.
- Snapshot the project into a clean temporary worktree.
- Copy approved memories into an isolated treatment store.
- Implement remove-one-memory and no-memory treatments.

Exit: two strict replays differ only when the treatment memory differs; source DB and source worktree hashes remain unchanged.

### M2: strict Memory Wind Tunnel

- Add CLI/API commands: `capsule create`, `capsule verify`, `treatment create`, `replay`, `compare`, `report`.
- Align traces by step identity and show first divergence.
- Add deterministic tests for memory inclusion/exclusion, context receipts, tool calls, permissions, diffs, and acceptance results.

Exit: a captured run becomes a checked-in regression fixture with a reproducible comparison report and zero live model calls.

### M3: real E2E browser lane

- Add `scripts/verify_e2e.sh` and a browser test that talks to the compiled server.
- Keep mocked UI suites for fast feedback, but label them clearly as non-server tests.
- Run the local provider as a separate process and assert real HTTP request/response capture.
- Add restart, permission approval, stale-anchor, stream-resume, and treatment-isolation scenarios.

Exit: release verification includes one real browser-to-Rust-to-SQLite-to-filesystem run.

### M4: live variance and research reports

- Add repeated live runs with budget limits.
- Add paired statistical summaries and effect confidence intervals.
- Add stale, conflict, pollution, and poisoned-memory treatments.
- Export a sanitized report with raw evidence links and limitations.

Exit: the report can distinguish "memory changed the trace" from "memory improved the task" and "the observed effect is too noisy to conclude."

### M5: optional UpCloud isolation

- Add a disposable VM runner behind an explicit opt-in flag.
- Pin image, region, VM size, TTL, and cleanup behavior.
- Add cost ledger and emergency cleanup command.
- Run only sanitized capsules and verify destruction after collection.

Exit: one remote experiment completes with a bounded bill, no leaked token, and a confirmed destroyed VM.

## 9. Success factors

The experiment succeeds if it can:

1. reproduce the same strict run from a sealed capsule;
2. prove the intervention changed only the declared memory condition;
3. show exactly where the agent's trace first diverged;
4. connect divergence to tool behavior and deterministic task outcome;
5. survive process restart without duplicate side effects;
6. report uncertainty for live runs instead of overclaiming;
7. preserve the user's real memory and project through isolation;
8. remain useful even when the result is "this memory did not matter."

The killer result is not "memory improved score by 4%." It is a matrix showing which memory types help which coding tasks, which memories are actively harmful, and which effects disappear under model variance.

## 10. Research boundaries and prior art

This proposal should not claim firstness for selective forgetting, memory deletion, causal memory influence, stale-memory benchmarks, or replay in the abstract. Relevant prior work includes:

- Xiong et al., *How Memory Management Impacts LLM Agents* (2025/2026): controlled addition/deletion and error propagation.
- Hu et al., *MemoryAgentBench* (2025): selective forgetting as one of four memory competencies.
- Tan et al., *MemAudit* (2026): counterfactual memory influence for post-hoc auditing.
- Uddin et al., *Memora* (2026): forgetting-aware accuracy for obsolete memories.
- Chao et al., *STALE* (2026): implicit conflict and stale-state handling.
- Zhang et al., *Useful Memories Become Faulty When Continuously Updated by LLMs* (2026): consolidation can degrade useful memory.

The honest novelty target is narrower: an open, durable, end-to-end experimental harness for **personal coding-agent memory interventions**, with strict replay, live variance, tool-side effects, and evidence-bound reports in one workflow.
