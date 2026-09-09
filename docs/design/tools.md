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

## Later tools (contracts to be written when scheduled)

`task` (P5): read-only sub-agent. `skill` (P5): load `skills/<name>/SKILL.md`. `web_fetch` (P5, opt-in per scope). `ast_edit`, `lsp`, `browser` (P6).
