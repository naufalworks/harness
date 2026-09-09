# Design — UI/UX

Goal: the user always knows *what the agent is doing, what it did, what it changed, and
what it knew*. Memory becomes ambient. Everything renders from recorded rows, so a
reload shows exactly what a live watcher saw.

Security constraints carried over: strict CSP (no inline scripts/styles), `textContent`
only (never `innerHTML` with model or tool text), token in tab memory only, all API
calls authenticated. Markdown rendering (P2) must use a sanitizing renderer with links
`rel="noopener"` and no raw HTML passthrough.

## P1 minimal (polling)

**Turn card**
```
You · 12:41   "add retry with backoff to the fetch client"           Saved · Thinking…
  ▾ Steps (4)
    ● read  src/lib/fetch.ts 1-120                          complete · 3.1 KB
    ● grep  "fetchWithTimeout" → 3 hits                       complete
    ● edit  src/lib/fetch.ts (+18 −2)                        awaiting approval  [Approve] [Deny]
    ○ bash  run unit tests                                     queued
Harness · 12:42   "Added exponential backoff…"        3 memories · 2 files changed · 12.4k tokens
```
- Steps list comes from `GET /chat/requests/{id}/steps`, polled every 1 s while the receipt is `generating` (reuse `followReceipt`).
- Each step row: icon by status (○ queued/running with spinner, ● complete, ✖ failed, ⚠ denied/interrupted), tool name, `summary`, right-aligned meta. Click expands `input_preview` / `output_preview` in a `<pre>`.
- Pending permission renders inside the step row **and** as a sticky card above the composer (so it is impossible to miss). Buttons call `POST /permissions/{id}`; disable on click; show the diff (`args_json.diff`) in a `<pre>` for edit/write and the command for bash.
- Plan: `GET /sessions/{id}/plan` rendered as a checklist strip above the composer; updates on every poll.
- Scope settings (root path, permission mode, diagnostics command, budgets) in the existing Models tab, renamed "Project & models". Root path is entered by the user; the server validates it.

## P2 layout

```
┌─ Sessions / scopes ─┬─ Conversation ─────────────────────┬─ Activity ─────────────┐
│ scope: myrepo  ▾    │ turn cards …                          │ Plan  (3/5 done)           │
│ • add retry…        │                                       │  ☑ read fetch client        │
│ • fix CI            │                                       │  ▶ write tests  00:12       │
│ • …                 │                                       │  ☐ run suite                │
│                     │                                       │ Running: bash “npm test”   │
│ Inbox (2)           │                                       │ Context 38k / 120k ███░░    │
│ Settings            │ [composer] [mode: ask ▾] [Send]        │ Memories used (3) ›        │
└─────────────────────┴───────────────────────────────────────┴────────────────────────────┘
```
- Left pane: scope picker (creates/edits scopes), session list grouped by scope, Inbox badge, Settings. Collapses to a drawer on narrow screens.
- Right pane (Activity): live plan; running step with elapsed timer and a Stop button (P2: `POST /chat/requests/{id}/stop` sets a cancel flag the loop checks between steps — never mid-tool); context meter (P3 data; P2 shows tokens so far); "memories used" chip opening the existing receipt view; pending permission.
- Event source: SSE stream (`docs/design/agentic-turn.md#sse`); fall back to polling if the stream errors twice.
- Theme: port light/dark and typography from `reference/renewed-ui-original/static/index.html`, extracted into `style.css` (no inline styles).

## Diff cards

For each `file_changes` row: header `path · +A −B · applied 12:42`, body unified diff with per-line coloring done via CSS classes on `<span>` elements built from `textContent`, footer `Revert` (P2-T03) when the current file hash still equals `after_hash`, otherwise a muted "file changed since; revert unavailable".

## Memory becomes ambient (P4)

- Under a turn: `Remember? “prefers pnpm over npm”  [Save] [Edit] [Dismiss]` — pending candidates associated through `chat:{request_id}` or `evidence.request_id`. The tray refreshes without blocking sending and disappears when all of that turn's suggestions resolve.
- `Edit` opens an inline, bounded textarea. Applying an edit revalidates only the proposed value and leaves evidence, category and expected revision intact; the user still chooses Save separately.
- Correction-backed suggestions carry a visible high-priority badge. All candidate/model text is constructed with DOM nodes and `textContent`; no raw HTML or inline style is introduced.
- Chip on the assistant message: `3 memories used ›` → receipt view.
- Memory tab renamed **Inbox**: import/backlog candidates only. Chat candidates do not appear twice.
- Never a modal. Never blocks sending.

## Keyboard and commands (P2+)

`Enter` send, `Shift+Enter` newline, `Esc` focus composer, `Cmd/Ctrl+K` palette. Slash commands: `/scope <name>`, `/mode ask|auto_edit|auto_all`, `/plan`, `/compact` (P3), `/forget <key>` (P4), `/skill <name>` (P5).

## Copy rules

- State labels stay honest: "Saved · answer failed" not "Error". "Awaiting your approval" not "Blocked".
- Never claim a file was changed until `applied=1`.
- Show costs as tokens, not currency, unless the user configures a price.
