//! Read Git state and propose or apply narrowly-scoped repository controls.
//!
//! Read operations are safe and return bounded, text-only evidence. Checkpoint restore, commit,
//! and push are explicit operations that stay behind the normal permission gate. They are not
//! hidden shell strings: the loop records the exact operation and payload before dispatching it.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{truncate_chars, Tool, ToolCtx, ToolResult};

const MESSAGE_MAX: usize = 120;
const CHECKPOINT_MAX: usize = 64;

pub struct Git;

impl Tool for Git {
    fn name(&self) -> &'static str {
        "git"
    }

    fn schema(&self) -> &'static str {
        include_str!("../../tools/schemas/git.json")
    }

    // The capability is mixed: side_effecting_for is the source of truth for each operation.
    fn side_effecting(&self) -> bool {
        false
    }

    fn side_effecting_for(&self, args: &Value) -> bool {
        matches!(
            args.get("operation").and_then(Value::as_str),
            Some("checkpoint_create" | "checkpoint_restore" | "commit" | "push")
        )
    }

    fn summary(&self, args: &Value) -> String {
        let operation = args
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("git");
        match operation {
            "commit" => format!(
                "git commit: {}",
                truncate_chars(
                    args.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("proposal"),
                    MESSAGE_MAX
                )
            ),
            "push" => "git push".into(),
            "checkpoint_restore" => "restore Git checkpoint".into(),
            "checkpoint_create" => "create Git checkpoint".into(),
            "propose_commit" => "prepare focused commit proposal".into(),
            other => format!("git {other}"),
        }
    }

    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value {
        let operation = args
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("status");
        json!({
            "operation": operation,
            "cwd": ctx.root,
            "message": args.get("message").and_then(Value::as_str),
            "paths": args.get("paths"),
            "checkpoint_id": args.get("checkpoint_id"),
            "remote": args.get("remote"),
            "branch": args.get("branch"),
            "approval_required": self.side_effecting_for(args),
        })
    }

    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let Some(operation) = args.get("operation").and_then(Value::as_str) else {
            return ToolResult::err("invalid_arguments", "operation is required");
        };
        match operation {
            "status" => run_git(ctx, &["status", "--short", "--branch"], "git status"),
            "diff" => run_git(ctx, &["diff", "--no-ext-diff", "--"], "git diff"),
            "log" => run_git(ctx, &["log", "--oneline", "--decorate", "-20"], "git log"),
            "propose_commit" => propose_commit(ctx),
            "checkpoint_create" => checkpoint_create(ctx, &args),
            "checkpoint_restore" => checkpoint_restore(ctx, &args),
            "commit" => commit(ctx, &args),
            "push" => push(ctx, &args),
            other => ToolResult::err(
                "invalid_arguments",
                format!("unsupported git operation {other:?}; use the documented operation enum"),
            ),
        }
    }
}

fn run_git(ctx: &ToolCtx, args: &[&str], summary: &str) -> ToolResult {
    let output = Command::new("git")
        .arg("-C")
        .arg(&ctx.root)
        .args(args)
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .output();
    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            ToolResult::ok(summary, text).with_exit_code(output.status.code())
        }
        Err(error) => ToolResult::err("git_unavailable", error),
    }
}

fn propose_commit(ctx: &ToolCtx) -> ToolResult {
    let status = capture_git(ctx, &["status", "--short", "--branch"]);
    let diff = capture_git(ctx, &["diff", "--stat", "--no-ext-diff", "--"]);
    ToolResult::ok(
        "prepare focused commit proposal",
        json!({
            "status": status.text,
            "diff_stat": diff.text,
            "note": "This is a recorded proposal only. Commit and push still require explicit approval."
        })
        .to_string(),
    )
    .with_exit_code(status.code.or(diff.code))
}

struct GitOutput {
    text: String,
    code: Option<i32>,
}

fn capture_git(ctx: &ToolCtx, args: &[&str]) -> GitOutput {
    match Command::new("git")
        .arg("-C")
        .arg(&ctx.root)
        .args(args)
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .output()
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            GitOutput {
                text,
                code: output.status.code(),
            }
        }
        Err(error) => GitOutput {
            text: error.to_string(),
            code: None,
        },
    }
}

