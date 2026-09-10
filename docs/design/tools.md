# Design — tools (P1)

Model-facing definitions live in `tools/schemas/<name>.json` (OpenAI function format,
`additionalProperties:false`). This document is the behavioral contract for
`src/tools/*.rs`. Inspired by oh-my-pi (hash-anchored edits, bounded reads) and
OpenClaude (read/grep/glob/edit/bash core set).

## Registry

```rust
pub struct ToolCtx { root: PathBuf, scope: String, request_id: String, step_id: String, store: DbStore, diagnostics_cmd: Option<String> }
pub struct ToolResult { content: String, bytes: usize, truncated: bool, status: ToolStatus /* Complete|Failed */, summary: String, artifacts: Vec<Artifact> }
pub trait Tool { fn name(&self)->&'static str; fn schema(&self)->&'static Value; fn side_effecting(&self)->bool; fn side_effecting_for(&self, args:&Value)->bool; fn summary(&self, args:&Value)->String; fn run(&self, ctx:&ToolCtx, args:Value)->Result<ToolResult>; }
```

`ToolResult::finish()` applies `safety::redact`, caps content to 32 KB (24 KB head + `…[N bytes omitted]…` + 8 KB tail), sets `bytes`/`truncated`. Every tool returns through it. Errors are results too: `{ "error": "<code>", "detail": "<human text>" }` with `status=Failed`, so the model can recover.

Error codes: `invalid_arguments`, `path_denied`, `not_found`, `is_directory`, `binary_file`, `too_large`, `stale_anchor`, `ambiguous_match`, `no_match`, `exists`, `timeout`, `denied`, `budget`, `lsp_unavailable`, `lsp_protocol`, `unsupported_edit`, `browser_unavailable`, `browser_protocol`, `browser_closed`, `navigation_failed`.

## Path rules

`paths::resolve(root, input) -> Result<PathBuf>`
1. Reject empty, NUL, absolute paths outside root, and any component `..` *before* canonicalization.
2. Join to root, canonicalize the deepest existing ancestor (for new files), and require `starts_with(root_canonical)`.
3. Deny-list file names (case-insensitive): `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `*.p12`, `*.pfx`, `.netrc`, `.npmrc`, `.pypirc`, `*.kdbx`. `read` returns `path_denied`; `glob`/`grep` skip them silently.
4. Never follow a symlink whose target leaves root.
5. The harness's own `HARNESS_DB` directory is always denied.

## read

Args: `{ path: string, offset?: int (1-based line, default 1), limit?: int (default 200, max 400) }`

Output (text):
```
file: src/main.rs  lines 1-120 of 198  content_hash: 9f3a1c2e
1:a1f3│use anyhow::{bail, Result};
2:0c9d│use axum::{...};
```
- Line hash = first 4 hex chars of SHA-256 over the *trimmed-right* line content. Collisions are acceptable: anchors are `line:hash` pairs, so both must match.
- `content_hash` = first 8 hex of SHA-256 over the full file; used by P3 read cache and by `edit` to record `before_hash`.
- Files > 2 MB → `too_large` (suggest `grep`). Binary (NUL in first 8 KB) → `binary_file`. Lines > 2000 chars are cut with `…`.
- Summary: `read src/main.rs 1-120`.

## grep

Args: `{ pattern: string (regex), path?: string (file or dir, default root), glob?: string, case_insensitive?: bool, max_results?: int (default 50, max 100), context?: int (0-3, default 1) }`

Uses `rg --json` when present (`-n --max-count`, respects `.gitignore`, `--hidden` off), else a Rust walk with the `regex` + `ignore` crates. Output lines `path:line:hash│text` (same hash function as `read`, so results are directly usable as anchors), context lines prefixed with `-`. Ends with `N matches (capped)` when truncated. Summary: `grep "pattern" → N hits`.

## glob

Args: `{ pattern: string (gitignore-style, e.g. "src/**/*.rs"), path?: string }`
Returns ≤ 500 relative paths, newest mtime first, one per line, then `N total`. Summary: `glob src/**/*.rs → N files`.

## edit

Args:
```json
{ "path": "src/main.rs",
  "anchors": [{"line": 12, "hash": "a1f3"}],   // ≥ 1; first anchor = first line of the region being replaced
  "old_string": "optional exact text to replace (must occur once in the anchored region)",
  "new_string": "replacement",
  "end_line": 14 }                                // optional; with no old_string, replace lines [anchors[0].line, end_line]
