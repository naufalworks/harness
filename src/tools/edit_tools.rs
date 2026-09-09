//! edit / write. Contract: docs/design/tools.md#edit, #write.
//!
//! Tools never touch the database. A change is *planned* from the current on-disk content
//! (no writes), which lets the loop record `file_changes(applied=0)` and show a diff for
//! approval; `run` then re-plans against disk and applies atomically, and the loop flips the
//! row to `applied=1`. Re-planning inside `run` is deliberate: the file may have changed
//! while a permission request was pending, and the anchors must still hold at write time.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use super::fs_tools::read_text;
use super::{content_hash, line_hash, paths, render_line, textdiff, PendingChange, Tool, ToolCtx, ToolResult};

const WRITE_MAX: usize = 512 * 1024;
const DIFF_CAP: usize = 64 * 1024;
const DIAGNOSTICS_TIMEOUT: u64 = 60;
const DIAGNOSTICS_CAP: usize = 4 * 1024;
/// Lines of fresh `line:hash│text` context returned on either side of a change.
const ECHO_CONTEXT: usize = 2;
/// Lines returned around a stale anchor so the model can re-anchor without another `read`.
const STALE_CONTEXT: usize = 3;

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> { args.get(key).and_then(Value::as_str) }

fn truncate_utf8(text: &mut String, cap: usize, note: &str) {
    if text.len() <= cap { return; }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) { end -= 1; }
    text.truncate(end);
    text.push_str(note);
}

/// Build the change record, including the unified diff stored in `file_changes.diff`.
fn describe(root: &Path, path: PathBuf, action: &'static str, before: Option<String>, after: String) -> PendingChange {
    let display = paths::display(root, &path);
    let diff = textdiff::unified(&display, before.as_deref().unwrap_or(""), &after);
    let mut text = diff.text;
    truncate_utf8(&mut text, DIFF_CAP, "\n…[diff truncated]\n");
    let before_hash = before.as_deref().map(content_hash);
    let after_hash = content_hash(&after);
    PendingChange { path, display, action, before, after, before_hash, after_hash, diff: text, plus: diff.plus, minus: diff.minus }
}

// ---- planning (never writes) ---------------------------------------------------------

fn plan_edit(ctx: &ToolCtx, args: &Value) -> Result<PendingChange, ToolResult> {
    let Some(rel) = arg_str(args, "path") else { return Err(ToolResult::err("invalid_arguments", "path is required")) };
    let Some(new_string) = arg_str(args, "new_string") else { return Err(ToolResult::err("invalid_arguments", "new_string is required")) };
    let path = paths::resolve(&ctx.root, rel).map_err(|e| ToolResult::err(e.code(), e.detail()))?;
    let before = read_text(&path)?;
    // `split_inclusive` keeps each line's terminator, so byte offsets stay exact and
    // `line_hash` (which right-trims) still agrees with what `read` printed.
    let raw: Vec<&str> = before.split_inclusive('\n').collect();

    let Some(anchors) = args.get("anchors").and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return Err(ToolResult::err("invalid_arguments", "at least one anchor is required"));
    };
    let mut parsed: Vec<(usize, &str)> = Vec::new();
    for anchor in anchors {
        let (Some(line), Some(hash)) = (anchor.get("line").and_then(Value::as_u64), anchor.get("hash").and_then(Value::as_str)) else {
            return Err(ToolResult::err("invalid_arguments", "each anchor needs a line number and a hash"));
        };
        if line == 0 { return Err(ToolResult::err("invalid_arguments", "anchor lines are 1-based")); }
        parsed.push((line as usize, hash));
    }

    // Every anchor must still match. Otherwise hand back the current lines so the model can
    // re-anchor from this result instead of paying for another `read`.
    let stale: Vec<usize> = parsed.iter().filter(|(line, hash)| raw.get(line - 1).map(|l| line_hash(l) != **hash).unwrap_or(true)).map(|(line, _)| *line).collect();
    if !stale.is_empty() {
        let low = stale.iter().min().copied().unwrap_or(1).saturating_sub(STALE_CONTEXT).max(1);
        let high = (stale.iter().max().copied().unwrap_or(1) + STALE_CONTEXT).min(raw.len());
        let mut detail = format!("anchors no longer match at lines {stale:?}; the file is now content_hash {} with {} lines. Re-anchor from:\n", content_hash(&before), raw.len());
        for i in low..=high { detail.push_str(&render_line(i, raw[i - 1])); detail.push('\n'); }
        return Err(ToolResult::err("stale_anchor", detail.trim_end()));
    }

    let first = parsed[0].0;
    let last_anchor = parsed.iter().map(|(line, _)| *line).max().unwrap_or(first);
    let end_line = args.get("end_line").and_then(Value::as_u64).map(|v| v as usize).unwrap_or(last_anchor);
    if end_line < first || end_line > raw.len() {
        return Err(ToolResult::err("invalid_arguments", format!("end_line must be between {first} and {} for this file", raw.len())));
    }
    let region_start: usize = raw[..first - 1].iter().map(|l| l.len()).sum();
    let region_end: usize = raw[..end_line].iter().map(|l| l.len()).sum();

    let after = match arg_str(args, "old_string") {
        // An empty needle would match at every position; whole-line mode is the way to say "replace this region".
        Some("") => return Err(ToolResult::err("invalid_arguments", "old_string must not be empty; omit it to replace whole lines")),
        Some(old) => {
            let region = &before[region_start..region_end];
            match region.matches(old).count() {
                0 => return Err(ToolResult::err("no_match", format!("old_string does not occur in lines {first}-{end_line}"))),
                1 => format!("{}{}{}", &before[..region_start], region.replacen(old, new_string, 1), &before[region_end..]),
                n => return Err(ToolResult::err("ambiguous_match", format!("old_string occurs {n} times in lines {first}-{end_line}; include more surrounding text or narrow the region"))),
            }
        }
        None => {
            // Whole-line replacement: keep the region's trailing newline so the file stays well-formed.
            let terminated = raw[end_line - 1].ends_with('\n');
            let mut block = new_string.to_string();
            if !block.is_empty() && terminated && !block.ends_with('\n') { block.push('\n'); }
            format!("{}{}{}", &before[..region_start], block, &before[region_end..])
        }
    };
    Ok(describe(&ctx.root, path, "modify", Some(before), after))
}

