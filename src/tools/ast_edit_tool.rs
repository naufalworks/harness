//! ast_edit. Contract: docs/design/tools.md#ast_edit.
//!
//! Structural find-and-replace. The pattern and the rewrite are parsed as code, so `$NAME`
//! captures a whole syntax node and formatting, line breaks and comments cannot break a match.
//! Only the matching stage is new here: the resulting after-text goes through the same
//! `describe`/`apply` path as `edit`. The permission payload is planned without touching disk,
//! `run` re-plans against the required content hash before writing atomically, and the loop then
//! records the applied, revertable file change.
use ast_grep_core::matcher::Pattern;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use serde_json::Value;
use std::path::Path;

use super::edit_tools::{apply, describe, payload};
use super::fs_tools::read_text;
use super::{content_hash, paths, PendingChange, Tool, ToolCtx, ToolResult};

/// How many sites one call may rewrite before it is refused. A pattern that hits more than this
/// is usually broader than intended, and a sprawling rewrite is not a diff anyone reviews.
const DEFAULT_MAX_MATCHES: usize = 20;
const MAX_MATCHES: usize = 200;
/// Matching parses the whole file into a syntax tree in memory, so it stops where `write` does.
const PARSE_MAX: usize = 512 * 1024;

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// The language table. Rust only for P6-T01: every other grammar is a compile-time dependency
/// with no caller yet, and adding one is a line here plus its feature in Cargo.toml.
fn language_of(path: &Path) -> Option<SupportLang> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Some(SupportLang::Rust),
        _ => None,
    }
}

fn plan_ast_edit(ctx: &ToolCtx, args: &Value) -> Result<PendingChange, ToolResult> {
    let Some(rel) = arg_str(args, "path") else {
        return Err(ToolResult::err("invalid_arguments", "path is required"));
    };
    let Some(expected_hash) = arg_str(args, "content_hash") else {
        return Err(ToolResult::err(
            "invalid_arguments",
            "content_hash from the latest read is required",
        ));
    };
    let Some(pattern_src) = arg_str(args, "pattern") else {
        return Err(ToolResult::err("invalid_arguments", "pattern is required"));
    };
    let Some(rewrite) = arg_str(args, "rewrite") else {
        return Err(ToolResult::err("invalid_arguments", "rewrite is required"));
    };
    if expected_hash.len() != 8
        || !expected_hash
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(ToolResult::err(
            "invalid_arguments",
            "content_hash must be the 8 lowercase hex characters returned by read",
        ));
    }
    if pattern_src.trim().is_empty() {
        return Err(ToolResult::err(
            "invalid_arguments",
            "pattern must not be empty",
        ));
    }
    let max_matches = match args.get("max_matches") {
        None => DEFAULT_MAX_MATCHES,
        Some(value) => match value.as_u64() {
            Some(n) if n >= 1 && n as usize <= MAX_MATCHES => n as usize,
            _ => {
                return Err(ToolResult::err(
                    "invalid_arguments",
                    format!("max_matches must be a whole number between 1 and {MAX_MATCHES}"),
                ))
            }
        },
    };

    let path = paths::resolve(&ctx.root, rel).map_err(|e| ToolResult::err(e.code(), e.detail()))?;
    let display = paths::display(&ctx.root, &path);
    let Some(lang) = language_of(&path) else {
        return Err(ToolResult::err("invalid_arguments", format!("ast_edit only understands Rust so far and {display} is not a .rs file; use edit for this one")));
    };
    let before = read_text(&path)?;
    if before.len() > PARSE_MAX {
        return Err(ToolResult::err(
            "too_large",
            format!(
                "{display} is {} bytes; ast_edit parses whole files and stops at {PARSE_MAX}",
                before.len()
            ),
        ));
    }
    let current_hash = content_hash(&before);
    if current_hash != expected_hash {
        return Err(ToolResult::err("stale_anchor", format!("{display} changed since it was read: expected content_hash {expected_hash}, current content_hash {current_hash}. Read the file again before retrying")));
    }
    // A pattern that is not itself parseable cannot match anything, so name that as the problem
    // instead of reporting no_match and sending the model looking at the file.
    let pattern = Pattern::try_new(pattern_src, lang).map_err(|e| {
        ToolResult::err(
            "invalid_arguments",
            format!("pattern is not parseable Rust: {e}"),
        )
    })?;

    let tree = lang.ast_grep(&before);
    // `replace_all` only computes the edits; nothing reaches disk until `apply` writes the file.
    let edits = tree.root().replace_all(pattern, rewrite);
    if edits.is_empty() {
        return Err(ToolResult::err("no_match", format!("the pattern matched nothing in {display}; check the shape against the file with grep, including the receiver and the argument count")));
    }
    if edits.len() > max_matches {
        return Err(ToolResult::err("ambiguous_match", format!("the pattern matched {} places in {display} and the cap is {max_matches}; narrow the pattern or raise max_matches (limit {MAX_MATCHES})", edits.len())));
    }

    // Splice the edits into the source. The offsets are byte offsets: `Content for String` sets
    // `Underlying = u8` and slices `as_bytes()`. Matches arrive in order and cannot nest, but an
    // overlapping or out-of-range edit would silently corrupt the file, so refuse instead.
    let mut after = String::with_capacity(before.len());
    let mut cursor = 0usize;
    for edit in &edits {
        let end = edit.position + edit.deleted_length;
        let sane = edit.position >= cursor
            && end <= before.len()
            && before.is_char_boundary(edit.position)
            && before.is_char_boundary(end);
        if !sane {
            return Err(ToolResult::err("ambiguous_match", format!("the matches in {display} overlap; narrow the pattern so each site is rewritten once")));
        }
        after.push_str(&before[cursor..edit.position]);
        match std::str::from_utf8(&edit.inserted_text) {
            Ok(text) => after.push_str(text),
            Err(_) => {
                return Err(ToolResult::err(
                    "invalid_arguments",
                    "the rewrite produced invalid UTF-8",
                ))
            }
        }
        cursor = end;
    }
    after.push_str(&before[cursor..]);

    Ok(describe(&ctx.root, path, "modify", Some(before), after))
}