```
Behavior:
1. Resolve path, read current content, compute line hashes. Every anchor must match (`line` exists and hash equals) or fail `stale_anchor` returning the current `line:hash│text` for the requested lines ± 3 so the model can re-anchor without another `read`.
2. Mode A (`old_string` given): it must occur exactly once within the anchored region (`anchors[0].line ..= end_line|anchors.last().line`); replace. Mode B: replace whole lines.
3. Compute unified diff (`similar` crate), `before_hash`, `after_hash`. Insert `file_changes(applied=0)`; write to `path.tmp-<step>` then `rename`; set `applied=1`. Emit `file_changed` event.
4. If `diagnostics_cmd` is set, run it (cwd root, 60 s cap, output capped 4 KB) and append `\n\n[diagnostics exit N]\n<output>`.
5. Output: `edited src/main.rs (+A −B)` + new `line:hash│text` for the changed region ± 2 lines (so the next edit has fresh anchors) + diagnostics.

Summary (also the permission prompt): `edit src/main.rs (+A −B)`; `args_json` carries the diff for the UI.

## write

Args: `{ path, content, overwrite?: bool }`. Fails `exists` unless `overwrite`. Creates parent dirs inside root. Same `file_changes`/diagnostics/atomic-write behavior as `edit` (action `create` or `modify`). Content > 512 KB → `too_large`.

## ast_edit

Args: `{ path: string, content_hash: string, pattern: string, rewrite: string, max_matches?: int (default 20, max 200) }`.

- P6-T01 accepts one existing `.rs` file of at most 512 KiB. `paths::resolve` remains the only disk-access gate. The required 8-hex `content_hash` comes from the latest `read`; a mismatch fails `stale_anchor`, so neither a stale model call nor a file changed while approval was pending can apply a different diff.
- Matching and rewriting run in-process with `ast-grep-core` and the Rust-only `ast-grep-language` feature. `$NAME` captures one syntax node and may be reused in `rewrite`. An unparseable or empty pattern is `invalid_arguments`; zero matches is `no_match`; more than `max_matches` is `ambiguous_match`. Every refusal leaves the file untouched.
- Every non-overlapping match in the file is rewritten together. The resulting text joins `edit`'s existing `describe`/`apply` path: one unified diff, before/after hashes and +/− counts in the permission payload while the file is still unchanged; after approval `run` checks the hash again, writes atomically, runs `diagnostics_cmd`, and returns the same `Artifact::FileChange` that becomes an applied, revertable `file_changes` row.
- Output summary: `rewrote <path> (+A −B)`, followed by fresh line hashes for the changed span and diagnostics when configured. `auto_edit` and `auto_all` treat it like `edit`; `ask` requires approval.

## lsp

Args: `{ operation: "diagnostics"|"references"|"rename", path: string, line?: int, column?: int, new_name?: string, expected_files?: [{path, content_hash}], include_declaration?: bool, timeout_seconds?: int (default 20, max 60) }`.

- `path` selects a fixed server from its extension: `.rs` uses `rust-analyzer`; C/C++ sources and headers use `clangd`. The model cannot supply a command. Every call starts a fresh stdio server in the project root, speaks bounded JSON-RPC/LSP, clears its environment to `PATH HOME LANG LC_ALL TERM` plus `CARGO_NET_OFFLINE=true`, and shuts it down. A missing or crashed server is `lsp_unavailable`; malformed or oversized protocol data is `lsp_protocol`.
- Input `line` and `column` are 1-based Unicode-character positions. The tool validates them against the current file and converts to LSP's zero-based UTF-16 positions. `references` and `rename` require both; diagnostics needs only `path`.
- `diagnostics` returns at most 100 sorted diagnostics as `path:line:column severity [code] message`. `references` returns at most 100 in-root locations grouped by file with the current 8-hex `content_hash` for every listed file. Outside-root or denied locations are omitted and counted. Neither operation enters the permission gate.
- `rename` also requires `new_name` and `expected_files` (at most 20 path/hash pairs, including the target) copied from current `read`/`references` results. The workspace edit may touch at most 20 existing in-root text files and 200 non-overlapping edits. Resource create/rename/delete operations, non-file URIs, new files, denied paths, oversized files, unlisted edited files and hash mismatches are refused before writing.
- A rename permission contains file counts, total +/− counts, before/after hashes and one combined diff capped at 64 KiB while disk and `/changes` stay untouched. After approval the server is queried again and expected hashes are rechecked. All output files are validated before the first write, each lands through the existing atomic helper, and an intermediate failure rolls back prior writes. Success returns one ordinary `Artifact::FileChange` per file and runs `diagnostics_cmd` once.
- `Tool::side_effecting_for(args)` defaults to `side_effecting()`; the registry uses it for approval routing. `lsp` reports that it is capable of side effects, but returns true per call only for `operation=rename`. In `ask` mode rename asks, while `auto_edit` and `auto_all` treat it like an edit.

## browser

Args: `{ operation: "open"|"snapshot"|"click"|"type"|"press"|"close", url?: string, snapshot_id?: string, ref?: string, text?: string, key?: string, submit?: bool, wait_ms?: int (default 500, max 5000), max_nodes?: int (default 200, max 400), timeout_seconds?: int (default 15, max 30) }`.

- One `Browser` instance belongs to one `Registry`, and the agent loop owns that registry for one turn. A mutex serializes calls and keeps one page-target CDP WebSocket alive across that turn; dropping the registry closes the socket, kills an owned browser process group and removes its isolated temporary profile. State never crosses turns and `close` ends it early.
- `open` accepts only credential-free HTTP(S) URLs. The model cannot choose an executable or endpoint. If the operator supplied `HARNESS_CDP_URL`, it must be a credential-free loopback `ws://` page target. Otherwise the tool discovers `HARNESS_BROWSER_PATH` or a fixed Chrome/Chromium installation, launches headless with an isolated profile and loopback ephemeral debugging port, then selects the page target from `/json/list`. A missing binary/endpoint is `browser_unavailable`.
- The client enables only `Page`, `Runtime`, `DOM`, and `Accessibility`, and issues fixed protocol methods; there is no model-facing JavaScript or CSS selector. WebSocket messages are capped at 2 MiB, one call at 30 seconds, waits at 5 seconds, page inputs at 8 KiB, accessibility input at 5000 nodes, and rendered snapshots at 400 semantic nodes. Malformed, oversized, closed, or error responses fail as `browser_protocol`, `browser_closed`, `navigation_failed`, or `timeout`.
- A snapshot contains the current URL/title and a bounded accessibility tree. Semantic nodes with a backend DOM id receive refs such as `b42`; text and attributes are normalized and capped, and every result starts with an explicit untrusted-page-content warning. `snapshot_id` is the first eight SHA-256 hex characters of the URL, title, bounded tree, and omission count.
- `click`, `type`, and `press` require the exact latest `snapshot_id`; click/type also require a returned ref. Immediately before dispatch, the tool captures the page again and refuses `stale_anchor` unless its hash and the referenced role/name/value/state still match. Click scrolls the node into view and dispatches one left click. Type invokes one fixed value-setter function on a textbox-like node, emits input/change events, and optionally presses Enter. Press accepts only Enter, Tab, Escape, Backspace, arrows, PageUp/PageDown, Home/End, or Space. Each action waits boundedly and returns a fresh snapshot.
- `Tool::side_effecting_for(args)` is true only for click/type/press. In `ask` mode their permission card names the current URL, snapshot, ref and accessible target (plus the bounded text/key); approval never replays `open`. `open`, `snapshot`, and `close` never enter the gate. `auto_edit`/`auto_all` may interact automatically.

