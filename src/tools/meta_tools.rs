//! `think` and `todo_write`: the two tools that change nothing outside the harness.
//! Contract: docs/design/tools.md#think, #todo_write.
//!
//! Neither is side-effecting, so neither ever waits for an approval — a scratchpad note and a
//! plan update must not be able to stall a turn. `todo_write` validates and normalizes here and
//! hands the result to the loop as `Artifact::Plan`; the loop writes it while finishing the step,
//! so tools still never touch the database.
use serde_json::{json, Value};

use super::{truncate_chars, Artifact, Tool, ToolCtx, ToolResult};
use crate::storage::{MAX_PLAN_ITEMS, MAX_PLAN_TEXT, PLAN_STATUSES};

const THOUGHT_MAX: usize = 4000;
/// How much of the in-progress item fits in a one-line summary.
const CURRENT_MAX: usize = 48;

pub struct Think;
impl Tool for Think {
    fn name(&self) -> &'static str { "think" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/think.json") }
    fn side_effecting(&self) -> bool { false }
    fn summary(&self, _args: &Value) -> String { "noted".into() }
    fn run(&self, _ctx: &ToolCtx, args: Value) -> ToolResult {
        let thought = args.get("thought").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
        let Some(thought) = thought else {
            return ToolResult::err("invalid_arguments", "thought is required and must not be blank");
        };
        if thought.chars().count() > THOUGHT_MAX {
            return ToolResult::err("too_large", format!("a thought holds at most {THOUGHT_MAX} characters"));
        }
        // The text is the step output: the UI renders it as a collapsed reasoning card, and the
        // model can read its own scratchpad back later. The one-word acknowledgement is the
        // summary, so the transcript is not padded with it.
        ToolResult::ok("noted", thought.to_string())
    }
}

pub struct TodoWrite;
impl Tool for TodoWrite {
    fn name(&self) -> &'static str { "todo_write" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/todo_write.json") }
    fn side_effecting(&self) -> bool { false }
    fn summary(&self, args: &Value) -> String {
        let Ok(items) = normalize(args) else { return "plan rejected".into() };
        let done = items.iter().filter(|(_, status)| status == "done").count();
        match items.iter().find(|(_, status)| status == "in_progress") {
            Some((text, _)) => format!("plan {done}/{} · {}", items.len(), truncate_chars(text, CURRENT_MAX)),
            None => format!("plan {done}/{} done", items.len()),
        }
    }
    fn run(&self, _ctx: &ToolCtx, args: Value) -> ToolResult {
        let items = match normalize(&args) { Ok(items) => items, Err(refusal) => return refusal };
        let listing: Vec<Value> = items.iter().enumerate()
            .map(|(i, (text, status))| json!({ "seq": i + 1, "text": text, "status": status }))
            .collect();
        ToolResult::ok(self.summary(&args), json!({ "items": listing }).to_string())
            .with_artifact(Artifact::Plan { items })
    }
}

