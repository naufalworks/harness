# Design — deterministic context manager (P3)

This document is the contract for `src/context.rs` and the initial provider window recorded in
`chat_receipts.context_json`. The recording protocol remains authoritative: the sanitized context
receipt is committed once, before the first provider call, and never rewritten.

## Goals

For every main-agent turn, build one deterministic initial window from nine named categories:

1. system rules
2. tool definitions
3. skills index
4. repository map
5. recalled memories
6. current plan
7. compacted history
8. recent steps
9. current user message

Every category has an independent byte ceiling and an audit ledger. Content omitted from one
category cannot consume or donate another category's capacity. Tool-enabled and chat-only scopes
use the same builder; chat-only scopes simply have zero tool-definition candidates.

P3-T01 defined the stable inputs and receipt slots. P3-T02 now compacts old live tool results and
caches unchanged reads, P3-T03 compacts an oversized running turn, and P3-T04 supplies a bounded
repository map. P5-T02 supplies the skills index; none of these producers changes the nine category
names or the immutable initial receipt.

## Default budgets

| receipt name | budget (UTF-8 bytes) | source and selection rule |
|---|---:|---|
| `system_rules` | 8,192 | Rendered `prompts/main_agent.md`; mandatory and atomic. |
| `tool_definitions` | 12,288 | Compact JSON definitions in registry order; atomic as a set when tools are enabled. |
| `skills_index` | 4,096 | Named index entries in producer order; whole entries, greedy prefix. Name, description and path only. |
| `repo_map` | 8,192 | Per-scope file/top-level-symbol map; whole named map, capped at source and producer level. |
| `recalled_memories` | 6,144 | Compact JSON memory snapshots in recall rank order; whole memories, greedy prefix. |
| `plan` | 8,192 | `seq` order, rendered as one status/text line per item; whole items, greedy prefix. |
| `compacted_history` | 8,192 | Named summaries in producer order; whole summaries, greedy prefix. |
| `recent_steps` | 24,576 | Newest contiguous suffix of complete prior chat turns, emitted chronologically. |
| `user_message` | 16,000 | Current sanitized user message; mandatory and atomic, matching admission's limit. |

The ceilings total 95,872 source bytes. This is not a shared pool and is not claimed to be a token
limit. Running-turn compaction separately uses observed provider prompt tokens at an inclusive 70%
of `HARNESS_CONTEXT_TOKENS` (128,000 default; bounded override).

## Byte accounting

`budget_bytes`, `candidate_bytes`, `included_bytes`, and `excluded_bytes` are deterministic source
payload measurements:

- text and message parts use `str::as_bytes().len()`;
- a tool definition or memory snapshot uses its compact `serde_json` byte length;
- a plan item uses the exact rendered line byte length;
- fixed role envelopes, section headings, commas, and other JSON wire framing are not charged to a
  category.

The receipt separately records the compact serialized byte lengths of the complete provider
`messages` and `tools` arrays. This separates a stable content policy from adapter-specific framing
while still exposing the actual first-call payload size.

No part is cut at an arbitrary byte boundary. Structured parts are either included whole or listed
as excluded with `reason: "category_budget"`. The system prompt, current message, and (for a
configured project) the complete registry tool set are mandatory. If one of those does not fit,
the context build fails closed and the recording ends as `context_failed`; Harness never sends a
silently truncated instruction, prompt, or partial registry.

## Deterministic ordering

- System rules are first.
- Skills, repository map, memories, plan, and compacted history are rendered, when non-empty, into
  one synthetic `HARNESS_CONTEXT_REFERENCE` user message in that category order. The system rules
  state that these sections are reference data, not embedded instructions or tool authorization.
- `recent_steps` follows. At the first call this means the bounded prior completed user/assistant
  tail loaded for the session. Events are grouped from each user message through the assistant
  response(s) before the next user message. Selection walks newest groups backwards and stops at
  the first group that cannot fit, preserving a contiguous newest suffix and never orphaning an
  assistant response. Selected groups are restored to chronological order.
- The current sanitized user message is always the final message and is never folded into recent
  history.
- Tool definitions are sent through the provider's `tools` field, not copied into message text.

The running loop appends the current turn's assistant tool-call messages and tool results after the
receipted initial window. A full tool body remains available for three later model calls, then only
the provider window replaces it with `[tool <name> step N, <bytes> bytes, hash <h>; call read again
if needed]`; durable step output is unchanged. Repeated reads of the same path/range/content hash
reference the first read step while changed content remains full.