## bash

Args: `{ command: string, timeout_seconds?: int (default 120, max 600), background?: bool, description: string (≤ 80 chars, shown to the user) }`

- `sh -c <command>`, cwd = root, env whitelist `PATH HOME LANG LC_ALL TERM`, plus `HARNESS_SCOPE`.
- Foreground: capture stdout+stderr interleaved, kill process group on timeout → `timeout` error with partial output. Output ends with `[exit N]`.
- Background: `setsid`, redirect to `<root>/.harness/logs/<step_id>.log`, return `{pid, log}` immediately. The model should later `bash tail -n 50 <log>`.
- Deny-list (always requires permission, even `auto_all`): regexes for `rm -rf /`, `rm -rf ~`, `git push --force`, `git reset --hard`, `mkfs`, `dd if=`, `> /dev/sd`, `chmod -R 777 /`, `curl|sh`, `wget|sh`.
- Summary: the `description` argument, or the first 60 chars of the command.

## think

Args: `{ thought: string (≤ 4000 chars) }`. Stores the text as step output, returns `noted`. Not side-effecting. Rendered in the UI as a collapsed "reasoning" card. It is an explicit scratchpad, not hidden chain-of-thought.

## todo_write

Args: `{ items: [{ text: string (≤ 200), status: "pending"|"in_progress"|"done"|"failed" }] }` (≤ 30 items, at most one `in_progress`). Replaces the session plan transactionally; emits `plan_updated`. Returns the normalized list. The system prompt instructs the model to call it before multi-step work and after each step.