/// Trim, default an absent status to `pending`, and enforce the same limits the 003 CHECKs do,
/// so a bad plan is a tool error the model can fix rather than a failed transaction.
fn normalize(args: &Value) -> Result<Vec<(String, String)>, ToolResult> {
    let invalid = |detail: String| ToolResult::err("invalid_arguments", detail);
    let Some(raw) = args.get("items").and_then(Value::as_array) else {
        return Err(invalid("items must be an array of {text, status} objects".into()));
    };
    if raw.len() > MAX_PLAN_ITEMS {
        return Err(invalid(format!("a plan holds at most {MAX_PLAN_ITEMS} items, got {}", raw.len())));
    }
    let mut items = Vec::with_capacity(raw.len());
    for (i, item) in raw.iter().enumerate() {
        let seq = i + 1;
        let text = item.get("text").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if text.is_empty() { return Err(invalid(format!("item {seq} needs text"))); }
        if text.chars().count() > MAX_PLAN_TEXT {
            return Err(invalid(format!("item {seq} is longer than {MAX_PLAN_TEXT} characters")));
        }
        let status = item.get("status").and_then(Value::as_str).unwrap_or("pending");
        if !PLAN_STATUSES.contains(&status) {
            return Err(invalid(format!("item {seq} has status {status:?}; expected one of {}", PLAN_STATUSES.join(", "))));
        }
        items.push((text.to_string(), status.to_string()));
    }
    if items.iter().filter(|(_, status)| status == "in_progress").count() > 1 {
        return Err(invalid("only one item may be in_progress".into()));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Registry, ToolStatus};

    fn ctx() -> ToolCtx {
        ToolCtx { root: std::env::temp_dir(), scope: "global".into(), request_id: "request".into(), step_id: "step".into(), diagnostics_cmd: None }
    }

    #[test]
    fn think_records_its_text_and_leaves_the_plan_alone() {
        let ctx = ctx();
        let r = Think.run(&ctx, json!({ "thought": "  two problems: the anchor drifts, and grep is slow  " }));
        assert_eq!(r.status, ToolStatus::Complete);
        assert_eq!(r.summary, "noted");
        assert_eq!(r.content, "two problems: the anchor drifts, and grep is slow");
        assert!(r.artifacts.is_empty(), "think must not touch the plan");
        assert!(!Think.side_effecting(), "a scratchpad must never wait for an approval");
        assert_eq!(Think.run(&ctx, json!({ "thought": "   " })).error_code, Some("invalid_arguments"));
        assert_eq!(Think.run(&ctx, json!({})).error_code, Some("invalid_arguments"));
        assert_eq!(Think.run(&ctx, json!({ "thought": "x".repeat(THOUGHT_MAX + 1) })).error_code, Some("too_large"));
    }

    #[test]
    fn todo_write_normalizes_and_hands_the_plan_to_the_loop() {
        let r = TodoWrite.run(&ctx(), json!({ "items": [
            { "text": "  read the failing test  ", "status": "done" },
            { "text": "fix the anchor", "status": "in_progress" },
            { "text": "run the gate" },
        ]}));
        assert_eq!(r.status, ToolStatus::Complete);
        assert_eq!(r.summary, "plan 1/3 · fix the anchor");
        let body: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(body["items"][0]["text"], "read the failing test", "text is trimmed");
        assert_eq!(body["items"][2]["status"], "pending", "an absent status defaults to pending");
        assert_eq!(body["items"][2]["seq"], 3, "seq is the position, so the UI can render the order");
        match r.artifacts.as_slice() {
            [Artifact::Plan { items }] => assert_eq!(items.len(), 3),
            other => panic!("expected exactly one plan artifact, got {other:?}"),
        }
        let empty = TodoWrite.run(&ctx(), json!({ "items": [] }));
        assert_eq!(empty.summary, "plan 0/0 done", "clearing the plan is legal");
    }

    #[test]
    fn todo_write_refuses_a_plan_it_could_not_store() {
        let long = "x".repeat(MAX_PLAN_TEXT + 1);
        let too_many: Vec<Value> = (0..=MAX_PLAN_ITEMS).map(|i| json!({ "text": format!("step {i}"), "status": "pending" })).collect();
        for args in [
            json!({}),
            json!({ "items": "not an array" }),
            json!({ "items": too_many }),
            json!({ "items": [{ "text": "  " }] }),
            json!({ "items": [{ "text": long }] }),
            json!({ "items": [{ "text": "a", "status": "blocked" }] }),
            json!({ "items": [{ "text": "a", "status": "in_progress" }, { "text": "b", "status": "in_progress" }] }),
        ] {
            let r = TodoWrite.run(&ctx(), args.clone());
            assert_eq!(r.error_code, Some("invalid_arguments"), "{args}");
            assert!(r.artifacts.is_empty(), "a refused plan must not be handed to the loop");
        }
    }

    #[test]
    fn the_registry_now_carries_a_schema_for_every_tool_including_the_plan_skill_and_task_tools() {
        let registry = Registry::standard();
        let schemas = registry.schemas().expect("every schema file parses");
        let names: Vec<String> = schemas.iter().map(|s| s["function"]["name"].as_str().unwrap_or_default().to_string()).collect();
        assert_eq!(names, ["read", "grep", "glob", "edit", "write", "ast_edit", "lsp", "bash", "think", "todo_write", "skill", "task"]);
    }
}
