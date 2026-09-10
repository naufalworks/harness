#!/usr/bin/env python3
"""Writes tools/schemas/*.json from one source of truth. Re-run after editing."""
import json
import pathlib

OUT = pathlib.Path(__file__).resolve().parents[1] / "tools" / "schemas"


def obj(props, required):
    return {"type": "object", "properties": props, "required": required, "additionalProperties": False}


S = lambda d, **k: {"type": "string", "description": d, **k}  # noqa: E731
I = lambda d, **k: {"type": "integer", "description": d, **k}  # noqa: E731
B = lambda d: {"type": "boolean", "description": d}  # noqa: E731

TOOLS = {
    "read": (
        "Read a text file inside the project root. Returns numbered lines as `line:hash|text`; use those line/hash pairs as anchors for `edit`. Prefer `grep` to locate code first, then read a small range.",
        obj({
            "path": S("Path relative to the project root."),
            "offset": I("1-based first line to return. Default 1.", minimum=1),
            "limit": I("Number of lines to return. Default 200, max 400.", minimum=1, maximum=400),
        }, ["path"]),
    ),
    "grep": (
        "Search file contents with a regular expression. Output lines are `path:line:hash|text` so they can be used directly as `edit` anchors. Respects .gitignore.",
        obj({
            "pattern": S("Regular expression (Rust/ripgrep syntax)."),
            "path": S("File or directory to search, relative to root. Default: whole project."),
            "glob": S("Only search files matching this glob, e.g. `*.rs` or `src/**/*.ts`."),
            "case_insensitive": B("Case-insensitive match. Default false."),
            "max_results": I("Maximum matches. Default 50, max 100.", minimum=1, maximum=100),
            "context": I("Context lines around each match, 0-3. Default 1.", minimum=0, maximum=3),
        }, ["pattern"]),
    ),
    "glob": (
        "List files matching a gitignore-style pattern, newest first. Use to discover project structure.",
        obj({
            "pattern": S("Glob such as `src/**/*.rs` or `**/package.json`."),
            "path": S("Directory to search under, relative to root. Default: root."),
        }, ["pattern"]),
    ),
    "edit": (
        "Edit an existing file using line anchors from a previous `read`/`grep`. Every anchor must still match; if the file changed you get the current lines back and must re-anchor. Give `old_string` to replace one exact occurrence inside the anchored region, or omit it to replace whole lines from anchors[0].line to end_line. A diff is recorded and diagnostics run afterwards.",
        obj({
            "path": S("Path relative to the project root."),
            "anchors": {
                "type": "array",
                "minItems": 1,
                "maxItems": 8,
                "description": "Line/hash pairs copied from `read` or `grep` output. The first anchor is the first line of the edited region.",
                "items": obj({"line": I("1-based line number.", minimum=1), "hash": S("4-hex line hash as shown by read/grep.", pattern="^[0-9a-f]{4}$")}, ["line", "hash"]),
            },
            "old_string": S("Exact text to replace. Must occur exactly once in the anchored region."),
            "new_string": S("Replacement text. Empty string deletes."),
            "end_line": I("Last line (inclusive) of the region when replacing whole lines.", minimum=1),
        }, ["path", "anchors", "new_string"]),
    ),
    "write": (
        "Create a new file (or overwrite with overwrite=true). For changes to existing files prefer `edit`.",
        obj({
            "path": S("Path relative to the project root. Parent directories are created."),
            "content": S("Full file content."),
            "overwrite": B("Allow replacing an existing file. Default false."),
        }, ["path", "content"]),
    ),
    "bash": (
        "Run a shell command in the project root and return its combined output and exit code. Use for builds, tests, git, and inspection. Long-running servers must use background=true and be checked via their log file.",
        obj({
            "command": S("The command to run with `sh -c`."),
            "description": S("Short human-readable purpose shown to the user (max 80 chars).", maxLength=80),
            "timeout_seconds": I("Kill after this many seconds. Default 120, max 600.", minimum=1, maximum=600),
            "background": B("Start detached and return pid + log path immediately. Default false."),
        }, ["command", "description"]),
    ),
    "think": (
        "Write down reasoning, hypotheses, or a checklist before acting. Nothing is executed. Use it when a task has more than two steps or when evidence conflicts.",
        obj({"thought": S("Your notes. Max 4000 characters.", maxLength=4000)}, ["thought"]),
    ),
    "todo_write": (
        "Replace the plan for this session. Call it before multi-step work and again whenever an item changes status. At most one item may be in_progress.",
        obj({
            "items": {
                "type": "array",
                "maxItems": 30,
                "items": obj({
                    "text": S("Short imperative description.", maxLength=200),
                    "status": {"type": "string", "enum": ["pending", "in_progress", "done", "failed"]},
                }, ["text", "status"]),
            }
        }, ["items"]),
    ),
    "skill": (
        "Load one project skill body from skills/<name>/SKILL.md. The context reference lists only skill names and descriptions; call this to read the instructions of a skill whose description matches the task, instead of guessing what it contains. Returns at most 16 KB.",
        obj({"name": S("Skill directory name exactly as listed in the skills index (letters, digits, - or _).", maxLength=64)}, ["name"]),
    ),
    "task": (
        "Delegate one read-only exploration to a sub-agent with its own context. It may only read, grep and glob, and it cannot edit files, run commands or change anything. Use it for open-ended searches such as 'where is X handled?' so the intermediate output stays out of this conversation; you get back a short summary plus the files it found. Give it self-contained instructions: it cannot see this conversation.",
        obj({
            "description": S("Short human-readable purpose shown to the user (max 80 chars).", maxLength=80),
            "prompt": S("Self-contained instructions: what to look for and what to report back.", maxLength=2000),
        }, ["description", "prompt"]),
    ),
    "ast_edit": (
        "Structural find-and-replace in one Rust file. The pattern is parsed as code, not text, so `$NAME` matches a whole expression, statement or item and formatting, line breaks and comments never matter. Prefer this over `edit` when the same shape repeats and text matching would be brittle; use `edit` for one-off edits and for files that are not Rust. Every match in the file is rewritten together, and the call is refused without writing anything if the pattern matches nothing or matches more often than max_matches.",
        obj({
            "path": S("Path to a .rs file, relative to the project root."),
            "content_hash": S("8-hex content hash from the latest read of this file. The rewrite is refused if the file changed before approval.", pattern="^[0-9a-f]{8}$"),
            "pattern": S("Pattern written as Rust code, e.g. `$X.unwrap_or($A)`. `$NAME` captures one node; repeating the same name requires the same code in both places."),
            "rewrite": S("Replacement written as Rust code. Reuse captures by name, e.g. `$X.unwrap_or_else(|| $A)`."),
            "max_matches": I("Refuse instead of rewriting if the pattern matches more times than this. Default 20, max 200.", minimum=1, maximum=200),
        }, ["path", "content_hash", "pattern", "rewrite"]),
    ),
    "lsp": (
        "Ask a local language server for diagnostics or symbol references, or perform an approval-gated workspace rename. Paths stay inside the project. Diagnostics and references are read-only. Before rename, collect references/read every affected file and pass their current content hashes in expected_files; an unlisted or stale file makes the whole rename fail without writing.",
        obj({
            "operation": {"type": "string", "enum": ["diagnostics", "references", "rename"], "description": "Operation to perform."},
            "path": S("Rust or C/C++ file relative to the project root."),
            "line": I("1-based line containing the symbol. Required for references and rename.", minimum=1),
            "column": I("1-based Unicode-character column at the symbol. Required for references and rename.", minimum=1),
            "new_name": S("New symbol name. Required for rename; max 128 characters with no whitespace.", maxLength=128),
            "expected_files": {
                "type": "array",
                "description": "For rename, every file the server may edit with its latest 8-hex content_hash from read/references.",
                "minItems": 1,
                "maxItems": 20,
                "items": obj({
                    "path": S("Project-relative file path."),
                    "content_hash": S("Current 8-hex whole-file hash.", pattern="^[0-9a-f]{8}$"),
                }, ["path", "content_hash"]),
            },
            "include_declaration": B("Include the declaration in references. Default true."),
            "timeout_seconds": I("Whole-call language-server deadline. Default 20, max 60.", minimum=1, maximum=60),
        }, ["operation", "path"]),
    ),
}


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, (desc, params) in TOOLS.items():
        doc = {"type": "function", "function": {"name": name, "description": desc, "parameters": params}}
        (OUT / f"{name}.json").write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {len(TOOLS)} schemas to {OUT}")


if __name__ == "__main__":
    main()
