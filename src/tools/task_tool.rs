//! `task`: one read-only exploration delegated to a sub-agent. Contract: docs/design/tools.md#task.
//!
//! The tool lives in the registry so the model keeps being offered exactly one schema per tool
//! from a single immutable list (`Registry::standard`). It is deliberately **not** executed
//! through `Registry::invoke`: a sub-agent needs provider calls and steps of its own, while
//! `Tool::run` is synchronous, filesystem-bound and has neither. `src/agent_loop.rs` intercepts
//! the call and hands it to `crate::subagent`; `run` below only refuses, so a missed interception
//! fails loudly instead of quietly skipping the delegation.
use serde_json::Value;

use super::{truncate_chars, Tool, ToolCtx, ToolResult};

/// Bound on the label echoed into summaries and the activity feed. The `prompt` is never shown:
/// it is model text aimed at another model, not a description of what is about to happen.
const SUMMARY_MAX: usize = 80;

pub struct Task;
impl Tool for Task {
    fn name(&self) -> &'static str { "task" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/task.json") }
    /// A sub-agent is offered read-only tools only, so delegating never needs an approval.
    fn side_effecting(&self) -> bool { false }
    fn summary(&self, args: &Value) -> String {
        let label = args.get("description").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if label.is_empty() { "explore (sub-agent)".to_string() } else { format!("explore: {}", truncate_chars(label, SUMMARY_MAX)) }
    }
    fn run(&self, _ctx: &ToolCtx, _args: Value) -> ToolResult {
        ToolResult::err("internal_error", "task is dispatched by the agent loop, not the tool registry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ToolCtx {
        ToolCtx { root: std::env::temp_dir(), scope: "proj".into(), request_id: "r1".into(), step_id: "st1".into(), diagnostics_cmd: None }
    }

    #[test]
    fn the_label_is_bounded_and_the_prompt_never_leaks_into_it() {
        let summary = Task.summary(&json!({ "description": "x".repeat(200), "prompt": "find the secret" }));
        assert!(summary.starts_with("explore: "));
        assert!(summary.chars().count() <= "explore: ".chars().count() + SUMMARY_MAX);
        assert!(!summary.contains("find the secret"), "the permission prompt must not carry model text for another model");
        assert_eq!(Task.summary(&json!({ "prompt": "p" })), "explore (sub-agent)");
    }

    #[test]
    fn a_direct_registry_invocation_refuses_instead_of_pretending_to_explore() {
        let result = Task.run(&ctx(), json!({ "description": "where is X handled", "prompt": "look" }));
        assert_eq!(result.error_code, Some("internal_error"));
        assert!(!Task.side_effecting(), "a read-only sub-agent must never raise an approval");
    }
}