fn plan_write(ctx: &ToolCtx, args: &Value) -> Result<PendingChange, ToolResult> {
    let Some(rel) = arg_str(args, "path") else { return Err(ToolResult::err("invalid_arguments", "path is required")) };
    let Some(content) = arg_str(args, "content") else { return Err(ToolResult::err("invalid_arguments", "content is required")) };
    if content.len() > WRITE_MAX {
        return Err(ToolResult::err("too_large", format!("content is {} bytes; the limit is {WRITE_MAX}", content.len())));
    }
    let path = paths::resolve(&ctx.root, rel).map_err(|e| ToolResult::err(e.code(), e.detail()))?;
    let display = paths::display(&ctx.root, &path);
    if path.is_dir() { return Err(ToolResult::err("is_directory", format!("{display} is a directory"))); }
    let existed = path.exists();
    if existed && !args.get("overwrite").and_then(Value::as_bool).unwrap_or(false) {
        return Err(ToolResult::err("exists", format!("{display} already exists; pass overwrite=true or use edit")));
    }
    let before = if existed { Some(read_text(&path)?) } else { None };
    Ok(describe(&ctx.root, path, if existed { "modify" } else { "create" }, before, content.to_string()))
}

// ---- applying ------------------------------------------------------------------------

/// Temp file in the destination directory, then `rename`: a reader sees either the old file
/// or the new one, never a half-written one. Existing permissions are preserved.
fn atomic_write(path: &Path, step: &str, content: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let tag: String = step.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    let tmp = parent.join(format!(".{name}.harness-tmp-{tag}"));
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(meta.permissions().mode()))?;
            }
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
}

/// First and last 1-based line of `after` that differs from `before`, inclusive.
fn changed_span(before: &str, after: &str) -> (usize, usize) {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let limit = a.len().min(b.len());
    let head = (0..limit).take_while(|&i| a[i] == b[i]).count();
    let tail = (0..limit - head).take_while(|&i| a[a.len() - 1 - i] == b[b.len() - 1 - i]).count();
    let first = head + 1;
    (first, b.len().saturating_sub(tail).max(first))
}

