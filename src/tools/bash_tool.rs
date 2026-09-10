//! `bash`: run a shell command in the scope root. Contract: docs/design/tools.md#bash.
//!
//! Foreground calls are bounded in wall-clock time and output size, and the output ends with
//! `[exit N]`. Background calls are detached, log to `<root>/.harness/logs/<step_id>.log` and
//! return a pid immediately, so a dev server cannot hold a turn open. The destructive-command
//! deny-list lives in `super::is_dangerous_command`, because the permission gate consults it
//! without building a tool; it forces an approval even in `auto_all`.
use serde_json::{json, Value};

use super::edit_tools::run_capped;
use super::{is_dangerous_command, paths, truncate_chars, Tool, ToolCtx, ToolResult, MAX_OUTPUT};

pub const DEFAULT_TIMEOUT: u64 = 120;
pub const MAX_TIMEOUT: u64 = 600;
/// Leaves room under `MAX_OUTPUT` for the `[exit N]` trailer and the truncation notice.
const OUTPUT_CAP: usize = MAX_OUTPUT - 1024;
const COMMAND_MAX: usize = 8 * 1024;
const DESCRIPTION_MAX: usize = 80;
const COMMAND_SUMMARY: usize = 60;
/// A detached spawn only has to fork and print a pid; it never waits for the job.
const SPAWN_TIMEOUT: u64 = 10;
const LOG_DIR: &str = ".harness/logs";

struct Call {
    command: String,
    timeout: u64,
    clamped: bool,
    background: bool,
}

fn invalid(detail: &str) -> ToolResult {
    ToolResult::err("invalid_arguments", detail)
}

fn parse(args: &Value) -> Result<Call, ToolResult> {
    let command = match args.get("command") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(Value::String(_)) | None => {
            return Err(invalid("command is required and must not be blank"))
        }
        Some(_) => return Err(invalid("command must be a string")),
    };
    if command.len() > COMMAND_MAX {
        return Err(invalid("command must be at most 8 KiB"));
    }
    if command.contains('\0') {
        return Err(invalid("command must not contain a NUL byte"));
    }
    match args.get("description") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return Err(invalid("description must be a string")),
    }
    // Over the maximum is clamped rather than refused: the cap is ours, not the model's error,
    // and losing a turn to it teaches the model nothing. Anything that is not a whole number of
    // seconds is a real mistake and comes back as `invalid_arguments`.
    let (timeout, clamped) = match args.get("timeout_seconds") {
        None | Some(Value::Null) => (DEFAULT_TIMEOUT, false),
        Some(v) => match v.as_i64() {
            Some(n) if n >= 1 => (n.min(MAX_TIMEOUT as i64) as u64, n > MAX_TIMEOUT as i64),
            _ => {
                return Err(invalid(
                    "timeout_seconds must be a whole number of seconds, at least 1",
                ))
            }
        },
    };
    let background = match args.get("background") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid("background must be a boolean")),
    };
    Ok(Call {
        command,
        timeout,
        clamped,
        background,
    })
}

/// Step title and permission prompt: the model's `description`, else the head of the command.
fn label(args: &Value) -> String {
    if let Some(text) = args.get("description").and_then(Value::as_str) {
        let text = text.trim();
        if !text.is_empty() {
            return truncate_chars(text, DESCRIPTION_MAX);
        }
    }
    match args.get("command").and_then(Value::as_str) {
        Some(command) => truncate_chars(command.trim(), COMMAND_SUMMARY),
        None => "bash".into(),
    }
}

fn push_line(text: &mut String, line: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
}

/// POSIX single-quote, so the outer shell hands the command to the inner one untouched.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Step ids come from the harness, not the model, but a stray separator would still escape the
/// log directory, so the name is reduced to a safe stem.
fn log_name(step_id: &str) -> String {
    let safe: String = step_id
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if safe.is_empty() {
        format!("step-{}", uuid::Uuid::new_v4())
    } else {
        safe
    }
}

fn foreground(ctx: &ToolCtx, call: &Call, summary: String) -> ToolResult {
    let run = run_capped(
        &call.command,
        &ctx.root,
        &ctx.scope,
        call.timeout,
        OUTPUT_CAP,
    );
    if !run.started {
        return ToolResult::err("spawn_failed", &run.output);
    }
    let mut out = run.output.clone();
    if call.clamped {
        push_line(
            &mut out,
            &format!("[timeout_seconds capped at the {MAX_TIMEOUT}s maximum]"),
        );
    }
    push_line(&mut out, &format!("[{}]", run.label()));
    if run.timed_out {
        return ToolResult::failed("timeout", summary, out);
    }
    match run.code {
        // A non-zero exit is information, not a broken step: a red test run has to come back as
        // output so the next step can react to it.
        Some(code) => ToolResult::ok(summary, out).with_exit_code(Some(code)),
        // Killed from outside, and not by our own timeout.
        None => ToolResult::failed("signal", summary, out),
    }
}