pub struct AstEdit;
impl Tool for AstEdit {
    fn name(&self) -> &'static str {
        "ast_edit"
    }
    fn schema(&self) -> &'static str {
        include_str!("../../tools/schemas/ast_edit.json")
    }
    fn side_effecting(&self) -> bool {
        true
    }
    // `summary` cannot read the file, so the match count and diff live in `permission_payload`.
    fn summary(&self, a: &Value) -> String {
        format!("ast_edit {}", arg_str(a, "path").unwrap_or("?"))
    }
    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value {
        payload(plan_ast_edit(ctx, args), args)
    }
    fn plan(&self, ctx: &ToolCtx, args: &Value) -> Option<Result<PendingChange, ToolResult>> {
        Some(plan_ast_edit(ctx, args))
    }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        match plan_ast_edit(ctx, &args) {
            Ok(change) => apply(ctx, "rewrote", change),
            Err(refusal) => refusal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Artifact, ToolStatus};
    use serde_json::json;
    use std::path::PathBuf;

    /// Two call sites with the same shape but different formatting: a text edit would need two
    /// different `old_string`s, one pattern covers both.
    const LIB: &str = "fn main() {\n    let a = first().unwrap_or(fallback());\n    let b = second()\n        .unwrap_or(other());\n}\n";

    fn project() -> (PathBuf, ToolCtx) {
        let dir = std::env::temp_dir().join(format!("harness-ast-edit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), LIB).unwrap();
        std::fs::write(dir.join("notes.md"), "# not rust\n").unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let ctx = ToolCtx {
            root: root.clone(),
            scope: "global".into(),
            request_id: "request".into(),
            step_id: "step-1".into(),
            diagnostics_cmd: None,
        };
        (root, ctx)
    }
    fn read_back(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }
    fn rewrite_args() -> Value {
        json!({ "path": "src/lib.rs", "content_hash": content_hash(LIB), "pattern": "$X.unwrap_or($A)", "rewrite": "$X.unwrap_or_else(|| $A)" })
    }

    #[test]
    fn ast_edit_rewrites_every_match_whatever_the_formatting() {
        let (root, ctx) = project();
        let out = AstEdit.run(&ctx, rewrite_args());
        assert_eq!(out.status, ToolStatus::Complete, "{}", out.content);
        let text = read_back(&root, "src/lib.rs");
        assert!(
            text.contains("first().unwrap_or_else(|| fallback())"),
            "{text}"
        );
        assert!(
            text.contains("unwrap_or_else(|| other())"),
            "the match split over two lines must be rewritten too: {text}"
        );
        assert!(!text.contains("unwrap_or("), "{text}");
        assert!(
            matches!(
                out.artifacts.first(),
                Some(Artifact::FileChange {
                    action: "modify",
                    ..
                })
            ),
            "{:?}",
            out.artifacts
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ast_edit_refuses_zero_matches_without_writing() {
        let (root, ctx) = project();
        let out = AstEdit.run(&ctx, json!({ "path": "src/lib.rs", "content_hash": content_hash(LIB), "pattern": "$X.expect($A)", "rewrite": "$X.unwrap()" }));
        assert_eq!(out.error_code, Some("no_match"));
        assert_eq!(
            read_back(&root, "src/lib.rs"),
            LIB,
            "a refusal must leave the file untouched"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ast_edit_refuses_more_matches_than_the_cap_without_writing() {
        let (root, ctx) = project();
        let mut args = rewrite_args();
        args["max_matches"] = json!(1);
        let out = AstEdit.run(&ctx, args);
        assert_eq!(out.error_code, Some("ambiguous_match"));
        assert!(out.content.contains("matched 2 places"), "{}", out.content);
        assert_eq!(
            read_back(&root, "src/lib.rs"),
            LIB,
            "a refusal must leave the file untouched"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ast_edit_refuses_a_language_it_cannot_parse_and_a_pattern_that_is_not_code() {
        let (root, ctx) = project();
        let wrong_language = AstEdit.run(&ctx, json!({ "path": "notes.md", "content_hash": "00000000", "pattern": "$A", "rewrite": "$A" }));
        assert_eq!(wrong_language.error_code, Some("invalid_arguments"));
        assert!(
            wrong_language.content.contains("Rust"),
            "{}",
            wrong_language.content
        );
        let empty = AstEdit.run(&ctx, json!({ "path": "src/lib.rs", "content_hash": content_hash(LIB), "pattern": "   ", "rewrite": "x" }));
        assert_eq!(empty.error_code, Some("invalid_arguments"));
        assert_eq!(read_back(&root, "src/lib.rs"), LIB);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ast_edit_plans_the_change_without_touching_disk() {
        let (root, ctx) = project();
        let planned = AstEdit.plan(&ctx, &rewrite_args()).unwrap().unwrap();
        assert_eq!(planned.action, "modify");
        assert!(planned.diff.contains("unwrap_or_else"), "{}", planned.diff);
        assert!(planned.before_hash.is_some());
        assert_eq!(
            read_back(&root, "src/lib.rs"),
            LIB,
            "planning must not write"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ast_edit_refuses_a_stale_content_hash_without_writing() {
        let (root, ctx) = project();
        assert_ne!(content_hash(LIB), "00000000");
        let mut args = rewrite_args();
        args["content_hash"] = json!("00000000");
        let out = AstEdit.run(&ctx, args);
        assert_eq!(out.error_code, Some("stale_anchor"));
        assert!(
            out.content.contains("current content_hash"),
            "{}",
            out.content
        );
        assert_eq!(
            read_back(&root, "src/lib.rs"),
            LIB,
            "a stale structural edit must leave the file untouched"
        );
        std::fs::remove_dir_all(root).ok();
    }
}
