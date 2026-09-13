# Design: causal observability for agent executions

## Thesis

Ordinary traces answer **what happened**. Harness should answer **what enabled it, what it changed, and what upstream fact made the failure possible**.

The unit is a typed causal edge, not just a timestamped span:

`memory/evidence → model claim → tool call → permission → observation → state mutation → recovery`

## Scope

Start with one coding turn and one incident. Do not build a general-purpose graph database or expose hidden chain-of-thought. Record only externally inspectable inputs, outputs, decisions, permissions, tool results, durable state changes, and explicit recovery actions.

## Minimal provenance vocabulary

| Edge | Meaning |
|---|---|
| `supports` | Evidence or memory supports a claim or tool argument |
| `contradicts` | Evidence conflicts with a memory, claim, or proposed action |
| `depends_on` | An action consumes a prior observation or decision |
| `authorizes` | A human or policy decision permits a side effect |
| `mutates` | A tool changes an external or durable state |
| `invalidates` | New evidence makes a prior memory or plan unsafe to use |
| `triggers` | An observation causes retry, denial, interruption, or repair |

## Durable edge contract

Migration 006 adds `provenance_edges`. Each edge is request-scoped, capped at 2,000 per request,
and contains only two durable row references, one relation, and a timestamp. Node kinds resolve as:
`evidence` to a same-scope `sources` row, `step` to `turn_steps`, `permission` to
`permission_requests`, `mutation` to `file_changes`, `memory` to a same-scope `memories` row, and
`recovery` to that request's `activity_events(kind='interrupted')` row. A trigger rejects unknown,
cross-request, and cross-scope endpoints because SQLite has no polymorphic foreign key. Delete
guards give these references foreign-key-like `RESTRICT` behavior after insertion.

There is intentionally no freeform explanation or reasoning column. The edge is an inspectable
provenance assertion over already-recorded artifacts, not a place to persist hidden reasoning.

## First experiment

Inject one stale memory or stale file anchor into a recorded coding turn. Compare the normal trace with the causal graph and measure whether the graph identifies the earliest invalidating node, the affected tool call, and the recovery action without rereading the full trace.

## Guardrails

- No private chain-of-thought capture.
- Every edge points to a durable row or bounded evidence receipt.
- Missing provenance is visible as `unknown`, never inferred as support.
- Redact before persistence and preserve the existing permission boundary.
- A graph is diagnostic, not proof of internal model causality.

## Success criteria

For a denial, stale anchor, or crash-recovery run, a reviewer can navigate from the incident to the exact request, step, evidence, permission, state mutation, and recovery event. The system identifies the earliest known causal break and distinguishes missing evidence from contradictory evidence.

## Bounded projection integrity

The read model is a closed bounded graph, not independently truncated node and edge lists. It selects at most 400 nodes, retains only edges whose source and target are both selected, rebuilds adjacency from those retained edges, and reports whether either collection was truncated. Therefore every identifier exposed through an edge, an upstream/downstream list, or `earliest_known_break.node_id` resolves to a node in the same response.

Selection uses deterministic `causal-neighborhood-v1` ordering. The earliest recorded break is the seed; breadth-first traversal over recorded edges keeps its nearest causal neighborhood before remaining capacity is filled in durable row order. If no break is recorded, the request node is the seed. The response reports total, returned, and omitted counts separately for nodes and edges. When anything is omitted it also returns an opaque continuation anchor under `expansion_cursors`; clients must pass that object back unchanged when a paginated incident endpoint is introduced rather than constructing row offsets or trusting mutable counts.