fn background(ctx: &ToolCtx, call: &Call, summary: String) -> ToolResult {
    let dir = ctx.root.join(LOG_DIR);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return ToolResult::err(
            "spawn_failed",
            format!("could not create {}: {e}", paths::display(&ctx.root, &dir)),
        );
    }
    let log = dir.join(format!("{}.log", log_name(&ctx.step_id)));
    let display = paths::display(&ctx.root, &log);
    // The outer shell backgrounds the job and exits immediately, so the job is reparented to
    // init and we never have to reap it; `run_capped` has already put that shell in its own
    // process group, which the job inherits, so a signal aimed at the harness cannot reach it.
    // `setsid` (per the design note) is not on macOS, and the versions that fork print the
    // wrapper's pid rather than the job's, so `$!` from the shell is the more honest answer.
    let script = format!(
        "sh -c {command} >>{log} 2>&1 &\nprintf %s \"$!\"\n",
        command = quote(&call.command),
        log = quote(&log.to_string_lossy()),
    );
    let spawn = run_capped(&script, &ctx.root, &ctx.scope, SPAWN_TIMEOUT, 256);
    let pid = spawn
        .output
        .split_whitespace()
        .last()
        .and_then(|t| t.parse::<u32>().ok());
    match pid {
        Some(pid) => ToolResult::ok(summary, json!({
            "pid": pid,
            "log": display,
            "note": format!("started detached; read it with `bash tail -n 50 {display}` and stop it with `bash kill {pid}`"),
        }).to_string()),
        None => ToolResult::err("spawn_failed", format!("could not start the command in the background ({}): {}", spawn.label(), spawn.output)),
    }
}

