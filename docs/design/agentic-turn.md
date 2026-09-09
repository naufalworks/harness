# Design — the agentic turn (P1/P2)

This document is the contract for `migrations/003_agentic.sql`, `src/agent_loop.rs`,
the tool registry and the P1 API. `docs/RECORDING_PROTOCOL.md` still governs admission,
receipts, sessions and the memory outbox; nothing here changes those.

## Scopes

A scope now owns a project.

| column | meaning |
|---|---|
| `scope` | existing label (`safety::scope` rules) |
| `root_path` | canonical absolute directory; NULL means "chat only, tools disabled" |
| `permission_mode` | `ask` (default) · `auto_edit` · `auto_all` |
| `diagnostics_cmd` | optional shell command run after `edit`/`write` (e.g. `cargo check -q`, `npx tsc --noEmit`) |
| `max_steps`, `max_tool_bytes`, `max_wall_seconds` | per-scope budgets; NULL = defaults 40 / 400000 / 900 |

`GET /scopes/{scope}` → row or 404. `POST /scopes/{scope}` body `{root_path?, permission_mode?, diagnostics_cmd?, max_steps?, max_tool_bytes?, max_wall_seconds?}`; the server canonicalizes `root_path`, requires it to exist and be a directory, rejects paths inside the harness data dir.

## Schema (003_agentic.sql)

Additive. `PRAGMA user_version=3`. `DbStore::init` accepts versions 0..3 and applies 003 when `< 3`.

- `scopes(scope PK, root_path, permission_mode, diagnostics_cmd, max_steps, max_tool_bytes, max_wall_seconds, created_at, updated_at)`
- `turn_steps(id PK, request_id FK chat_receipts, parent_step_id NULL, seq, kind, status, tool_name, tool_call_id, input_json, output_json, output_bytes, truncated, tokens_in, tokens_out, error_code, started_at, finished_at, UNIQUE(request_id,seq))`
  - `kind ∈ model_call | tool_call | permission_wait | compaction | verification | subagent`
  - `status ∈ running | complete | failed | denied | interrupted`
  - `input_json`: for `model_call` the exact message array + tool definitions sent; for `tool_call` the parsed arguments.
  - `output_json`: sanitized, bounded; `truncated=1` when capped.
- `activity_events(seq PK AUTOINCREMENT, request_id FK, session_id FK, step_id NULL, kind TEXT, payload_json, created_at)` — open `kind` vocabulary (see Events); this is the UI/SSE feed. `recording_events` remains for the legacy receipt timeline.
- `permission_requests(id PK, request_id FK, step_id FK, tool_name, summary, args_json, status pending|approved|denied|expired, created_at, resolved_at, expires_at)`
- `file_changes(id PK, request_id FK, step_id FK, path, action create|modify|delete, before_hash, after_hash, diff, applied 0|1, reverted_at NULL, created_at)`
- `plan_items(id PK, session_id FK, seq, text, status pending|in_progress|done|failed, updated_at, UNIQUE(session_id,seq))`

### Recovery additions
`recording::recover` additionally runs, in the same transaction:
- `UPDATE turn_steps SET status='interrupted', finished_at=?1 WHERE status='running'`
- `UPDATE permission_requests SET status='expired', resolved_at=?1 WHERE status='pending'`
- insert an `activity_events` row `kind='interrupted'` for each affected request.
No tool is re-executed. The receipt goes to `interrupted` via the existing statement.

## Provider adapter

OpenAI-style wire format is kept. New request field `tools` (array of `{type:"function", function:{name, description, parameters}}` loaded from `tools/schemas/*.json`) and `tool_choice:"auto"`.

Response handling:
- `choices[0].message.content` → optional text.
- `choices[0].message.tool_calls[]` → `{id, function:{name, arguments}}`; `arguments` is a JSON string. Parse failure → a `tool_call` step with `status=failed, error_code=invalid_arguments`; the model receives `{ "error": "invalid_arguments", "detail": "..." }` as the tool result.
- `usage.prompt_tokens / completion_tokens` → `tokens_in / tokens_out` on the `model_call` step when present.
- Tool results are sent back as `{role:"tool", tool_call_id, content}`; the assistant message that contained the tool_calls is replayed verbatim (some providers require it).
- If the provider returns HTTP 400 mentioning `tools`, the loop records `error_code=tools_unsupported` and falls back to the text-only path for that scope (settings flag), so today's behavior is never worse.

## Loop

```
generate(turn):
  recall  = store.recall(scope, prompt)                      # unchanged
  window  = build_messages(system_prompt, recall, history, prompt)   # P1: existing builder + system prompt from prompts/main_agent.md
  save context_json once (write-once trigger keeps it immutable)
  scope_cfg = store.scope(scope)  # root_path, mode, budgets
  tools = if scope_cfg.root_path { registry.schemas() } else { [] }
  messages = window; steps = 0; tool_bytes = 0; start = now
  loop:
    if steps >= max_steps or tool_bytes >= max_tool_bytes or elapsed >= max_wall:
        answer = budget_exhausted_message(...); break
    step = store.begin_step(request, seq, kind=model_call, input=messages+tools)      # committed before the call
    resp = provider.complete_with_tools(model, messages, tools)
    store.finish_step(step, output=resp, tokens)                                        # committed before use
    if resp.tool_calls.is_empty():
        answer = resp.text.unwrap_or("(empty)"); break
    messages.push(assistant_message(resp))
    for call in resp.tool_calls:
        tstep = store.begin_step(request, seq, kind=tool_call, tool_name, tool_call_id, input=args)
        if registry.requires_permission(call, scope_cfg.mode):
            perm = store.request_permission(tstep, summary)      # activity event permission_requested
            outcome = wait_for_permission(perm, 30 min)          # poll DB every 500 ms
            if outcome != approved:
                result = ToolResult::error("denied", ...); store.finish_step(tstep, status=denied)
                messages.push(tool_message(call.id, result)); continue
        result = registry.run(call, ctx{root, scope, request, step})      # may write file_changes rows
        tool_bytes += result.bytes
        store.finish_step(tstep, output=result, status=complete|failed)
        messages.push(tool_message(call.id, result.content))
    steps += 1
  store.complete_recording(request, redact(answer))          # unchanged statement
```

