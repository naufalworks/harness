//! Tool registry for the agentic turn. Contract: docs/design/tools.md.
//! Every tool is synchronous and filesystem/process bound; the loop runs it in
//! `spawn_blocking`. Results are sanitized and capped here, never by the caller.
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use crate::safety;

pub mod paths;
pub mod fs_tools;
pub mod textdiff;
pub mod edit_tools;
pub mod ast_edit_tool;
pub mod bash_tool;
pub mod meta_tools;
pub mod skill_tool;
pub mod task_tool;

pub const MAX_OUTPUT: usize = 32 * 1024;
const HEAD: usize = 24 * 1024;
const TAIL: usize = 8 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PermissionMode { Ask, AutoEdit, AutoAll }
impl PermissionMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s { "ask" => Some(Self::Ask), "auto_edit" => Some(Self::AutoEdit), "auto_all" => Some(Self::AutoAll), _ => None }
    }
    pub fn as_str(self) -> &'static str { match self { Self::Ask => "ask", Self::AutoEdit => "auto_edit", Self::AutoAll => "auto_all" } }
}

/// Per-turn context handed to every tool invocation.
#[derive(Clone, Debug)]
pub struct ToolCtx {
    pub root: PathBuf,
    pub scope: String,
    /// Unread today: the loop names the turn in every row it writes, so no tool needs it yet.
    /// Kept because P2-T03's revert has to attribute a change to the turn that made it.
    pub request_id: String,
    pub step_id: String,
    pub diagnostics_cmd: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolStatus { Complete, Failed }

/// Side effects a tool wants persisted; the loop writes them so the DB stays out of tools.
#[derive(Clone, Debug)]
pub enum Artifact {
    FileChange { path: String, action: &'static str, before_hash: Option<String>, after_hash: Option<String>, diff: String, plus: usize, minus: usize },
    Plan { items: Vec<(String, String)> },
}

/// A file change computed from the current on-disk content but **not yet written**.
/// Permission payloads render it for approval; `Tool::run` re-plans, applies atomically, and
/// returns its artifact for the loop to record as `file_changes(applied=1)`.
#[derive(Clone, Debug)]
pub struct PendingChange {
    pub path: PathBuf,
    /// Root-relative path shown to the model, the UI and the `file_changes` row.
    pub display: String,
    pub action: &'static str,
    pub before: Option<String>,
    pub after: String,
    pub before_hash: Option<String>,
    pub after_hash: String,
    pub diff: String,
    pub plus: usize,
    pub minus: usize,
}

impl PendingChange {
    pub fn artifact(&self) -> Artifact {
        Artifact::FileChange { path: self.display.clone(), action: self.action, before_hash: self.before_hash.clone(), after_hash: Some(self.after_hash.clone()), diff: self.diff.clone(), plus: self.plus, minus: self.minus }
    }
    /// Rewriting a file with the content it already has is not a change worth recording.
    pub fn is_noop(&self) -> bool { self.before.as_deref() == Some(self.after.as_str()) }
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub content: String,
    pub bytes: usize,
    pub truncated: bool,
    pub status: ToolStatus,
    pub summary: String,
    pub error_code: Option<&'static str>,
    /// Process exit status, for `tool_finished {exit_code?}`. Only `bash` sets it.
    pub exit_code: Option<i32>,
    pub artifacts: Vec<Artifact>,
}

impl ToolResult {
    pub fn ok(summary: impl Into<String>, content: String) -> Self {
        Self { content, bytes: 0, truncated: false, status: ToolStatus::Complete, summary: summary.into(), error_code: None, exit_code: None, artifacts: Vec::new() }.finish()
    }
    pub fn err(code: &'static str, detail: impl std::fmt::Display) -> Self {
        let content = json!({ "error": code, "detail": detail.to_string() }).to_string();
        Self { content, bytes: 0, truncated: false, status: ToolStatus::Failed, summary: format!("error: {code}"), error_code: Some(code), exit_code: None, artifacts: Vec::new() }.finish()
    }
    /// A failure whose *output* is still the useful part — a timed-out or signalled command.
    /// Unlike `err`, the content stays the command's own output instead of a JSON envelope.
    pub fn failed(code: &'static str, summary: impl Into<String>, content: String) -> Self {
        Self { content, bytes: 0, truncated: false, status: ToolStatus::Failed, summary: summary.into(), error_code: Some(code), exit_code: None, artifacts: Vec::new() }.finish()
    }
    pub fn with_artifact(mut self, a: Artifact) -> Self { self.artifacts.push(a); self }
    pub fn with_exit_code(mut self, code: Option<i32>) -> Self { self.exit_code = code; self }
    /// Redact and cap. Idempotent.
    pub fn finish(mut self) -> Self {
        let redacted = safety::redact(&self.content);
        let raw_len = redacted.len();
        if raw_len > MAX_OUTPUT {
            let head_end = floor_char(&redacted, HEAD);
            let tail_start = ceil_char(&redacted, raw_len - TAIL);
            self.content = format!("{}\n…[{} bytes omitted]…\n{}", &redacted[..head_end], tail_start - head_end, &redacted[tail_start..]);
            self.truncated = true;
        } else {
            self.content = redacted;
        }
        self.bytes = raw_len;
        self
    }
}

fn floor_char(s: &str, mut i: usize) -> usize { while i > 0 && !s.is_char_boundary(i) { i -= 1; } i }
fn ceil_char(s: &str, mut i: usize) -> usize { while i < s.len() && !s.is_char_boundary(i) { i += 1; } i }

/// Shorten a label to `max` characters (not bytes), with an ellipsis when it had to cut.
/// Used for summaries and permission prompts, never for tool output.
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max { return text.to_string(); }
    text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> &'static str;
    fn side_effecting(&self) -> bool;
    /// Human summary of a call; also used as the permission prompt. Never model text.
    fn summary(&self, args: &Value) -> String;
    /// Extra permission payload for the UI (diff preview, command). Default: the args.
    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value { let _ = ctx; args.clone() }
    /// File-mutating tools may describe their change here without touching disk. The current
    /// loop gets its pre-write diff from `permission_payload`; this hook remains available to
    /// consumers that need a typed pending change. `None` for non-file tools.
    fn plan(&self, ctx: &ToolCtx, args: &Value) -> Option<std::result::Result<PendingChange, ToolResult>> { let _ = (ctx, args); None }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult;
}

pub struct Registry { tools: Vec<Box<dyn Tool>> }

impl Registry {
    pub fn standard() -> Self {
        Self { tools: vec![
            Box::new(fs_tools::Read), Box::new(fs_tools::Grep), Box::new(fs_tools::Glob),
            Box::new(edit_tools::Edit), Box::new(edit_tools::Write), Box::new(ast_edit_tool::AstEdit),
            Box::new(bash_tool::Bash),
            Box::new(meta_tools::Think), Box::new(meta_tools::TodoWrite),
            Box::new(skill_tool::Skill),
            // Registered like any other tool so the model sees one list, but dispatched by the
            // loop (src/agent_loop.rs), never by `invoke`: a sub-agent needs provider calls.
            Box::new(task_tool::Task),
        ] }
    }
    pub fn get(&self, name: &str) -> Option<&dyn Tool> { self.tools.iter().find(|t| t.name() == name).map(|b| b.as_ref()) }
    /// The loop's only entry point. `ctx` is `None` when the scope has no `root_path`,
    /// and then every tool refuses instead of touching the filesystem (P1-T04).
    pub fn invoke(&self, ctx: Option<&ToolCtx>, name: &str, args: Value) -> ToolResult {
        let Some(ctx) = ctx else {
            return ToolResult::err("tools_disabled", "this scope has no root_path; configure one with POST /scopes/{scope} before using tools");
        };
        match self.get(name) {
            Some(tool) => tool.run(ctx, args),
            None => ToolResult::err("unknown_tool", name),
        }
    }
    /// OpenAI `tools` array built from tools/schemas/*.json (embedded at compile time).
    pub fn schemas(&self) -> Result<Vec<Value>> {
        self.tools.iter().map(|t| Ok(serde_json::from_str(t.schema())?)).collect()
    }
    /// Decide whether a call needs a human approval given the scope mode.
    pub fn requires_permission(&self, tool: &dyn Tool, args: &Value, mode: PermissionMode) -> bool {
        if !tool.side_effecting() { return false; }
        match (tool.name(), mode) {
            (_, PermissionMode::Ask) => true,
            ("bash", PermissionMode::AutoEdit) => true,
            ("bash", PermissionMode::AutoAll) => is_dangerous_command(args.get("command").and_then(Value::as_str).unwrap_or("")),
            (_, PermissionMode::AutoEdit | PermissionMode::AutoAll) => false,
        }
    }
}

/// Deny-list from docs/design/tools.md#bash: these always ask, even in `auto_all`.
/// Lives here (not in bash_tool) so the permission gate compiles before P1-T08 lands.
pub fn is_dangerous_command(cmd: &str) -> bool {
    let c = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    const PATTERNS: &[&str] = &["rm -rf /", "rm -rf ~", "rm -rf *", "git push --force", "git push -f", "git reset --hard", "git clean -fd", "mkfs", "dd if=", "> /dev/sd", "chmod -R 777 /", "curl | sh", "wget | sh", ":(){"];
    PATTERNS.iter().any(|p| c.contains(p)) || c.contains("| sh") && (c.contains("curl ") || c.contains("wget "))
}

/// First 4 hex chars of SHA-256 over the right-trimmed line. Anchors are `line:hash` pairs.
pub fn line_hash(line: &str) -> String { hex4(line.trim_end()) }
/// First 8 hex chars of SHA-256 over the whole file.
pub fn content_hash(content: &str) -> String { hex(content.as_bytes(), 8) }
fn hex4(s: &str) -> String { hex(s.as_bytes(), 4) }
fn hex(bytes: &[u8], n: usize) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = String::with_capacity(n);
    for b in digest.as_ref() { out.push_str(&format!("{b:02x}")); if out.len() >= n { break; } }
    out.truncate(n);
    out
}

/// `line:hash│text` rendering shared by read/grep/edit.
pub fn render_line(no: usize, text: &str) -> String {
    let mut t = text.to_string();
    if t.chars().count() > 2000 { t = t.chars().take(2000).collect::<String>() + "…"; }
    format!("{no}:{}│{t}", line_hash(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx() -> ToolCtx {
        ToolCtx { root: std::env::temp_dir(), scope: "global".into(), request_id: "request".into(), step_id: "step".into(), diagnostics_cmd: None }
    }
    #[test] fn scopes_without_root_path_refuse_every_tool() {
        let refused = Registry::standard().invoke(None, "read", json!({ "path": "src/main.rs" }));
        assert_eq!(refused.status, ToolStatus::Failed);
        assert_eq!(refused.error_code, Some("tools_disabled"));
        assert!(refused.content.contains("root_path"));
    }
    #[test] fn scopes_with_a_root_path_still_reject_unknown_tools() {
        let unknown = Registry::standard().invoke(Some(&ctx()), "teleport", json!({}));
        assert_eq!(unknown.error_code, Some("unknown_tool"));
    }
}