fn safe_id(value: Option<&Value>, label: &str) -> Result<String, ToolResult> {
    let id = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolResult::err("invalid_arguments", format!("{label} is required")))?;
    if id.len() > CHECKPOINT_MAX
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ToolResult::err(
            "invalid_arguments",
            format!("{label} must be a short safe identifier"),
        ));
    }
    Ok(id.to_string())
}

fn checkpoint_dir(root: &Path) -> PathBuf {
    root.join(".harness").join("checkpoints")
}

fn checkpoint_create(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let id = match safe_id(args.get("checkpoint_id"), "checkpoint_id") {
        Ok(id) => id,
        Err(error) => return error,
    };
    let head = capture_git(ctx, &["rev-parse", "HEAD"]);
    if head.code != Some(0) {
        return ToolResult::err("git_failed", head.text);
    }
    let patch = capture_git(ctx, &["diff", "--binary", "HEAD", "--"]);
    if patch.code != Some(0) {
        return ToolResult::err("git_failed", patch.text);
    }
    let dir = checkpoint_dir(&ctx.root);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return ToolResult::err("checkpoint_failed", error);
    }
    let patch_path = dir.join(format!("{id}.patch"));
    let meta_path = dir.join(format!("{id}.json"));
    let metadata = json!({
        "checkpoint_id": id,
        "head": head.text.trim(),
        "status": capture_git(ctx, &["status", "--short"]).text,
        "untracked_included": false,
    });
    if let Err(error) = std::fs::write(&patch_path, patch.text) {
        return ToolResult::err("checkpoint_failed", error);
    }
    if let Err(error) = std::fs::write(&meta_path, metadata.to_string()) {
        return ToolResult::err("checkpoint_failed", error);
    }
    ToolResult::ok(
        "create Git checkpoint",
        json!({
            "checkpoint_id": id,
            "head": head.text.trim(),
            "patch": super::paths::display(&ctx.root, &patch_path),
            "metadata": super::paths::display(&ctx.root, &meta_path),
            "note": "Tracked changes were captured; untracked files were not included."
        })
        .to_string(),
    )
}

fn checkpoint_restore(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let id = match safe_id(args.get("checkpoint_id"), "checkpoint_id") {
        Ok(id) => id,
        Err(error) => return error,
    };
    let patch_path = checkpoint_dir(&ctx.root).join(format!("{id}.patch"));
    let patch = match std::fs::read(&patch_path) {
        Ok(patch) => patch,
        Err(error) => return ToolResult::err("checkpoint_not_found", error),
    };
    let metadata_path = checkpoint_dir(&ctx.root).join(format!("{id}.json"));
    let metadata: Value = match std::fs::read_to_string(&metadata_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        Some(metadata) => metadata,
        None => {
            return ToolResult::err(
                "checkpoint_not_found",
                "checkpoint metadata is missing or invalid",
            )
        }
    };
    if metadata.get("checkpoint_id").and_then(Value::as_str) != Some(id.as_str())
        || metadata.get("untracked_included").and_then(Value::as_bool) != Some(false)
    {
        return ToolResult::err(
            "checkpoint_inconsistent",
            "checkpoint metadata does not match the requested tracked-only checkpoint",
        );
    }
    let expected_head = metadata
        .get("head")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_head = capture_git(ctx, &["rev-parse", "HEAD"]);
    if current_head.code != Some(0) || current_head.text.trim() != expected_head {
        return ToolResult::err(
            "checkpoint_stale",
            "the repository HEAD changed since this checkpoint; restore refused",
        );
    }
    let current_diff = capture_git(ctx, &["diff", "--binary", "HEAD", "--"]);
    if current_diff.code != Some(0) {
        return ToolResult::err("checkpoint_failed", current_diff.text);
    }
    if !current_diff.text.trim().is_empty() {
        return ToolResult::err(
            "checkpoint_stale",
            "tracked files changed since this checkpoint; restore refused",
        );
    }
    if patch.is_empty() {
        return ToolResult::ok(
            "restore Git checkpoint",
            json!({"checkpoint_id":id,"applied":false,"note":"checkpoint contains no tracked changes"}).to_string(),
        );
    }
    let mut child = match Command::new("git")
        .arg("-C")
        .arg(&ctx.root)
        .args(["apply", "--3way", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return ToolResult::err("git_unavailable", error),
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if let Err(error) = std::io::Write::write_all(stdin, &patch) {
            return ToolResult::err("checkpoint_failed", error);
        }
    }
    match child.wait_with_output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            if output.status.success() {
                ToolResult::ok(
                    "restore Git checkpoint",
                    json!({"checkpoint_id":id,"output":text,"applied":true}).to_string(),
                )
                .with_exit_code(output.status.code())
            } else {
                ToolResult::failed("checkpoint_stale", "restore Git checkpoint", text)
                    .with_exit_code(output.status.code())
            }
        }
        Err(error) => ToolResult::err("checkpoint_failed", error),
    }
}