fn apply(ctx: &ToolCtx, verb: &str, change: PendingChange) -> ToolResult {
    if change.is_noop() {
        return ToolResult::ok(format!("{} already had this content", change.display), format!("{} is already exactly this content; nothing was written.", change.display));
    }
    if let Some(parent) = change.path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ToolResult::err("write_failed", format!("could not create the parent directory of {}: {e}", change.display));
        }
    }
    if let Err(e) = atomic_write(&change.path, &ctx.step_id, &change.after) {
        return ToolResult::err("write_failed", format!("{}: {e}", change.display));
    }

    let summary = format!("{verb} {} (+{} −{})", change.display, change.plus, change.minus);
    let mut out = format!("{summary}\ncontent_hash: {}\n", change.after_hash);
    // Echo the new region with fresh hashes so the next edit can anchor without re-reading.
    let lines: Vec<&str> = change.after.split_inclusive('\n').collect();
    let (first, last) = changed_span(change.before.as_deref().unwrap_or(""), &change.after);
    let low = first.saturating_sub(ECHO_CONTEXT).max(1);
    let high = (last + ECHO_CONTEXT).min(lines.len());
    for i in low..=high { out.push_str(&render_line(i, lines[i - 1])); out.push('\n'); }

    if let Some(command) = ctx.diagnostics_cmd.as_deref() {
        let run = run_capped(command, &ctx.root, &ctx.scope, DIAGNOSTICS_TIMEOUT, DIAGNOSTICS_CAP);
        out.push_str(&format!("\n[diagnostics {}]\n{}\n", run.label(), run.output));
    }
    ToolResult::ok(summary, out).with_artifact(change.artifact())
}

pub(crate) struct Capped { pub started: bool, pub code: Option<i32>, pub timed_out: bool, pub output: String, pub seconds: u64 }

impl Capped {
    pub fn label(&self) -> String {
        if !self.started { return "could not run".into(); }
        if self.timed_out { return format!("timed out after {}s", self.seconds); }
        match self.code { Some(code) => format!("exit {code}"), None => "killed by a signal".into() }
    }
}

/// Run `command` through `sh -c` with cwd = root, a stripped environment and a wall-clock cap.
/// Output is interleaved through a temp file rather than pipes, so a command that writes more
/// than one pipe buffer cannot deadlock us while we are polling for the timeout.
pub(crate) fn run_capped(command: &str, cwd: &Path, scope: &str, seconds: u64, cap: usize) -> Capped {
    let failed = |message: String| Capped { started: false, code: None, timed_out: false, output: message, seconds };
    let log = std::env::temp_dir().join(format!("harness-cmd-{}.log", uuid::Uuid::new_v4()));
    let Ok(out_handle) = std::fs::File::create(&log) else { return failed("could not create a temporary log for the command".into()) };
    let Ok(err_handle) = out_handle.try_clone() else {
        let _ = std::fs::remove_file(&log);
        return failed("could not duplicate the temporary log handle".into());
    };
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::from(out_handle)).stderr(Stdio::from(err_handle));
    cmd.env_clear();
    #[cfg(unix)]
    {
        // Give the command its own process group: the timeout below can then kill the whole
        // tree, and a job the caller detaches is out of reach of signals aimed at the harness.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    for key in ["PATH", "HOME", "LANG", "LC_ALL", "TERM"] {
        if let Ok(value) = std::env::var(key) { cmd.env(key, value); }
    }
    cmd.env("HARNESS_SCOPE", scope);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => { let _ = std::fs::remove_file(&log); return failed(format!("could not start the command: {e}")); }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let (mut code, mut timed_out) = (None, false);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => { code = status.code(); break; }
            Ok(None) => {}
            Err(_) => break,
        }
        if std::time::Instant::now() >= deadline {
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut output = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    truncate_utf8(&mut output, cap, "\n…[output truncated]");
    Capped { started: true, code, timed_out, output: output.trim_end().to_string(), seconds }
}

/// Kill the child's whole process group, not just the shell that leads it. This crate has no
/// `libc` dependency, so shell out to `kill`; a failure is harmless because the caller kills
/// the direct child as well.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let _ = Command::new("sh").arg("-c").arg(format!("kill -9 -{pid} 2>/dev/null"))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}
#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

// ---- tools ---------------------------------------------------------------------------

fn payload(planned: Result<PendingChange, ToolResult>, args: &Value) -> Value {
    match planned {
        Ok(c) => json!({ "path": c.display, "action": c.action, "plus": c.plus, "minus": c.minus, "before_hash": c.before_hash, "after_hash": c.after_hash, "diff": c.diff }),
        Err(r) => json!({ "path": arg_str(args, "path"), "error": r.error_code }),
    }
}