pub struct Bash;
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn schema(&self) -> &'static str {
        include_str!("../../tools/schemas/bash.json")
    }
    fn side_effecting(&self) -> bool {
        true
    }
    fn summary(&self, args: &Value) -> String {
        label(args)
    }
    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value {
        match parse(args) {
            Ok(call) => json!({
                "command": call.command,
                "description": args.get("description").and_then(Value::as_str),
                "cwd": ctx.root.to_string_lossy(),
                "timeout_seconds": call.timeout,
                "background": call.background,
                "dangerous": is_dangerous_command(&call.command),
            }),
            Err(refusal) => json!({ "command": args.get("command"), "error": refusal.error_code }),
        }
    }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let summary = label(&args);
        match parse(&args) {
            Ok(call) if call.background => background(ctx, &call, summary),
            Ok(call) => foreground(ctx, &call, summary),
            Err(refusal) => refusal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{PermissionMode, Registry, ToolStatus};

    fn project() -> (std::path::PathBuf, ToolCtx) {
        let dir = std::env::temp_dir().join(format!("harness-bash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), "hello from the project\n").unwrap();
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

    #[test]
    fn runs_in_the_root_with_a_stripped_environment() {
        let (_root, ctx) = project();
        let r = Bash.run(
            &ctx,
            json!({ "command": "cat hello.txt", "description": "read the fixture" }),
        );
        assert_eq!(r.status, ToolStatus::Complete);
        assert_eq!(r.exit_code, Some(0));
        assert_eq!(r.summary, "read the fixture");
        assert!(
            r.content.contains("hello from the project"),
            "{}",
            r.content
        );
        assert!(r.content.trim_end().ends_with("[exit 0]"), "{}", r.content);
        // The scope is exported; nothing else of ours is.
        let env = Bash.run(
            &ctx,
            json!({ "command": "printf %s \"$HARNESS_SCOPE\"", "description": "scope" }),
        );
        assert!(env.content.contains("global"), "{}", env.content);
    }

    #[test]
    fn a_failing_command_is_output_not_a_broken_step() {
        let (_root, ctx) = project();
        let r = Bash.run(
            &ctx,
            json!({ "command": "echo boom >&2; exit 3", "description": "fail" }),
        );
        assert_eq!(r.status, ToolStatus::Complete);
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(r.error_code, None);
        assert!(r.content.contains("boom"), "{}", r.content);
        assert!(r.content.trim_end().ends_with("[exit 3]"), "{}", r.content);
    }

    #[test]
    fn a_timeout_kills_the_command_and_keeps_the_partial_output() {
        let (_root, ctx) = project();
        let r = Bash.run(&ctx, json!({ "command": "echo started; sleep 30", "description": "hang", "timeout_seconds": 1 }));
        assert_eq!(r.status, ToolStatus::Failed);
        assert_eq!(r.error_code, Some("timeout"));
        assert!(r.content.contains("started"), "{}", r.content);
        assert!(r.content.contains("timed out after 1s"), "{}", r.content);
    }

    #[test]
    fn an_over_long_timeout_is_capped_instead_of_refused() {
        let (_root, ctx) = project();
        let r = Bash.run(
            &ctx,
            json!({ "command": "true", "description": "no-op", "timeout_seconds": 5000 }),
        );
        assert_eq!(r.exit_code, Some(0));
        assert!(
            r.content.contains("capped at the 600s maximum"),
            "{}",
            r.content
        );
    }

    #[test]
    fn refuses_arguments_it_cannot_run() {
        let (_root, ctx) = project();
        for args in [
            json!({ "description": "no command at all" }),
            json!({ "command": "   " }),
            json!({ "command": 7 }),
            json!({ "command": "true", "timeout_seconds": 0 }),
            json!({ "command": "true", "timeout_seconds": 1.5 }),
            json!({ "command": "true", "background": "yes" }),
            json!({ "command": "true", "description": 3 }),
        ] {
            let r = Bash.run(&ctx, args.clone());
            assert_eq!(
                r.error_code,
                Some("invalid_arguments"),
                "{args} -> {}",
                r.content
            );
        }
    }

    #[test]
    fn background_returns_a_pid_and_a_log_under_the_root() {
        let (root, ctx) = project();
        let r = Bash.run(&ctx, json!({ "command": "printf 'started\\n'; sleep 5", "description": "serve", "background": true }));
        assert_eq!(r.status, ToolStatus::Complete, "{}", r.content);
        let body: Value = serde_json::from_str(&r.content).unwrap();
        let pid = body["pid"].as_u64().unwrap();
        assert!(pid > 1, "{body}");
        assert_eq!(body["log"].as_str().unwrap(), ".harness/logs/step-1.log");
        let log = root.join(".harness/logs/step-1.log");
        let mut text = String::new();
        for _ in 0..100 {
            text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("started") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(text.contains("started"), "log held {text:?}");
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill -9 -{pid} {pid} 2>/dev/null"))
            .status();
    }

    #[test]
    fn the_deny_list_always_needs_permission_even_in_auto_all() {
        let registry = Registry::standard();
        let bash = registry.get("bash").expect("bash is registered");
        let danger = json!({ "command": "rm -rf / --no-preserve-root", "description": "clean" });
        let safe = json!({ "command": "cargo test", "description": "run the tests" });
        assert!(registry.requires_permission(bash, &danger, PermissionMode::AutoAll));
        assert!(!registry.requires_permission(bash, &safe, PermissionMode::AutoAll));
        // Every command still asks in the two stricter modes.
        assert!(registry.requires_permission(bash, &safe, PermissionMode::AutoEdit));
        assert!(registry.requires_permission(bash, &safe, PermissionMode::Ask));
        // Padding does not hide a force-push: the check normalizes whitespace first.
        assert!(registry.requires_permission(
            bash,
            &json!({ "command": "git   push    --force origin main" }),
            PermissionMode::AutoAll
        ));
    }

    #[test]
    fn the_summary_prefers_the_description_and_the_payload_carries_the_risk() {
        let (_root, ctx) = project();
        assert_eq!(
            Bash.summary(&json!({ "command": "ls", "description": "list files" })),
            "list files"
        );
        let long = format!("echo {}", "x".repeat(200));
        assert_eq!(
            Bash.summary(&json!({ "command": long })).chars().count(),
            COMMAND_SUMMARY
        );
        let payload = Bash.permission_payload(
            &ctx,
            &json!({ "command": "rm -rf /", "description": "nope" }),
        );
        assert_eq!(payload["dangerous"], json!(true));
        assert_eq!(payload["timeout_seconds"], json!(DEFAULT_TIMEOUT));
        assert_eq!(payload["cwd"], json!(ctx.root.to_string_lossy()));
        assert_eq!(payload["background"], json!(false));
    }
}
