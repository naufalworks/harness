# Design — tools (P1)

Model-facing definitions live in `tools/schemas/<name>.json` (OpenAI function format,
`additionalProperties:false`). This document is the behavioral contract for
`src/tools/*.rs`. Inspired by oh-my-pi (hash-anchored edits, bounded reads) and
OpenClaude (read/grep/glob/edit/bash core set).

## Registry

```rust
pub struct ToolCtx { root: PathBuf, scope: String, request_id: String, step_id: String, store: DbStore, diagnostics_cmd: Option<String> }
pub struct ToolResult { content: String, bytes: usize, truncated: bool, status: ToolStatus /* Complete|Failed */, summary: String, artifacts: Vec<Artifact> }
pub trait Tool { fn name(&self)->&'static str; fn schema(&self)->&'static Value; fn side_effecting(&self)->bool; fn summary(&self, args:&Value)->String; fn run(&self, ctx:&ToolCtx, args:Value)->Result<ToolResult>; }
```

`ToolResult::finish()` applies `safety::redact`, caps content to 32 KB (24 KB head + `…[N bytes omitted]…` + 8 KB tail), sets `bytes`/`truncated`. Every tool returns through it. Errors are results too: `{ "error": "<code>", "detail": "<human text>" }` with `status=Failed`, so the model can recover.

Error codes: `invalid_arguments`, `path_denied`, `not_found`, `is_directory`, `binary_file`, `too_large`, `stale_anchor`, `ambiguous_match`, `no_match`, `exists`, `timeout`, `denied`, `budget`.

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

`web_fetch` (P5, opt-in per scope). `ast_edit`, `lsp`, `browser` (P6).