pub struct Edit;
impl Tool for Edit {
    fn name(&self) -> &'static str { "edit" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/edit.json") }
    fn side_effecting(&self) -> bool { true }
    // `summary` has no filesystem access, so the line counts live in `permission_payload`.
    fn summary(&self, a: &Value) -> String { format!("edit {}", arg_str(a, "path").unwrap_or("?")) }
    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value { payload(plan_edit(ctx, args), args) }
    fn plan(&self, ctx: &ToolCtx, args: &Value) -> Option<Result<PendingChange, ToolResult>> { Some(plan_edit(ctx, args)) }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        match plan_edit(ctx, &args) { Ok(change) => apply(ctx, "edited", change), Err(refusal) => refusal }
    }
}

pub struct Write;
impl Tool for Write {
    fn name(&self) -> &'static str { "write" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/write.json") }
    fn side_effecting(&self) -> bool { true }
    fn summary(&self, a: &Value) -> String { format!("write {}", arg_str(a, "path").unwrap_or("?")) }
    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value { payload(plan_write(ctx, args), args) }
    fn plan(&self, ctx: &ToolCtx, args: &Value) -> Option<Result<PendingChange, ToolResult>> { Some(plan_write(ctx, args)) }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        match plan_write(ctx, &args) { Ok(change) => apply(ctx, "wrote", change), Err(refusal) => refusal }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Artifact, ToolStatus};

    const LIB: &str = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n";

    fn project() -> (PathBuf, ToolCtx) {
        let dir = std::env::temp_dir().join(format!("harness-edit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), LIB).unwrap();
        std::fs::write(dir.join("src/dup.rs"), "let x = 1;\nlet x = 1;\n").unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let ctx = ToolCtx { root: root.clone(), scope: "global".into(), request_id: "request".into(), step_id: "step-1".into(), diagnostics_cmd: None };
        (root, ctx)
    }
    fn beta_anchor() -> Value { json!([{ "line": 2, "hash": line_hash("fn beta() {}") }]) }
    fn read_back(root: &Path, rel: &str) -> String { std::fs::read_to_string(root.join(rel)).unwrap() }

    #[test] fn edit_replaces_whole_lines_and_echoes_fresh_anchors() {
        let (root, ctx) = project();
        let out = Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": beta_anchor(), "new_string": "fn beta(x: u8) {}" }));
        assert_eq!(out.status, ToolStatus::Complete, "{}", out.content);
        assert_eq!(read_back(&root, "src/lib.rs"), "fn alpha() {}\nfn beta(x: u8) {}\nfn gamma() {}\n");
        assert!(out.content.contains("(+1 −1)"), "{}", out.content);
        assert!(out.content.contains(&render_line(2, "fn beta(x: u8) {}")), "{}", out.content);
        assert!(matches!(out.artifacts.first(), Some(Artifact::FileChange { action: "modify", plus: 1, minus: 1, .. })), "{:?}", out.artifacts);
        std::fs::remove_dir_all(root).ok();
    }

    #[test] fn edit_refuses_a_stale_anchor_without_writing() {
        let (root, ctx) = project();
        assert_ne!(line_hash("fn beta() {}"), "0000", "the test's deliberately wrong hash must differ from the real one");
        let out = Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": [{ "line": 2, "hash": "0000" }], "new_string": "x" }));
        assert_eq!(out.error_code, Some("stale_anchor"));
        assert!(out.content.contains("fn beta() {}"), "the refusal must hand back current lines: {}", out.content);
        assert_eq!(read_back(&root, "src/lib.rs"), LIB, "a stale anchor must leave the file untouched");
        // A line past the end of the file is stale too, not a panic.
        assert_eq!(Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": [{ "line": 99, "hash": "0000" }], "new_string": "x" })).error_code, Some("stale_anchor"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test] fn edit_requires_old_string_to_occur_exactly_once_in_the_region() {
        let (root, ctx) = project();
        let missing = Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": beta_anchor(), "old_string": "delta", "new_string": "x" }));
        assert_eq!(missing.error_code, Some("no_match"));
        let duplicated = Edit.run(&ctx, json!({ "path": "src/dup.rs", "anchors": [{ "line": 1, "hash": line_hash("let x = 1;") }], "end_line": 2, "old_string": "let x = 1;", "new_string": "let y = 2;" }));
        assert_eq!(duplicated.error_code, Some("ambiguous_match"));
        assert_eq!(read_back(&root, "src/dup.rs"), "let x = 1;\nlet x = 1;\n");
        assert_eq!(Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": beta_anchor(), "old_string": "", "new_string": "x" })).error_code, Some("invalid_arguments"));
        let single = Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": beta_anchor(), "old_string": "beta", "new_string": "delta" }));
        assert_eq!(single.status, ToolStatus::Complete, "{}", single.content);
        assert_eq!(read_back(&root, "src/lib.rs"), "fn alpha() {}\nfn delta() {}\nfn gamma() {}\n");
        std::fs::remove_dir_all(root).ok();
    }

    #[test] fn edit_plans_the_change_without_touching_disk() {
        let (root, ctx) = project();
        let planned = Edit.plan(&ctx, &json!({ "path": "src/lib.rs", "anchors": beta_anchor(), "new_string": "fn beta(x: u8) {}" })).unwrap().unwrap();
        assert_eq!((planned.action, planned.plus, planned.minus), ("modify", 1, 1));
        assert!(planned.diff.contains("+fn beta(x: u8) {}"), "{}", planned.diff);
        assert_eq!(planned.before_hash.as_deref(), Some(content_hash(LIB).as_str()));
        assert_eq!(read_back(&root, "src/lib.rs"), LIB, "plan must never write");
        std::fs::remove_dir_all(root).ok();
    }

    #[test] fn write_creates_refuses_and_overwrites_as_documented() {
        let (root, ctx) = project();
        assert_eq!(Write.run(&ctx, json!({ "path": "src/lib.rs", "content": "x" })).error_code, Some("exists"));
        assert_eq!(read_back(&root, "src/lib.rs"), LIB);
        let created = Write.run(&ctx, json!({ "path": "docs/notes/new.md", "content": "hello\n" }));
        assert_eq!(created.status, ToolStatus::Complete, "{}", created.content);
        assert_eq!(read_back(&root, "docs/notes/new.md"), "hello\n", "parent directories are created inside root");
        assert!(matches!(created.artifacts.first(), Some(Artifact::FileChange { action: "create", before_hash: None, .. })), "{:?}", created.artifacts);
        let replaced = Write.run(&ctx, json!({ "path": "docs/notes/new.md", "content": "bye\n", "overwrite": true }));
        assert!(matches!(replaced.artifacts.first(), Some(Artifact::FileChange { action: "modify", .. })), "{:?}", replaced.artifacts);
        let repeated = Write.run(&ctx, json!({ "path": "docs/notes/new.md", "content": "bye\n", "overwrite": true }));
        assert_eq!(repeated.status, ToolStatus::Complete);
        assert!(repeated.artifacts.is_empty(), "an identical write records no file change: {}", repeated.content);
        assert_eq!(Write.run(&ctx, json!({ "path": ".env", "content": "x", "overwrite": true })).error_code, Some("path_denied"));
        assert_eq!(Write.run(&ctx, json!({ "path": "big.txt", "content": "x".repeat(WRITE_MAX + 1) })).error_code, Some("too_large"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test] fn edit_appends_diagnostics_and_leaves_no_temp_file_behind() {
        let (root, mut ctx) = project();
        ctx.diagnostics_cmd = Some("echo checked; exit 3".into());
        let out = Edit.run(&ctx, json!({ "path": "src/lib.rs", "anchors": beta_anchor(), "new_string": "fn beta(x: u8) {}" }));
        assert_eq!(out.status, ToolStatus::Complete, "{}", out.content);
        assert!(out.content.contains("[diagnostics exit 3]"), "{}", out.content);
        assert!(out.content.contains("checked"), "{}", out.content);
        let leftovers: Vec<String> = std::fs::read_dir(root.join("src")).unwrap().flatten()
            .map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.contains("harness-tmp")).collect();
        assert!(leftovers.is_empty(), "atomic write left {leftovers:?} behind");
        std::fs::remove_dir_all(root).ok();
    }

    #[test] fn capped_runs_report_a_timeout_instead_of_hanging() {
        let root = std::env::temp_dir();
        let run = run_capped("sleep 5", &root, "global", 1, DIAGNOSTICS_CAP);
        assert!(run.timed_out, "expected the wall-clock cap to fire");
        assert!(run.label().contains("timed out"));
        let quiet = run_capped("printf 'ok'", &root, "global", 10, DIAGNOSTICS_CAP);
        assert_eq!((quiet.code, quiet.output.as_str()), (Some(0), "ok"));
    }
}