Invariants:
1. A step row exists before its side effect and is finished before its output influences anything.
2. `messages` sent on step N are stored in step N's `input_json`; the receipt `context_json` holds only the initial window (write-once). Together they give a full audit trail.
3. Any panic inside the loop is caught by the existing `tokio::spawn` wrapper → `fail_recording(worker_failed)`; the running step is finished as `failed` if possible.
4. Tool output is redacted and capped before storage (`ToolResult` does this; the loop trusts it but asserts `bytes <= 32 KB`).

## Permissions

| tool | side_effecting | `ask` | `auto_edit` | `auto_all` |
|---|---|---|---|---|
| read, grep, glob, think, todo_write | no | run | run | run |
| edit, write | yes | ask | run | run |
| bash | yes | ask | ask | run, **except deny-list → ask** |

`POST /permissions/{id}` body `{decision: "approve"|"deny", scope}` → idempotent; 409 if already resolved with a different decision; 410 if expired. `GET /permissions?scope=` lists pending. Approval writes `activity_events kind=permission_resolved`.

The `summary` shown to the user is generated by the tool (e.g. `edit src/main.rs (+4 −1)` with the diff preview in `args_json`), never by the model.

**Deadline (settled in P1-T11).** The 30-minute TTL and the turn's wall budget are not two independent clocks: the **earlier** one wins, and `expires_at` is written as that effective deadline. A default turn (900 s) therefore offers a 15-minute approval window, not 30, and a pending row never advertises a window the waiting turn will not honour. Whichever clock runs out, the row goes `expired` and the tool result is a `denied` error the model can adapt to.

## Events (activity_events.kind)

`turn_started`, `model_call_started`, `model_call_finished {tokens_in, tokens_out, tool_call_count}`, `tool_started {tool, summary}`, `tool_finished {tool, status, bytes, truncated, exit_code?}`, `permission_requested {permission_id, tool, summary}`, `permission_resolved {permission_id, decision}`, `file_changed {change_id, path, action, plus, minus}`, `plan_updated {items}`, `budget_exhausted {reason}`, `answer_saved`, `turn_failed {error_code}`, `interrupted`, (P3) `compacted`, (P5) `verified {unverified_claims}`.

Payloads are small JSON; bodies live in `turn_steps`/`file_changes`.

## API (P1)

- `GET /chat/requests/{id}/steps` → `{steps:[{id, seq, kind, status, tool_name, tool_call_id, summary, input_preview, output_preview, previews_capped, output_bytes, truncated, tokens_in, tokens_out, error_code, started_at, finished_at}]}`; previews ≤ 2 KB. 404 for an unknown request, because an empty step list would otherwise read as "this turn did nothing". `summary` is the tool's own phrase read back from the finished step's output, so a step still running has none — its `tool_started` event carries it. `previews_capped` (the preview hit 2 KB) and `truncated` (the tool's own output was capped) are different facts and both are reported.
- `GET /sessions/{id}/plan` → `{items:[{seq, text, status}]}`.
- `GET /activity?session_id=&after_seq=N` → `{events:[{seq, request_id, step_id, kind, payload, created_at}], next_after_seq}` (≤ 200). `next_after_seq` only moves when rows were returned, so a poll that finds nothing cannot skip an event that commits a moment later. This feed is the agentic log only; the receipt timeline (`captured`, `generation_started`, ...) stays in `recording_events` behind `/chat/requests/{id}/context`.
- `GET /permissions?scope=`, `POST /permissions/{id}`.
- `GET /scopes/{scope}`, `POST /scopes/{scope}`.
- `GET /changes?request_id=` → file changes with diffs; (P2) `POST /changes/{id}/revert`.

All under the existing auth middleware and Origin check.

## SSE (P2)

`GET /activity/stream?after_seq=N&session_id=` with `Authorization` header (use `fetch` + `ReadableStream`, not `EventSource`, to keep the token out of the URL). Frames: `id: <seq>\nevent: <kind>\ndata: <json>\n\n`. Server polls `activity_events` (200 ms) and sends a `: heartbeat` comment every 15 s. Client reconnects with the last `id`; exactly-once is guaranteed by the DB sequence, not by the transport.

## Testing

- `tests/mock_provider.py`: loopback OpenAI-style server; a script (list of canned responses) is loaded per test; supports responses with `tool_calls`, records the requests it received (to assert tool results were echoed with the right `tool_call_id`).
- Recovery: start server, submit a turn whose second tool is `bash sleep 30`, SIGKILL during it, restart, assert step `interrupted`, receipt `interrupted`, no new `bash` step, and the sleep process is not restarted.
- Path escape: `read ../../etc/passwd`, symlink inside root pointing outside → `path_denied`.
- Stale anchor: edit with a wrong hash → `stale_anchor` and file unchanged (`before_hash` equals current).