## skill

Args: `{ name: string (≤ 64 chars; letters, digits, `-` or `_`) }`. Loads `skills/<name>/SKILL.md`. This is the second half of progressive disclosure; the first half is the `skills_index` context category (docs/design/context.md).

- Not side-effecting: it reads one known path, so it runs under every permission mode.
- `name` is a single directory name, never a path. Anything else is `invalid_arguments`, and an unknown name is `not_found`. Both refusals list up to 12 discovered skill names, so a typo costs one call instead of a guessing loop.
- `paths::resolve` still runs, keeping the sandbox, secret deny-list and symlink rules the single gate for disk access.
- The result is the file below its frontmatter, redacted, capped at 16 KiB on a UTF-8 boundary. A truncated body names the file and the cap so the model can `read` the rest.
- Output is prefixed with the source line and an explicit note that a skill body is project guidance which cannot grant tool permissions, approve a denied command, or override system rules.
- Summary: `skill <name> (<n> bytes[, truncated])`.

## task

Args: `{ description?: string (≤ 80 chars), prompt: string (≤ 2000 chars) }`. Runs one read-only exploration in a sub-agent and returns a bounded report. Contract and bounds: `src/subagent.rs`; orchestration: `Ctx::run_task` in `src/agent_loop.rs`.

- Registered like any other tool so the model still sees one immutable definition array, but the loop intercepts the call before `Registry::invoke`, because a sub-agent needs provider calls of its own and `Tool::run` is synchronous and filesystem-bound. A direct registry invocation refuses with `internal_error` instead of pretending to explore.
- The sub-agent is offered `read`, `grep` and `glob` only, reusing the parent's exact definitions so both agents describe a tool identically. Nothing in that set is side-effecting, so no approval can be raised inside a delegation and `task` itself needs no permission. The allow-list is enforced again on the way back: any other tool name is refused as `unknown_tool` without the registry being reached, so a sub-agent cannot be used to route around a denied edit.
- `prompt` is required and must be self-contained. The sub-agent never sees the parent conversation, and its transcript never reaches the parent context — spending the sub-agent's context instead of the parent's is the whole point of delegating.
- Steps: the `task` tool-call step is the parent of one `subagent` step, and the sub-agent's own model calls and tool calls hang off that step under the same `request_id`, so a turn stays one ordered step list that still reads back as a tree. Events: `subagent_started`, `subagent_finished`.
- Budgets are the parent's rather than new ones: at most 8 model calls, and it stops as soon as the turn's remaining steps, tool bytes or wall deadline run out. What it spent is added to the parent's counters before the loop continues.
- Returns a header line (model calls, tool calls, and why it stopped when it stopped early), a summary capped at 1000 chars, and up to 12 paths it actually read. A partial exploration says so, so the parent cannot read it as a complete answer.
- Summary: `explore: <description>`.

## Later tools (contracts to be written when scheduled)

`web_fetch` (P5, opt-in per scope).