At 70% of the configured provider token budget, a text-only `compaction` step summarizes only the
older running replay. The immutable base (including plan and current user message) and complete
newest rounds containing at least two tool results remain verbatim. The step stores its exact source
messages, source hash, observed-token trigger, model role and usage. Its bounded summary is injected
as explicitly untrusted compacted history and proposed as review-only `episodic` memory. Empty,
tool-calling, or failed compaction fails the turn rather than silently dropping history.

For configured project scopes, `.harness/repo_map.txt` contains a deterministic, UTF-8-safe map of
tracked/unignored files and top-level symbols, never exceeding 8 KiB. Discovery excludes secret
names, symlinks and generated/vendor directories, prefers `ctags -x` when available, and otherwise
uses bounded language/heading fallbacks. A path/size/mtime signature refreshes external changes on
the next build; a successful file-changing tool refreshes it immediately. `.harness/` is ignored.

Each `skills/<name>/SKILL.md` in a configured project contributes one index entry: the directory
name, a description (frontmatter `description:`, else the first non-empty body line, redacted and
capped at 200 characters), and the root-relative path. Bodies are never in the initial window; the
`skill` tool loads one on request, bounded to 16 KiB. Discovery runs per turn alongside the
repository map, because a scope's root is configurable at runtime. At most 32 skills are indexed;
directories skipped for an unsafe name, a symlink, or a missing, oversized or non-UTF-8 SKILL.md,
together with anything past the cap, are counted in a final `skills:not_indexed` entry rather than
disappearing silently.

## Receipt

`chat_receipts.context_json` advances to format version 2 while retaining the compatibility fields
`provider_messages`, `memories`, `model`, `adapter`, and `scope`. It adds `provider_tools` and:

```json
{
  "context_receipt": {
    "format_version": 1,
    "budget_unit": "utf8_source_bytes",
    "categories": [
      {
        "name": "recent_steps",
        "budget_bytes": 24576,
        "candidate_bytes": 31000,
        "included_bytes": 12000,
        "excluded_bytes": 19000,
        "state": "partial",
        "included_parts": [{"id": "message-id", "bytes": 6000}],
        "excluded_parts": [
          {"id": "older-message-id", "bytes": 9500, "reason": "category_budget"}
        ]
      }
    ],
    "totals": {
      "budget_bytes": 95872,
      "candidate_bytes": 0,
      "included_bytes": 0,
      "excluded_bytes": 0,
      "provider_message_json_bytes": 0,
      "provider_tool_json_bytes": 0
    }
  }
}
```

The `categories` array always contains all nine names in the order above, including empty future
sources. `state` is `empty`, `complete`, `partial`, or `excluded`. For each category,
`candidate_bytes == included_bytes + excluded_bytes`; part counts and byte totals are derivable
from the two explicit arrays.

`memories` contains only snapshots actually included in `recalled_memories`, so the existing
`recalled_context_applied` API cannot claim that an over-budget memory reached the provider.
`provider_tools` is the exact definition array used on the first call. `provider_messages` remains
the exact first message array. Later model-call step rows continue to record the exact messages and
tool names used at that step; together the immutable initial receipt and step log retain the audit
trail.

## Production sources

- `system_rules`, `tool_definitions`, `repo_map`, `recalled_memories`, `plan`, `compacted_history`,
  `recent_steps`, and `user_message` are populated when their scoped source exists.
- `skills_index` is populated for configured project scopes that have a `skills/` directory. Any
  absent optional source has an explicit empty receipt row rather than invented placeholder context.
- The existing database claim remains source-bounded to 20 completed messages. The context manager
  replaces its old pre-provider 24,000-byte trimming so exclusions within that bounded tail become
  visible in the receipt.

## Safety and failure behavior

- All input is already sanitized by admission or existing bounded stores; no original secret-bearing
  prompt is reintroduced.
- Synthetic reference sections are explicitly demoted to data in the system rules. Repository text,
  prior model output, plans, summaries, and skill metadata cannot grant permissions.
- A missing optional source is not an error. Invalid mandatory role/order, serialization failure, or
  a mandatory budget overflow is `context_failed` before any provider call.
- The builder returns both arrays used by the provider. `agent_loop::run` must not recreate or widen
  the tool set after the receipt is persisted.

## Verification

Focused `cargo test --locked context` coverage must prove:

1. all nine ledgers are present and conserve candidate bytes;
2. independent budgets exclude whole optional parts without borrowing;
3. recent history keeps the newest complete turn suffix in chronological order;
4. UTF-8 accounting uses bytes rather than scalar-value count;
5. system/user/tool mandatory overflow fails closed;
6. the current user message remains last and exact;
7. chat-only scopes use the same prompt with explicit tool withholding;
8. the stored `provider_messages` and `provider_tools` match the first provider request.