fn commit(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let message = match args.get("message").and_then(Value::as_str).map(str::trim) {
        Some(message) if !message.is_empty() && message.chars().count() <= MESSAGE_MAX => message,
        _ => return ToolResult::err("invalid_arguments", "message must be 1–120 characters"),
    };
    let Some(paths) = args.get("paths").and_then(Value::as_array) else {
        return ToolResult::err("invalid_arguments", "paths must be an array");
    };
    if paths.is_empty() || paths.len() > 50 {
        return ToolResult::err(
            "invalid_arguments",
            "commit paths must contain 1–50 entries",
        );
    }
    let mut owned = Vec::with_capacity(paths.len());
    for path in paths {
        let Some(path) = path.as_str() else {
            return ToolResult::err("invalid_arguments", "each commit path must be a string");
        };
        if path.is_empty()
            || Path::new(path).is_absolute()
            || path.split('/').any(|part| part == "..")
        {
            return ToolResult::err(
                "invalid_arguments",
                "commit paths must stay relative to the project root",
            );
        }
        owned.push(path.to_string());
    }
    let mut add = Command::new("git");
    add.arg("-C")
        .arg(&ctx.root)
        .arg("add")
        .arg("--")
        .args(&owned);
    if !add.status().map(|status| status.success()).unwrap_or(false) {
        return ToolResult::err("git_failed", "git add refused the focused path set");
    }
    let result = Command::new("git")
        .arg("-C")
        .arg(&ctx.root)
        .args(["commit", "-m", message])
        .output();
    match result {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            ToolResult::ok("git commit", text).with_exit_code(output.status.code())
        }
        Err(error) => ToolResult::err("git_failed", error),
    }
}

fn push(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let remote = args
        .get("remote")
        .and_then(Value::as_str)
        .unwrap_or("origin");
    let branch = args.get("branch").and_then(Value::as_str).unwrap_or("HEAD");
    if !safe_ref(remote) || !safe_ref(branch) {
        return ToolResult::err(
            "invalid_arguments",
            "remote and branch must be simple Git refs",
        );
    }
    run_git(ctx, &["push", remote, branch], "git push")
}

fn safe_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolCtx;

    fn ctx(root: PathBuf) -> ToolCtx {
        ToolCtx {
            root,
            scope: "global".into(),
            request_id: "request".into(),
            step_id: "step".into(),
            diagnostics_cmd: None,
        }
    }

    #[test]
    fn read_operations_are_not_side_effecting_but_controls_are() {
        assert!(!Git.side_effecting_for(&json!({"operation":"status"})));
        assert!(!Git.side_effecting_for(&json!({"operation":"propose_commit"})));
        assert!(Git.side_effecting_for(&json!({"operation":"checkpoint_restore"})));
        assert!(Git.side_effecting_for(&json!({"operation":"commit"})));
        assert!(Git.side_effecting_for(&json!({"operation":"push"})));
    }

    #[test]
    fn invalid_checkpoint_and_commit_inputs_fail_closed() {
        let root = std::env::temp_dir();
        let context = ctx(root);
        assert_eq!(
            Git.run(&context, json!({"operation":"checkpoint_create"}))
                .error_code,
            Some("invalid_arguments")
        );
        assert_eq!(
            Git.run(
                &context,
                json!({"operation":"commit","message":"x","paths":["../outside"]})
            )
            .error_code,
            Some("invalid_arguments")
        );
        assert_eq!(
            Git.run(&context, json!({"operation":"push","remote":"origin;rm"}))
                .error_code,
            Some("invalid_arguments")
        );
    }
}
